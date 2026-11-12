use super::*;
#[derive(Clone)]
pub(super) struct Binding {
    generation: String,
    subtitle_id: String,
    subtitle_hash: String,
    media_identity: Option<String>,
    audio_identity: Option<String>,
    revision: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Selection {
    subtitle_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Policy {
    revision: u64,
    manual: bool,
}
pub(crate) async fn start(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
    input: Result<Json<Selection>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, Error> {
    let Json(input) = input.map_err(|_| Error::Code("invalid_playback_request"))?;
    if !protocol::identifier(&input.subtitle_id) {
        return Err(Error::Code("invalid_playback_request"));
    }
    let entry = app.gateway_playbacks.snapshot(&id, &lease)?;
    let _gate = entry.subtitle_sync_gate.lock().await;
    validate_entry(&app, &entry).await?;
    let remote = {
        let state = entry.state.lock().unwrap();
        if state.status != "ready" || !state.retained_subtitles.contains(&input.subtitle_id) {
            return Err(Error::Code("invalid_playback_request"));
        }
        state
            .remote
            .clone()
            .ok_or(Error::Code("invalid_playback_request"))?
    };
    let media = entry
        .proof
        .media_id
        .as_deref()
        .ok_or_else(crate::auth::forbidden)?;
    let scoped = app.clone().with_lease(lease.clone());
    crate::engine_v3::subtitles::validate_playback_context(
        &scoped,
        &lease,
        &input.subtitle_id,
        entry.proof.metadata_proof.as_ref(),
    )?;
    let caption = crate::engine_v3::subtitles::load(
        &scoped,
        &lease,
        &input.subtitle_id,
        Some((&entry.proof.kind, media)),
    )
    .await?;
    validate_entry(&app, &entry).await?;
    let binding = Binding {
        generation: uuid::Uuid::new_v4().to_string(),
        subtitle_id: input.subtitle_id,
        subtitle_hash: format!("{:x}", Sha256::digest(caption.content_vtt.as_bytes())),
        media_identity: None,
        audio_identity: None,
        revision: 0,
    };
    {
        let mut state = entry.state.lock().unwrap();
        if state.status != "ready" || entry.cancelled.load(Ordering::Acquire) {
            return Err(Error::Code("playback_expired"));
        }
        state.subtitle_sync = Some(binding.clone());
    }
    let body = json!({"content_vtt":caption.content_vtt.as_ref(),"playback_generation":binding.generation});
    let value = remote
        .client
        .request(
            &remote.target.gateway.endpoint,
            remote.target.key.expose(),
            reqwest::Method::POST,
            &format!("v1/sessions/{}/subtitle-sync", remote.viewer),
            Some(&body),
            None,
            Duration::from_secs(8),
        )
        .await?;
    finish(&app, &lease, &entry, &binding, value)
        .await
        .map(Json)
}
pub(crate) async fn get(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Error> {
    forward(app, lease, id, reqwest::Method::GET, None).await
}
pub(crate) async fn policy(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
    input: Result<Json<Policy>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, Error> {
    let Json(input) = input.map_err(|_| Error::Code("invalid_playback_request"))?;
    forward(
        app,
        lease,
        id,
        reqwest::Method::PUT,
        Some(json!({"revision":input.revision,"manual":input.manual})),
    )
    .await
}
async fn forward(
    app: App,
    lease: ResourceLease,
    id: String,
    method: reqwest::Method,
    body: Option<Value>,
) -> Result<Json<Value>, Error> {
    let entry = app.gateway_playbacks.snapshot(&id, &lease)?;
    let _gate = entry.subtitle_sync_gate.lock().await;
    validate_entry(&app, &entry).await?;
    let (remote, binding) = {
        let state = entry.state.lock().unwrap();
        (
            state
                .remote
                .clone()
                .ok_or(Error::Code("invalid_playback_request"))?,
            state
                .subtitle_sync
                .clone()
                .ok_or(Error::Code("invalid_playback_request"))?,
        )
    };
    let scoped = app.clone().with_lease(lease.clone());
    crate::engine_v3::subtitles::validate_playback_context(
        &scoped,
        &lease,
        &binding.subtitle_id,
        entry.proof.metadata_proof.as_ref(),
    )?;
    if body
        .as_ref()
        .is_some_and(|b| b["revision"].as_u64() != Some(binding.revision))
    {
        return Err(Error::Code("invalid_playback_request"));
    }
    let value = remote
        .client
        .request(
            &remote.target.gateway.endpoint,
            remote.target.key.expose(),
            method,
            &format!("v1/sessions/{}/subtitle-sync", remote.viewer),
            body.as_ref(),
            None,
            Duration::from_secs(5),
        )
        .await?;
    finish(&app, &lease, &entry, &binding, value)
        .await
        .map(Json)
}
async fn finish(
    app: &App,
    lease: &ResourceLease,
    entry: &Arc<Entry>,
    expected: &Binding,
    value: Value,
) -> Result<Value, Error> {
    validate_entry(app, entry).await?;
    let scoped = app.clone().with_lease(lease.clone());
    crate::engine_v3::subtitles::validate_playback_context(
        &scoped,
        lease,
        &expected.subtitle_id,
        entry.proof.metadata_proof.as_ref(),
    )?;
    let mut state = entry.state.lock().unwrap();
    if state.status != "ready" || entry.cancelled.load(Ordering::Acquire) {
        return Err(Error::Code("playback_expired"));
    }
    let binding = state
        .subtitle_sync
        .as_mut()
        .filter(|b| b.generation == expected.generation)
        .ok_or(Error::Code("invalid_playback_request"))?;
    validate_record(binding, &value)?;
    binding.revision = value["revision"].as_u64().unwrap();
    binding.media_identity = Some(value["media_identity"].as_str().unwrap().into());
    binding.audio_identity = Some(value["audio_identity"].as_str().unwrap().into());
    Ok(value)
}
fn validate_record(binding: &Binding, value: &Value) -> Result<(), Error> {
    let valid = value["version"] == 1
        && value["playback_generation"] == binding.generation
        && value["subtitle_hash"] == binding.subtitle_hash
        && value["algorithm_version"] == "activity-offset-affine-v1"
        && value["revision"]
            .as_u64()
            .is_some_and(|r| r > 0 && r >= binding.revision)
        && value["manual"].is_boolean()
        && matches!(
            value["state"].as_str(),
            Some("observing" | "accepted" | "ambiguous" | "unavailable")
        )
        && ["media_identity", "audio_identity"].iter().all(|key| {
            value[*key]
                .as_str()
                .is_some_and(|s| s.len() <= 128 && protocol::identifier(s))
        })
        && binding
            .media_identity
            .as_ref()
            .is_none_or(|id| value["media_identity"] == *id)
        && binding
            .audio_identity
            .as_ref()
            .is_none_or(|id| value["audio_identity"] == *id);
    if !valid {
        return Err(Error::Code("gateway_protocol_invalid"));
    }
    if value["state"] == "accepted" {
        let c = &value["correction"];
        if !c["offsetUs"]
            .as_i64()
            .is_some_and(|v| (-30_000_000..=30_000_000).contains(&v))
            || c["anchorUs"].as_i64().is_none()
            || !c["scale"]
                .as_f64()
                .is_some_and(|s| s.is_finite() && (0.98..=1.02).contains(&s))
        {
            return Err(Error::Code("gateway_protocol_invalid"));
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn correction_records_reject_wrong_generation_track_identity_and_revision() {
        let binding = Binding {
            generation: "generation".into(),
            subtitle_id: "subtitle".into(),
            subtitle_hash: "hash".into(),
            media_identity: Some("session_media".into()),
            audio_identity: Some("audio".into()),
            revision: 3,
        };
        let value = json!({"version":1,"playback_generation":"generation","subtitle_hash":"hash","media_identity":"session_media","audio_identity":"audio","algorithm_version":"activity-offset-affine-v1","revision":4,"manual":false,"state":"accepted","correction":{"anchorUs":600000000,"offsetUs":2000000,"scale":1.001}});
        assert!(validate_record(&binding, &value).is_ok());
        for (key, replacement) in [
            ("playback_generation", json!("stale")),
            ("subtitle_hash", json!("other")),
            ("media_identity", json!("other")),
            ("audio_identity", json!("dub")),
            ("revision", json!(2)),
            ("manual", json!(null)),
        ] {
            let mut wrong = value.clone();
            wrong[key] = replacement;
            assert!(validate_record(&binding, &wrong).is_err(), "{key}");
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Stop {
    playback_generation: String,
}
pub(crate) async fn stop(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
    input: Result<Json<Stop>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, Error> {
    let Json(input) = input.map_err(|_| Error::Code("invalid_playback_request"))?;
    let entry = app.gateway_playbacks.snapshot(&id, &lease)?;
    let _gate = entry.subtitle_sync_gate.lock().await;
    validate_entry(&app, &entry).await?;
    let remote = {
        let mut state = entry.state.lock().unwrap();
        if state
            .subtitle_sync
            .as_ref()
            .is_none_or(|b| b.generation != input.playback_generation)
        {
            return Err(Error::Code("invalid_playback_request"));
        }
        state.subtitle_sync = None;
        state
            .remote
            .clone()
            .ok_or(Error::Code("invalid_playback_request"))?
    };
    remote
        .client
        .request(
            &remote.target.gateway.endpoint,
            remote.target.key.expose(),
            reqwest::Method::DELETE,
            &format!("v1/sessions/{}/subtitle-sync", remote.viewer),
            Some(&json!({"playback_generation":input.playback_generation})),
            None,
            Duration::from_secs(5),
        )
        .await?;
    validate_entry(&app, &entry).await?;
    Ok(Json(json!({"ok":true})))
}
