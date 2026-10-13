use super::*;
use viptv_engine::{
    capabilities::Capability,
    presentation::RowState,
    stremio::{self, SubtitleCandidate, SubtitleExtras, SubtitleProvider, SubtitleSnapshot},
};
mod selections;
use selections::Selection;
#[cfg(test)]
mod tests;
pub(crate) use selections::Selections;

pub(crate) async fn discover(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path((kind, id)): Path<(String, String)>,
    Query(extras): Query<SubtitleExtras>,
) -> ApiResult {
    if id.is_empty()
        || id.len() > 512
        || id.chars().any(char::is_control)
        || !["movie", "series"].contains(&kind.as_str())
    {
        return Err("Invalid subtitle request".into());
    }
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
        kids::require_item(&db, &app.identity(), &kind, &id)?;
    }
    let mapped = crate::simkl::resolve_id(&app, &kind, &id).await?;
    let lookup_id = mapped.as_ref().map(|m| m.target.id.as_str()).unwrap_or(&id);
    let addon_revision = app.addons.revision()?;
    let results = app.addons.subtitles(&kind, lookup_id, &extras).await?;
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
                    scope: scope.clone(),
                    lease: lease.clone(),
                    addon_revision: addon_revision.clone(),
                    kind: kind.clone(),
                    media_id: id.clone(),
                    url: file.url,
                    created: Instant::now(),
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
    kids::require_item(&db, &app.identity(), &selection.kind, &selection.media_id)
}

pub(crate) async fn content(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
) -> Result<axum::response::Response, ApiError> {
    let app = app.with_lease(lease.clone());
    let selected = app.subtitle_selections.get(&id, &lease)?;
    validate(&app, &selected)?;
    let url = url::Url::parse(&selected.url).map_err(|_| ApiError::from("Subtitle unavailable"))?;
    let response = tokio::time::timeout(
        Duration::from_secs(10),
        crate::source_http::bytes_with_content_type(
            url,
            2 * 1024 * 1024,
            Duration::from_secs(10),
            app.addons.fixture_transport(),
            true,
        ),
    )
    .await
    .map_err(|_| {
        ApiError(
            StatusCode::GATEWAY_TIMEOUT,
            "Subtitle fetch timed out".into(),
        )
    })?
    .map_err(|_| ApiError(StatusCode::BAD_GATEWAY, "Subtitle file unavailable".into()))?;
    let vtt = stremio::subtitle_bytes_vtt(&response.body, response.content_type.as_deref())
        .map_err(|_| {
            ApiError(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Subtitle encoding or format is unsupported".into(),
            )
        })?;
    validate(&app, &selected)?;
    Ok((
        [
            (header::CONTENT_TYPE, "text/vtt; charset=utf-8"),
            (header::CACHE_CONTROL, "private, no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        vtt,
    )
        .into_response())
}
