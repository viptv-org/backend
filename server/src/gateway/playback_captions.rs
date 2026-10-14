//! Attach only an already scoped subtitle selection to its exact playback viewer.
use super::*;
use crate::auth;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Attach {
    subtitle_id: String,
}
pub(crate) async fn attach(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
    input: Result<Json<Attach>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, Error> {
    let Json(input) = input.map_err(|_| Error::Code("invalid_playback_request"))?;
    if !protocol::identifier(&input.subtitle_id) {
        return Err(Error::Code("invalid_playback_request"));
    }
    let entry = app.gateway_playbacks.snapshot(&id, &lease)?;
    validate_entry(&app, &entry).await?;
    let remote = {
        let state = entry.state.lock().unwrap();
        if state.status != "ready"
            || state
                .delivery
                .as_ref()
                .is_none_or(|d| d["external_subtitles_supported"] != true)
        {
            return Err(ApiError(
                StatusCode::UNPROCESSABLE_ENTITY,
                "External subtitles are unavailable for this delivery".into(),
            )
            .into());
        }
        state.remote.clone().ok_or_else(|| {
            ApiError(
                StatusCode::UNPROCESSABLE_ENTITY,
                "External subtitles require a gateway viewer".into(),
            )
        })?
    };
    let media = entry
        .proof
        .media_id
        .as_deref()
        .ok_or_else(auth::forbidden)?;
    let scoped = app.clone().with_lease(lease.clone());
    let caption = crate::engine_v3::subtitles::load(
        &scoped,
        &lease,
        &input.subtitle_id,
        Some((&entry.proof.kind, media)),
    )
    .await?;
    validate_entry(&app, &entry).await?;
    let body = json!({"content_vtt":caption.content_vtt});
    let value = remote
        .client
        .request(
            &remote.target.gateway.endpoint,
            remote.target.key.expose(),
            reqwest::Method::POST,
            &format!("v1/sessions/{}/captions", remote.viewer),
            Some(&body),
            None,
            Duration::from_secs(8),
        )
        .await?;
    validate_entry(&app, &entry).await?;
    let relative = value["url"]
        .as_str()
        .ok_or(Error::Code("gateway_protocol_invalid"))?;
    let parts = relative.split('/').collect::<Vec<_>>();
    let valid = parts.len() == 5
        && parts[0].is_empty()
        && parts[1] == "media"
        && parts[2] == remote.viewer
        && protocol::identifier(parts[3])
        && parts[4]
            .strip_prefix("caption_")
            .and_then(|f| f.strip_suffix(".srt"))
            .is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()));
    let offset = value["timeline_offset_seconds"]
        .as_f64()
        .filter(|v| v.is_finite() && *v >= 0.0);
    let expected = entry
        .state
        .lock()
        .unwrap()
        .delivery
        .as_ref()
        .and_then(|d| d["timeline_offset_seconds"].as_f64())
        .unwrap_or(0.0);
    if !valid || value["format"] != "srt" || offset.is_none_or(|v| (v - expected).abs() > 0.001) {
        return Err(Error::Code("gateway_protocol_invalid"));
    }
    let url = super::super::client::endpoint(&remote.target.gateway.endpoint)?
        .join(relative.trim_start_matches('/'))
        .map_err(|_| Error::Code("gateway_protocol_invalid"))?;
    Ok(Json(
        json!({"url":url.as_str(),"format":"srt","language":caption.language,"label":caption.label,"timeline_offset_seconds":offset}),
    ))
}
