use super::*;
use viptv_engine::stremio::SourceCandidate;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Rank {
    ids: Vec<String>,
    #[serde(default)]
    quality_height: Option<u32>,
    #[serde(default)]
    device_max_height: Option<u32>,
}
pub(crate) async fn rank(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    axum::Json(input): axum::Json<Rank>,
) -> ApiResult {
    if input.ids.len() > crate::app_state::MAX_SOURCES_PER_JOB
        || input.ids.iter().any(|id| id.is_empty() || id.len() > 128)
        || input
            .device_max_height
            .is_some_and(|height| !(144..=4320).contains(&height))
    {
        return Err("Invalid source selection".into());
    }
    let app = app.with_lease(lease.clone());
    let preferences =
        super::preferences::get(State(app.clone()), Extension(lease.clone()), Path(profile))
            .await?
            .0;
    let preferences: viptv_engine::preferences::PreferencesSnapshot =
        serde_json::from_value(preferences)
            .map_err(|_| ApiError::from("Preferences unavailable"))?;
    let (configuration, connections) = configuration::read(&app, &lease, profile)?;
    let eligible = viptv_engine::registry::Registry::builtin()
        .connections_for(
            &configuration,
            &connections,
            viptv_engine::capabilities::Capability::Sources,
        )
        .map_err(|_| ApiError::from("Source configuration unavailable"))?;
    let allowed: std::collections::HashSet<_> = eligible.iter().map(|c| c.id.as_str()).collect();
    for id in &input.ids {
        app.check_resource(&app.identity(), "stream", id)?;
    }
    let wanted: std::collections::HashSet<_> = input.ids.iter().map(String::as_str).collect();
    let mut cards = std::collections::HashMap::new();
    {
        let jobs = app.jobs.lock().unwrap();
        for job in jobs.values() {
            let state = job.state.lock().unwrap();
            for event in &state.events {
                for card in event["streams"].as_array().into_iter().flatten() {
                    if let Some(id) = card["id"].as_str().filter(|id| wanted.contains(*id)) {
                        cards.insert(id.to_string(), card.clone());
                    }
                }
            }
        }
    }
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_profile(&db, profile)?;
    lease.validate(&db)?;
    let grants = app.streams.lock().unwrap();
    let mut candidates = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for id in input.ids {
        if !seen.insert(id.clone()) {
            continue;
        }
        let grant = grants.get(&id).ok_or_else(auth::forbidden)?;
        if grant.created.elapsed() >= crate::app_state::STREAM_GRANT_WINDOW
            || crate::sources::source_configuration(&db, &grant.producer)? != grant.configuration
        {
            return Err(auth::forbidden());
        }
        if grant.producer.starts_with("addon:")
            || grant.producer.starts_with("vsources:")
            || grant.producer.starts_with("m3u:")
        {
            if !allowed.contains(grant.producer.as_str()) {
                return Err(auth::forbidden());
            }
        } else if let Some(provider) = grant.provider_id {
            app.providers.require_owner(&db, provider)?;
        } else {
            return Err(auth::forbidden());
        }
        if let Some(media) = &grant.media_id {
            kids::require_item(&db, &lease.principal, &grant.kind, media)?;
        }
        let card = cards.get(&id).ok_or_else(auth::forbidden)?;
        candidates.push(
            SourceCandidate {
                presentation: viptv_engine::stremio::SourcePresentation::from_stream(card),
                id,
                provider_id: grant.producer.clone(),
                label: card["name"].as_str().unwrap_or("Stream").into(),
                detail: viptv_engine::stremio::source_detail(card),
                quality_height: viptv_engine::stremio::source_quality_height(card),
                audio_languages: card["audio_languages"]
                    .as_array()
                    .map(|languages| {
                        languages
                            .iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default(),
                delivery: card["delivery"]
                    .as_str()
                    .filter(|delivery| viptv_engine::stremio::DELIVERIES.contains(delivery))
                    .map(str::to_owned),
                ..Default::default()
            }
            .with_release_facts(viptv_engine::stremio::source_release_facts(card)),
        );
    }
    drop(grants);
    drop(db);
    let ranking = viptv_engine::preferences::rank_sources(
        &mut candidates,
        &preferences.preferences,
        None,
        input.device_max_height,
    );
    configuration::validate_revision(&app, profile, configuration.scope.revision)?;
    let ranked: Vec<_> = candidates
        .into_iter()
        .filter_map(|candidate| {
            let mut card = cards.remove(&candidate.id)?;
            let addon_label = eligible
                .iter()
                .find(|connection| connection.id == candidate.provider_id)
                .map(|connection| connection.name.clone());
            card["view"] = display(&candidate, addon_label.as_deref());
            Some(card)
        })
        .collect();
    let mut heights = std::collections::BTreeMap::<u32, usize>::new();
    for card in &ranked {
        if let Some(height) = viptv_engine::stremio::source_quality_height(card) {
            *heights.entry(height).or_default() += 1;
        }
    }
    let filters=heights.into_iter().rev().map(|(height,count)|json!({"height":height,"label":if height==2160{"4K".into()}else{format!("{height}p")},"count":count})).collect::<Vec<_>>();
    let best_match_ids = ranking.best_match_ids;
    let best_match_id = ranking.best_match_id;
    let total = ranked.len();
    let selection_note = if preferences.preferences.source_selection
        == viptv_engine::preferences::SourceSelection::Best
    {
        "Play starts the best match. Change how it’s chosen in Settings › Playback."
    } else {
        "Choose a source to start playback. Change your default in Settings › Playback."
    };
    let streams = ranked
        .into_iter()
        .filter(|card| {
            input.quality_height.is_none_or(|height| {
                viptv_engine::stremio::source_quality_height(card) == Some(height)
            })
        })
        .collect::<Vec<_>>();
    Ok(axum::Json(
        json!({"streams":streams,"best_match_id":best_match_id,"best_match_ids":best_match_ids,"quality_filters":filters,"total":total,"selection_note":selection_note}),
    ))
}

fn display(candidate: &SourceCandidate, addon: Option<&str>) -> Value {
    let presentation = candidate.presentation.as_ref();
    let quality = presentation.and_then(|p| p.quality.clone()).or_else(|| {
        candidate.quality_height.map(|height| {
            if height == 2160 {
                "4K".into()
            } else {
                format!("{height}p")
            }
        })
    });
    let format = candidate
        .dynamic_range
        .iter()
        .map(|value| match value.as_str() {
            "dv" => "Dolby Vision",
            "hdr10_plus" => "HDR10+",
            "hdr10" => "HDR10",
            "hdr" => "HDR",
            "hlg" => "HLG",
            _ => "",
        })
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    json!({"title":candidate.label,"body":candidate.detail,"quality_label":quality,"format_label":format,"addon_label":addon,"provider_label":presentation.and_then(|p|p.provider_name.as_deref())})
}
