use super::*;
use viptv_engine::stremio::SourceCandidate;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Rank {
    ids: Vec<String>,
}
pub(crate) async fn rank(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    axum::Json(input): axum::Json<Rank>,
) -> ApiResult {
    if input.ids.len() > crate::app_state::MAX_SOURCES_PER_JOB || input.ids.iter().any(|id| id.is_empty() || id.len() > 128) {
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
        if grant.created.elapsed() >= Duration::from_secs(1800)
            || crate::sources::source_configuration(&db, &grant.producer)? != grant.configuration
        {
            return Err(auth::forbidden());
        }
        if grant.producer.starts_with("addon:") || grant.producer.starts_with("vsources:") || grant.producer.starts_with("m3u:") {
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
        candidates.push(SourceCandidate {
            presentation: viptv_engine::stremio::SourcePresentation::read(&card["presentation"]),
            id,
            provider_id: grant.producer.clone(),
            label: card["name"].as_str().unwrap_or("Stream").into(),
            detail: card["description"].as_str().map(str::to_string),
            quality_height: card["quality_height"]
                .as_u64()
                .and_then(|h| u32::try_from(h).ok()),
            audio_languages: card["audio_languages"]
                .as_array()
                .map(|languages| {
                    languages
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        });
    }
    drop(grants);
    drop(db);
    viptv_engine::preferences::rank_sources(&mut candidates, &preferences.preferences);
    configuration::validate_revision(&app, profile, configuration.scope.revision)?;
    Ok(axum::Json(
        json!({"streams":candidates.into_iter().filter_map(|candidate|cards.remove(&candidate.id)).collect::<Vec<_>>()}),
    ))
}
