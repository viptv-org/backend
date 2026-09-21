//! Owned playback request lifecycle. Channel/source resolution, provider admission,
//! authenticated media access and teardown stay together behind the existing routes.
//! The media engine owns input processes and reservations throughout preparation.
use super::*;
mod live;
mod publish;
pub(crate) mod shared;
mod start;
pub(super) use live::LiveSession;
use publish::{publish, record_family_startup};
pub(super) use start::start_playback;
use start::{prepare_family, start_playback_inner};

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
// One track selection for both the direct and family startup paths.
fn track_selection(v: &PlaybackRequest) -> playback::TrackSelection {
    playback::TrackSelection {
        audio_track_index: v.audio_track_index,
        audio_language: v.audio_language.clone(),
        subtitle_track_index: v.subtitle_track_index,
        preferred_audio_language: Some(v.preferences.audio_language.clone()),
        preferred_subtitle_language: (v.preferences.subtitles_enabled && !v.subtitles_off)
            .then(|| v.preferences.subtitle_language.clone()),
    }
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
        if lease.validate_media(&a).await.is_err() {
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
        lease.validate_media(&a).await?;
        // Progressive bodies can outlive the handler: validate the viewer lease
        // before yielding each bounded chunk, not only at response creation.
        // The lease cache bounds the database work to one validation per lease
        // per window; the in-memory owner registry still catches server-side
        // teardown on every chunk.
        let (parts, body) = response.into_parts();
        let stream = async_stream::try_stream! {
            use futures::StreamExt;
            let mut chunks=body.into_data_stream();
            while let Some(chunk)=chunks.next().await {
                if lease.validate_media(&a).await.is_err() || a.resource_lease("playback",&id).is_none() {
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
    if lease.validate_media(&a).await.is_err() {
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
