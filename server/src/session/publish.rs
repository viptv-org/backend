use super::*;

pub(super) async fn publish(
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

// Finish the audit before publishing the new generation. Contention must not
// silently retain an earlier account selection, or block a Tokio worker thread.
pub(super) async fn record_family_startup(a: &App, channel: &str, attempts: &[Value]) {
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
