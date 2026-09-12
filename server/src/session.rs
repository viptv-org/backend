//! Owned playback request lifecycle. Channel/source resolution, provider admission,
//! authenticated media access and teardown stay together behind the existing routes.
//! The media engine owns input processes and reservations throughout preparation.
use super::*;
mod live;
pub(crate) mod shared;
pub(super) use live::LiveSession;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PlaybackRequest {
    stream_id: Option<String>,
    startup_id: Option<String>,
    channel_id: Option<String>,
    #[serde(default)]
    position: f64,
    capabilities: Option<playback::Capabilities>,
    #[serde(default)]
    force_transcode: bool,
    #[serde(default)]
    managed_only: bool,
    audio_track_index: Option<u32>,
    audio_language: Option<String>,
    subtitle_track_index: Option<u32>,
    #[serde(default)]
    subtitles_off: bool,
    #[serde(skip)]
    preferences: preferences::Preferences,
}
pub(super) async fn start_playback(
    State(a): State<App>,
    axum::Json(mut v): axum::Json<PlaybackRequest>,
) -> ApiResult {
    if v.audio_language.as_ref().is_some_and(|s| {
        s.is_empty() || s.len() > 16 || !s.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
    }) {
        return Err("Invalid audio language".into());
    }
    let prefs_app = a.clone();
    v.preferences = blocking(move || {
        let db = prefs_app.db.lock().unwrap();
        prefs_app.request_lease().validate(&db)?;
        let auth::Principal::Account { profile_id, .. } = prefs_app.identity();
        profile_id
            .map(|id| preferences::load(&db, id))
            .transpose()
            .map(Option::unwrap_or_default)
    })
    .await?;
    v.capabilities = v.preferences.cap(v.capabilities);
    if v.managed_only {
        if let Some(caps) = &mut v.capabilities {
            caps.direct_play = false;
        }
    }
    let effective_preferences = json!(v.preferences);
    let key = v
        .startup_id
        .as_deref()
        .map(|id| startup_key(&a, id))
        .transpose()?;
    let cancelled = if let Some(key) = &key {
        let mut requests = a.startup_requests.lock().unwrap();
        prune_startups(&mut requests);
        if requests.contains_key(key) {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "Playback startup cancelled or already used".into(),
            ));
        }
        if requests.len() >= 4096 {
            return Err(ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many playback requests".into(),
            ));
        }
        let request = StartupRequest::new();
        let cancelled = request.cancelled.clone();
        requests.insert(key.clone(), request);
        Some(cancelled)
    } else {
        None
    };
    let budget = if v.channel_id.is_some() {
        Duration::from_secs(45)
    } else {
        Duration::from_secs(70)
    };
    let preparation = shared::start(a.clone(), v);
    let result=tokio::time::timeout(budget,async {
        tokio::select! {
            result=preparation=>result,
            _=startup_cancelled(cancelled)=>Err(ApiError(StatusCode::CONFLICT,"Playback startup cancelled".into())),
            error=revoked(&a)=>Err(error),
        }
    }).await.unwrap_or_else(|_|Err(ApiError(StatusCode::GATEWAY_TIMEOUT,"Playback startup timed out. Try again later.".into())));
    if let (Some(key), Ok(response)) = (key, &result) {
        let cancelled = {
            let mut requests = a.startup_requests.lock().unwrap();
            if let Some(request) = requests.get_mut(&key) {
                request.session = response.0["id"].as_str().map(str::to_owned);
                request.cancelled.load(std::sync::atomic::Ordering::Acquire)
            } else {
                true
            }
        };
        if cancelled {
            if let Some(id) = response.0["id"].as_str() {
                stop_owned(&a, id).await;
                a.resource_owners
                    .lock()
                    .unwrap()
                    .remove(&format!("playback:{id}"));
            }
            return Err(ApiError(
                StatusCode::CONFLICT,
                "Playback startup cancelled".into(),
            ));
        }
    }
    result.map(|mut response| {
        response.0["preferences"] = effective_preferences;
        response
    })
}
async fn start_playback_inner(a: App, v: PlaybackRequest) -> ApiResult {
    a.prune_playback_owners().await;
    a.prune();
    let source_lease = v
        .stream_id
        .as_ref()
        .and_then(|id| a.resource_lease("stream", id));
    let worker = a.clone();
    let channel = v.channel_id.clone();
    let family = blocking(move || {
        let db = worker.db.lock().unwrap();
        worker.require_media(&db)?;
        channel
            .as_deref()
            .map(|id| lineup::family_id(&db, id).map_err(ApiError::from))
            .transpose()
            .map(Option::flatten)
    })
    .await?;
    if v.stream_id.is_none() {
        if let Some(family) = family {
            return start_family(a, &v, family).await;
        }
    }
    let (url, headers, live, provider_id, kind) = match (v.stream_id, v.channel_id) {
        (Some(id), None) => {
            a.check_resource(&a.identity(), "stream", &id)?;
            let streams = a.streams.lock().unwrap();
            let s = streams.get(&id).ok_or("Stream expired; discover again")?;
            (
                s.url.clone(),
                s.headers.clone(),
                s.live,
                s.provider_id,
                s.kind.clone(),
            )
        }
        (None, Some(id)) => {
            let (url, provider_id) = a.providers.blocking(move |p| p.channel_source(&id)).await?;
            (
                url,
                provider::egress::headers(&a.db.lock().unwrap(), provider_id)?,
                true,
                Some(provider_id),
                "live".to_owned(),
            )
        }
        _ => return Err("Provide exactly one stream_id or channel_id".into()),
    };
    let permit = if let Some(provider_id) = provider_id {
        Some(
            a.providers
                .acquire_playback_for_kind(provider_id, &kind)
                .await
                .map_err(|e| {
                    if e == "Provider connection limit reached" {
                        ApiError(StatusCode::TOO_MANY_REQUESTS, e)
                    } else {
                        ApiError::from(e)
                    }
                })?,
        )
    } else {
        None
    };
    let r = a
        .playback
        .start_with_selection(
            url,
            headers,
            v.position,
            v.capabilities,
            v.force_transcode,
            live,
            permit,
            playback::TrackSelection {
                audio_track_index: v.audio_track_index,
                audio_language: v.audio_language.clone(),
                subtitle_track_index: v.subtitle_track_index,
                preferred_audio_language: Some(v.preferences.audio_language.clone()),
                preferred_subtitle_language: (v.preferences.subtitles_enabled && !v.subtitles_off)
                    .then(|| v.preferences.subtitle_language.clone()),
            },
        )
        .await?;
    publish(a, r, source_lease, false).await
}
async fn publish(
    a: App,
    r: playback::PlaybackResponse,
    source_lease: Option<ResourceLease>,
    managed_live: bool,
) -> ApiResult {
    let mut unpublished = UnpublishedSession {
        playback: a.playback.clone(),
        id: r.id.clone(),
        armed: true,
    };
    if managed_live {
        a.playback.supervise_live(&r.id).await;
    }
    a.own_resource("playback", &r.id);
    let worker = a.clone();
    let authorized = blocking(move || {
        let db = worker.db.lock().unwrap();
        if let Some(audience) = &worker.playback_audience {
            return shared::authorize_audience(&worker, audience, &db);
        }
        worker.request_lease().validate(&db).and_then(|_| {
            source_lease
                .as_ref()
                .map_or(Ok(()), |lease| lease.validate(&db))
        })
    })
    .await;
    if let Err(error) = authorized {
        a.playback.stop(&r.id).await;
        a.resource_owners
            .lock()
            .unwrap()
            .remove(&format!("playback:{}", r.id));
        return Err(error);
    }
    unpublished.armed = false;
    Ok(axum::Json(
        serde_json::to_value(r).map_err(|_| "Response encoding failed")?,
    ))
}
pub(super) async fn heartbeat(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
) -> ApiResult {
    let a = a.with_lease(lease);
    if let Some(result) = shared::heartbeat(&a, &id, None).await {
        return result;
    }
    if let Some(result) = live::heartbeat(&a, &id, None).await {
        return result;
    }
    if !a.playback.heartbeat(&id).await {
        a.resource_owners
            .lock()
            .unwrap()
            .remove(&format!("playback:{id}"));
        return Err(ApiError(StatusCode::NOT_FOUND, "Session not found".into()));
    }
    Ok(axum::Json(json!({"ok":true})))
}
pub(super) async fn stop_playback(State(a): State<App>, Path(id): Path<String>) -> ApiResult {
    stop_owned(&a, &id).await;
    a.resource_owners
        .lock()
        .unwrap()
        .remove(&format!("playback:{id}"));
    Ok(axum::Json(json!({"ok":true})))
}
pub(super) async fn media(
    State(a): State<App>,
    Path((id, cap, file)): Path<(String, String, String)>,
    method: axum::http::Method,
    headers: axum::http::HeaderMap,
) -> Result<Response, ApiError> {
    let lease = a
        .resource_lease("playback", &id)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Media not found or expired".into()))?;
    {
        let authorization = lease.validate(&a.db.lock().unwrap());
        if authorization.is_err() {
            stop_owned(&a, &id).await;
            a.resource_owners
                .lock()
                .unwrap()
                .remove(&format!("playback:{id}"));
            return Err(ApiError(
                StatusCode::NOT_FOUND,
                "Media not found or expired".into(),
            ));
        }
    }
    let target = shared::original_target(&a, &id, &cap)
        .unwrap_or_else(|| Ok((id.clone(), cap.clone())))
        .map_err(|_| ApiError(StatusCode::NOT_FOUND, "Media expired".into()))?;
    if let Some(result) = a
        .playback
        .serve_original(&target.0, &target.1, &file, method, headers)
        .await
    {
        let response = result
            .map_err(|_| ApiError(StatusCode::BAD_GATEWAY, "Media origin unavailable".into()))?;
        lease.validate(&a.db.lock().unwrap())?;
        // Progressive bodies can outlive the handler: validate the viewer lease
        // before yielding each bounded chunk, not only at response creation.
        let (parts, body) = response.into_parts();
        let stream = async_stream::try_stream! {
            use futures::StreamExt;
            let mut chunks=body.into_data_stream();
            while let Some(chunk)=chunks.next().await {
                if lease.validate(&a.db.lock().unwrap()).is_err() || a.resource_lease("playback",&id).is_none() {
                    Err(std::io::Error::other("Media access revoked"))?;
                }
                yield chunk.map_err(|_|std::io::Error::other("Media delivery failed"))?;
            }
        };
        return Ok(Response::from_parts(
            parts,
            axum::body::Body::from_stream(futures::StreamExt::map(
                stream,
                |item: Result<axum::body::Bytes, std::io::Error>| item,
            )),
        ));
    }
    let served = if let Some(result) = shared::serve(&a, &id, &cap, &file).await {
        result
    } else {
        a.playback.serve(&id, &cap, &file).await
    };
    let (mime, bytes) =
        served.map_err(|_| ApiError(StatusCode::NOT_FOUND, "Media not found or expired".into()))?;
    // Serving may await disk/process I/O; never return buffered bytes after revocation.
    if lease.validate(&a.db.lock().unwrap()).is_err() {
        return Err(ApiError(
            StatusCode::NOT_FOUND,
            "Media not found or expired".into(),
        ));
    }
    Ok((
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, "no-store".into()),
        ],
        bytes,
    )
        .into_response())
}

