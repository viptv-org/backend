use super::*;
use viptv_engine::{
    capabilities::Capability,
    presentation::RowState,
    stremio::{self, SubtitleCandidate, SubtitleExtras, SubtitleProvider, SubtitleSnapshot},
};
mod content_fetch;
mod selections;
pub(crate) use content_fetch::load;
use selections::Selection;
#[cfg(test)]
mod tests;
pub(crate) use selections::Selections;

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubtitleQuery {
    #[serde(rename = "videoHash")]
    video_hash: Option<String>,
    #[serde(rename = "videoSize")]
    video_size: Option<u64>,
    filename: Option<String>,
    metadata_provider_id: Option<String>,
    metadata_addon_id: Option<i64>,
    metadata_origin: Option<String>,
    card: Option<String>,
}

pub(crate) async fn discover(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path((kind, id)): Path<(String, String)>,
    Query(query): Query<SubtitleQuery>,
) -> ApiResult {
    if id.is_empty()
        || id.len() > 512
        || id.chars().any(char::is_control)
        || !["movie", "series"].contains(&kind.as_str())
    {
        return Err("Invalid subtitle request".into());
    }
    let extras = SubtitleExtras {
        video_hash: query.video_hash,
        video_size: query.video_size,
        filename: query.filename,
    };
    extras
        .validate()
        .map_err(|_| ApiError::from("Invalid subtitle file information"))?;
    let auth::Principal::Account { profile_id, .. } = &lease.principal;
    let profile = profile_id.ok_or_else(auth::forbidden)?;
    let (app, revision) = configuration::apply(
        app.with_lease(lease.clone()),
        &lease,
        profile,
        Capability::Subtitles,
    )?;
    {
        let db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        app.require_profile(&db, profile)?;
        if query.metadata_origin.is_none() {
            kids::require_item(&db, &app.identity(), &kind, &id)?;
        }
    }
    let wire_origin = super::origins::parse_origin(query.metadata_origin.as_deref())?;
    let expected = super::origins::parse_card(query.card.as_deref())?;
    if wire_origin.is_none()
        && expected
            .as_ref()
            .is_some_and(|card| card.metadata_origin.is_some())
    {
        return Err("Metadata origin is required".into());
    }
    let (provider, addon) = super::origins::selectors(
        &app,
        query.metadata_provider_id.as_deref(),
        query.metadata_addon_id,
        wire_origin.as_ref(),
    )?;
    let origin = super::search::request_origin(&app, provider.as_deref(), addon)?;
    let metadata_proof = if let Some(wire) = &wire_origin {
        let expected = expected
            .as_ref()
            .ok_or("A scoped title selection is required")?;
        if expected.identity.kind != kind
            || expected.identity.id != id
            || expected.metadata_origin.as_ref() != Some(wire)
        {
            return Err("Conflicting metadata expectation".into());
        }
        Some(super::origins::MetadataProof::verified(&app, expected).await?)
    } else {
        None
    };
    let metadata_provider = origin
        .as_ref()
        .map(|(provider, _, _)| provider.clone())
        .unwrap_or(
            configuration::read(&app, &lease, profile)?
                .0
                .browsing_plugin,
        );
    if metadata_proof.is_none() {
        kids::consumers::require(&app, &kind, &id, &metadata_provider, true)?;
    }
    let mapped = if origin
        .as_ref()
        .is_some_and(|(provider, _, _)| provider == "stremio")
    {
        None
    } else {
        crate::simkl::resolve_id(&app, &kind, &id).await?
    };
    let lookup_id = mapped.as_ref().map(|m| m.target.id.as_str()).unwrap_or(&id);
    let addon_revision = app.addons.revision()?;
    let subtitle_addons = if let Some(proof) = &metadata_proof {
        if !viptv_engine::origin::shared_identity(&proof.card.identity) {
            let id = proof
                .card
                .metadata_origin
                .as_ref()
                .and_then(|o| o.connection_id.as_deref())
                .and_then(|id| id.strip_prefix("addon:"))
                .and_then(|id| id.parse::<i64>().ok());
            app.addons.clone().with_engine_ids(id.into_iter().collect())
        } else {
            app.addons.clone()
        }
    } else {
        app.addons.clone()
    };
    let results = subtitle_addons.subtitles(&kind, lookup_id, &extras).await?;
    configuration::validate_revision(&app, profile, revision)?;
    if app.addons.revision()? != addon_revision {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Addon configuration changed".into(),
        ));
    }
    let scope = scope(&lease, profile, revision);
    let mut snapshot = SubtitleSnapshot {
        scope: scope.clone(),
        candidates: vec![],
        providers: vec![],
        complete: true,
    };
    let mut selections = Vec::new();
    for provider in results {
        let state = match &provider.files {
            None => RowState::Failed,
            Some(files) if files.is_empty() => RowState::Empty,
            _ => RowState::Ready,
        };
        snapshot.providers.push(SubtitleProvider {
            provider_id: provider.provider_id.clone(),
            message: (state == RowState::Failed)
                .then(|| "Subtitles from this addon are unavailable. Try again.".into()),
            state,
        });
        for file in provider
            .files
            .unwrap_or_default()
            .into_iter()
            .take(80 - snapshot.candidates.len())
        {
            let selection_id = uuid::Uuid::new_v4().to_string();
            snapshot.candidates.push(SubtitleCandidate {
                id: selection_id.clone(),
                provider_id: provider.provider_id.clone(),
                language: file.language.clone(),
                label: format!("{} · {}", file.language, provider.name),
            });
            selections.push((
                selection_id,
                Selection {
                    metadata_proof: metadata_proof.clone(),
                    scope: scope.clone(),
                    lease: lease.clone(),
                    addon_revision: addon_revision.clone(),
                    kind: kind.clone(),
                    media_id: id.clone(),
                    url: file.url,
                    language: file.language.clone(),
                    label: format!("{} · {}", file.language, provider.name),
                    created: Instant::now(),
                    content_vtt: None,
                    retained_until: None,
                },
            ));
        }
    }
    lease.validate(
        &*app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?,
    )?;
    app.subtitle_selections.insert(&lease, selections)?;
    Ok(axum::Json(
        serde_json::to_value(snapshot).map_err(|_| ApiError::from("Subtitles unavailable"))?,
    ))
}

