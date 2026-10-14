use super::*;
pub(crate) struct Caption {
    pub content_vtt: String,
    pub language: String,
    pub label: String,
}
pub(crate) async fn load(
    app: &App,
    lease: &ResourceLease,
    id: &str,
    expected: Option<(&str, &str)>,
) -> Result<Caption, ApiError> {
    let selected = app.subtitle_selections.get(id, lease)?;
    validate(app, &selected)?;
    if expected
        .is_some_and(|(kind, media_id)| selected.kind != kind || selected.media_id != media_id)
    {
        return Err(auth::forbidden());
    }
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
    validate(app, &selected)?;
    Ok(Caption {
        content_vtt: vtt,
        language: selected.language,
        label: selected.label,
    })
}