struct FamilyInputs {
    current: Option<provider::selection::ObservationKey>,
    policy: lineup::RecoveryPolicy,
    attempted: std::collections::HashSet<String>,
}
async fn start_family(a: App, v: &PlaybackRequest, channel: String) -> ApiResult {
    let worker = a.clone();
    let policy = blocking(move || {
        let db = worker.db.lock().unwrap();
        worker.request_lease().validate(&db)?;
        lineup::recovery_policy(&db).map_err(db_error)
    })
    .await?;
    let mut inputs = FamilyInputs {
        current: None,
        policy,
        attempted: std::collections::HashSet::new(),
    };
    let response = tokio::time::timeout(
        Duration::from_secs(policy.deadline_seconds),
        prepare_family(a.clone(), v, channel.clone(), &mut inputs),
    )
    .await
    .map_err(|_| {
        ApiError(
            StatusCode::GATEWAY_TIMEOUT,
            "Playback startup timed out. Try again later.".into(),
        )
    })??;
    let mut result = publish(a.clone(), response, None, true).await?;
    result.0["candidate_id"] = json!(inputs.current.as_ref().map(|key| key.candidate.clone()));
    result.0["_source_key"] = json!(inputs.current.as_ref().map(|key| key.source.clone()));
    live::register(a, v.clone(), channel, inputs, &mut result.0);
    Ok(result)
}
async fn prepare_family(
    a: App,
    v: &PlaybackRequest,
    channel: String,
    inputs: &mut FamilyInputs,
) -> Result<playback::PlaybackResponse, ApiError> {
    if v.position != 0.0 {
        return Err("Live playback does not support offset seeking".into());
    }
    let caps = v.capabilities.clone().unwrap_or_default();
    if !caps.h264 || !caps.aac || caps.max_width < 2 || caps.max_height < 2 {
        return Err("H264/AAC playback support and valid dimensions are required".into());
    }
    let device_settings = json!({"capabilities":caps,"force_transcode":v.force_transcode,"audio":v.audio_track_index,"audio_language":v.audio_language,"subtitle":v.subtitle_track_index,"subtitles_off":v.subtitles_off,"preferences":v.preferences}).to_string();
    let mut excluded = inputs.attempted.clone();
    let deadline = Instant::now() + Duration::from_secs(inputs.policy.deadline_seconds);
    let mut attempts = Vec::new();
    let mut failures = 0;
    let mut busy = 0;
    loop {
        if Instant::now() >= deadline || failures >= 5 {
            break;
        }
        validate_request(&a).await?;
        let requested = channel.clone();
        let omitted = excluded.clone();
        let device = device_settings.clone();
        let selection = a
            .providers
            .blocking(move |p| p.reserve_family(&requested, &omitted, &device))
            .await?;
        for skipped in selection.skipped {
            if skipped["reason"] == "connections_busy" {
                busy += 1;
            }
            if !attempts.iter().any(|previous: &Value| {
                previous["candidate_id"] == skipped["candidate_id"]
                    && previous["reason"] == skipped["reason"]
            }) {
                attempts.push(skipped);
            }
        }
        let Some(selected) = selection.reservation else {
            break;
        };
        let provider::selection::Reservation {
            candidate,
            url,
            permit,
            explanation,
            observation,
        } = selected;
        excluded.insert(candidate.clone());
        let started = Instant::now();
        validate_request(&a).await?;
        inputs.attempted.insert(candidate.clone());
        let headers = provider::egress::candidate_headers(&a.db.lock().unwrap(), &candidate)?;
        let preparation = a.playback.start_with_selection(
            url,
            headers,
            0.0,
            v.capabilities.clone(),
            v.force_transcode,
            true,
            Some(permit),
            playback::TrackSelection {
                audio_track_index: v.audio_track_index,
                audio_language: v.audio_language.clone(),
                subtitle_track_index: v.subtitle_track_index,
                preferred_audio_language: Some(v.preferences.audio_language.clone()),
                preferred_subtitle_language: (v.preferences.subtitles_enabled && !v.subtitles_off)
                    .then(|| v.preferences.subtitle_language.clone()),
            },
        );
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_secs(inputs.policy.attempt_seconds));
        let result = tokio::select! {
            result=tokio::time::timeout(remaining,preparation)=>result,
            error=revoked(&a)=>return Err(error),
        };
        match result {
            Ok(Ok(response)) => {
                let _ = a.providers.record_family_input(
                    &observation,
                    true,
                    started.elapsed().as_millis() as u64,
                );
                inputs.current = Some(observation);
                attempts.push(explanation);
                record_family_startup(&a, &channel, &attempts).await;
                return Ok(response);
            }
            Ok(Err(error)) => {
                let reason = if error == "Playback capacity reached" {
                    busy += 1;
                    "connections_busy"
                } else {
                    failures += 1;
                    let _ = a.providers.record_family_input(
                        &observation,
                        false,
                        started.elapsed().as_millis() as u64,
                    );
                    "startup_failed"
                };
                attempts.push(json!({"candidate_id":candidate,"reason":reason}));
            }
            Err(_) => {
                failures += 1;
                let _ = a.providers.record_family_input(
                    &observation,
                    false,
                    started.elapsed().as_millis() as u64,
                );
                attempts.push(json!({"candidate_id":candidate,"reason":"startup_timed_out"}));
            }
        }
        // Never begin replacement while cancelled process cleanup is unconfirmed.
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_secs(2));
        if tokio::time::timeout(remaining, a.playback.settle_cancelled_inputs())
            .await
            .is_err()
        {
            break;
        }
    }
    validate_request(&a).await?;
    record_family_startup(&a, &channel, &attempts).await;
    if busy > 0 && failures == 0 {
        Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "All available connections are busy. Try this channel again shortly.".into(),
        ))
    } else {
        Err(ApiError(
            StatusCode::BAD_GATEWAY,
            "No working stream is available for this channel. Try again later.".into(),
        ))
    }
}
async fn validate_request(a: &App) -> Result<(), ApiError> {
    let worker = a.clone();
    blocking(move || {
        let db = worker.db.lock().unwrap();
        if let Some(audience) = &worker.playback_audience {
            shared::authorize_audience(&worker, audience, &db)
        } else {
            worker.request_lease().validate(&db)
        }
    })
    .await
}
async fn revoked(a: &App) -> ApiError {
    loop {
        tokio::time::sleep(Duration::from_millis(250)).await;
        if let Err(error) = validate_request(a).await {
            return error;
        }
    }
}

