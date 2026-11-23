use super::*;

const ADMISSIONS_PER_HOUR: u32 = 60;

pub(crate) fn limiter() -> rate::Limiter {
    rate::Limiter::new(ADMISSIONS_PER_HOUR, Duration::from_secs(3600))
}

async fn current(app: &App, lease: &ResourceLease, proof: &Proof) -> Result<(), Error> {
    let database = app.db.clone();
    let lease = lease.clone();
    let proof = proof.clone();
    tokio::task::spawn_blocking(move || {
        let db = database.lock().map_err(|_| Error::Code("provider_storage_unavailable"))?;
        lease.validate(&db)?;
        validate_source(&db, &proof)
    })
    .await
    .map_err(|_| Error::Code("provider_storage_unavailable"))?
}

fn runtime(request: &Start) -> bool {
    request
        .client
        .native_torrent
        .as_ref()
        .is_some_and(|cap| cap.version == 2)
}

async fn direct(app: &App, lease: &ResourceLease, source: &Source) -> Result<Value, Error> {
    let destination = app
        .direct_probe
        .resolve(&source.url, &source.headers, || current(app, lease, &source.proof))
        .await?
        .ok_or(Error::Code("download_unsupported"))?;
    if let Some(provider) = source.provider_id {
        drop(
            app.providers
                .acquire_playback_for_kind(provider, &source.proof.kind)
                .await
                .map_err(ApiError::from)?,
        );
    }
    current(app, lease, &source.proof).await?;
    let mut delivery = json!({"kind":"direct","url":destination.url,"headers":destination.headers,"format":"original","size":destination.size,"expires_at":null});
    if let Some(index) = source.required_audio_index {
        delivery["required_audio_index"] = json!(index);
    }
    Ok(delivery)
}

async fn torrent(app: &App, lease: &ResourceLease, source: &Source) -> Result<Value, Error> {
    let input = runtime_input(source).await?;
    current(app, lease, &source.proof).await?;
    runtime_scope_current(app, lease, source)?;
    let size = input.expected_file_size;
    let issued_at = util::now() as u64;
    let grant = input.grant(source.file_index, issued_at, issued_at + 60);
    let body = viptv_core::torrent_runtime::ready_response(
        &grant.id,
        0.0,
        &viptv_core::native_torrent::NativeTorrentPreferences {
            audio_language: None,
            subtitle_language: None,
            subtitles_enabled: false,
        },
        &grant,
    )
    .map_err(|_| Error::Code("native_metainfo_invalid"))?;
    let mut ready: Value =
        serde_json::from_str(&body).map_err(|_| Error::Code("native_metainfo_invalid"))?;
    Ok(json!({"kind":"native_torrent","format":"original","size":size,"expires_at":null,"grant":ready["delivery"]["grant"].take()}))
}

pub(super) async fn admit(
    app: App,
    lease: ResourceLease,
    request: Start,
) -> Result<(StatusCode, Json<Value>), Error> {
    if request.decoder_start
        || request.prepare
        || request.resume
        || request.replace_source.is_some()
        || request.position != 0.0
    {
        return Err(Error::Code("invalid_playback_request"));
    }
    let crate::auth::Principal::Account {
        account_id,
        profile_id: Some(profile_id),
        ..
    } = lease.principal
    else {
        return Err(ApiError(StatusCode::FORBIDDEN, crate::MSG_PROFILE_REQUIRED.into()).into());
    };
    if !app
        .download_admission_limits
        .admit(&format!("{account_id}:{profile_id}"), Instant::now())
    {
        return Err(Error::Code("download_rate_limited"));
    }
    let source = source(&app, &lease, request.stream_id.clone()).await?;
    if source.live {
        return Err(Error::Code("download_unsupported"));
    }
    let (kind, delivery) = if runtime(&request) && native_candidate(&app, &lease, &source, &request) {
        ("native_torrent", torrent(&app, &lease, &source).await?)
    } else if direct_eligible(&source, &request) && source.info_hash.is_none() {
        ("direct", direct(&app, &lease, &source).await?)
    } else {
        return Err(Error::Code("download_unsupported"));
    };
    tracing::info!(
        session_tag = %crate::gateway::diagnostics::tag(lease.session_id.as_deref().unwrap_or_default()),
        profile_tag = %crate::gateway::diagnostics::tag(&profile_id.to_string()),
        delivery = kind,
        size_known = delivery["size"].is_u64(),
        "Download admission accepted"
    );
    Ok((
        StatusCode::OK,
        Json(json!({"purpose":"download","status":"ready","delivery":delivery})),
    ))
}