fn validate(app: &App, selection: &Selection) -> Result<(), ApiError> {
    let profile = selection
        .scope
        .profile_id
        .parse()
        .map_err(|_| auth::forbidden())?;
    configuration::validate_revision(app, profile, selection.scope.revision)?;
    if app.addons.revision()? != selection.addon_revision {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Addon configuration changed".into(),
        ));
    }
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    selection.lease.validate(&db)?;
    app.require_profile(&db, profile)?;
    if let Some(proof) = &selection.metadata_proof {
        proof.validate(&db, &selection.lease)
    } else {
        kids::require_item(&db, &app.identity(), &selection.kind, &selection.media_id)
    }
}

pub(crate) async fn content(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
) -> Result<axum::response::Response, ApiError> {
    let caption = load(&app.with_lease(lease.clone()), &lease, &id, None).await?;
    Ok((
        [
            (header::CONTENT_TYPE, "text/vtt; charset=utf-8"),
            (header::CACHE_CONTROL, "private, no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        caption.content_vtt.to_string(),
    )
        .into_response())
}

/// Raw media IDs alone do not identify a modern private playback context.
pub(crate) fn validate_playback_context(
    app: &App,
    lease: &ResourceLease,
    id: &str,
    expected: Option<&super::origins::MetadataProof>,
) -> Result<(), ApiError> {
    let selected = app.subtitle_selections.get(id, lease)?;
    validate(app, &selected)?;
    match (expected, selected.metadata_proof.as_ref()) {
        (None, None) => Ok(()),
        (Some(expected), Some(actual))
            if viptv_engine::origin::metadata_assertions_match(&expected.card, &actual.card)
                && viptv_engine::origin::metadata_assertions_match(
                    &actual.card,
                    &expected.card,
                ) =>
        {
            Ok(())
        }
        _ => Err(auth::forbidden()),
    }
}