pub(super) struct StartupRequest {
    cancelled: Arc<std::sync::atomic::AtomicBool>,
    created: Instant,
    session: Option<String>,
}
impl StartupRequest {
    fn new() -> Self {
        Self {
            cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            created: Instant::now(),
            session: None,
        }
    }
}
fn prune_startups(requests: &mut HashMap<String, StartupRequest>) {
    requests.retain(|_, request| request.created.elapsed() < Duration::from_secs(180));
}
fn startup_key(a: &App, id: &str) -> Result<String, ApiError> {
    if id.is_empty()
        || id.len() > 100
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err("Invalid startup identifier".into());
    }
    Ok(format!(
        "{:?}:{}:{id}",
        a.request_lease().session_id,
        App::scoped_key(&a.identity())
    ))
}
async fn startup_cancelled(cancelled: Option<Arc<std::sync::atomic::AtomicBool>>) {
    let Some(cancelled) = cancelled else {
        std::future::pending::<()>().await;
        return;
    };
    while !cancelled.load(std::sync::atomic::Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
pub(super) async fn cancel_startup(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
) -> ApiResult {
    let a = a.with_lease(lease);
    let key = startup_key(&a, &id)?;
    let session = {
        let mut requests = a.startup_requests.lock().unwrap();
        prune_startups(&mut requests);
        if !requests.contains_key(&key) && requests.len() >= 4096 {
            return Err(ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many playback requests".into(),
            ));
        }
        let request = requests.entry(key).or_insert_with(StartupRequest::new);
        request
            .cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        request.session.clone()
    };
    if let Some(id) = session {
        stop_owned(&a, &id).await;
        a.resource_owners
            .lock()
            .unwrap()
            .remove(&format!("playback:{id}"));
    }
    Ok(axum::Json(json!({"ok":true})))
}

struct UnpublishedSession {
    playback: Arc<playback::PlaybackManager>,
    id: String,
    armed: bool,
}
impl Drop for UnpublishedSession {
    fn drop(&mut self) {
        if self.armed {
            let playback = self.playback.clone();
            let id = self.id.clone();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    playback.stop(&id).await;
                });
            }
        }
    }
}

pub(super) async fn recover_live(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    let a = a.with_lease(lease);
    let generation = body["generation"]
        .as_u64()
        .ok_or("Recovery requires the current generation")?;
    if let Some(result) = shared::heartbeat(&a, &id, Some(generation)).await {
        return result;
    }
    live::heartbeat(&a, &id, Some(generation))
        .await
        .unwrap_or_else(|| {
            Err(ApiError(
                StatusCode::NOT_FOUND,
                "Managed live session not found".into(),
            ))
        })
}
pub(super) async fn stop_owned(a: &App, id: &str) {
    if shared::stop(a, id).await {
        return;
    }
    if !live::stop(a, id).await {
        a.playback.stop(id).await;
    }
}

// Finish the audit before publishing the new generation. Contention must not
// silently retain an earlier account selection, or block a Tokio worker thread.
async fn record_family_startup(a: &App, channel: &str, attempts: &[Value]) {
    let db = a.db.clone();
    let channel = channel.to_owned();
    let attempts = attempts.to_vec();
    let _ = tokio::task::spawn_blocking(move || {
        if let Ok(db) = db.lock() {
            let _ = lineup::record_startup(&db, &channel, &attempts);
        }
    })
    .await;
}
