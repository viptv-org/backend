//! Viewer leases are independent of shared input ownership. Only this module
//! translates a viewer's private capability into the current worker generation.
use super::*;
use tokio::sync::watch;
mod identity;
#[cfg(test)]
use identity::identity;
pub(super) use identity::start;

#[derive(Default)]
pub(crate) struct Registry {
    groups: Mutex<HashMap<String, Arc<Group>>>,
    viewers: Mutex<HashMap<String, Arc<Group>>>,
    workers: Mutex<HashMap<String, std::sync::Weak<Group>>>,
    audiences: Mutex<HashMap<String, std::sync::Weak<Group>>>,
}
pub(crate) struct Group {
    audience: String,
    state: Mutex<GroupState>,
    notify: tokio::sync::Notify,
    // Retained completion signal: a `stop` that starts after the worker loop
    // already ended still observes the finished state instead of racing a
    // one-shot notification.
    finish: watch::Sender<bool>,
    finished: watch::Receiver<bool>,
}
struct GroupState {
    viewers: HashMap<String, Viewer>,
    response: Option<Value>,
    error: Option<(StatusCode, String)>,
}
struct Viewer {
    lease: ResourceLease,
    capability: String,
    touched: Instant,
    options: PlaybackRequest,
}
struct Pending {
    registry: Arc<Registry>,
    id: String,
    armed: bool,
}
impl Drop for Pending {
    fn drop(&mut self) {
        if self.armed {
            self.registry.detach(&self.id);
        }
    }
}
impl Registry {
    fn detach(&self, id: &str) -> bool {
        let group = self.viewers.lock().unwrap().remove(id);
        if let Some(group) = group {
            group.state.lock().unwrap().viewers.remove(id);
            group.notify.notify_one();
            true
        } else {
            false
        }
    }
    pub(crate) fn ids(&self) -> Vec<String> {
        self.viewers.lock().unwrap().keys().cloned().collect()
    }
    fn group(&self, id: &str) -> Option<Arc<Group>> {
        self.viewers.lock().unwrap().get(id).cloned()
    }
}
async fn active_lease(a: &App, g: &Group) -> Option<ResourceLease> {
    let ttl = a.playback.session_ttl();
    let leases = {
        let state = g.state.lock().unwrap();
        state
            .viewers
            .values()
            .filter(|v| v.touched.elapsed() < ttl)
            .map(|v| v.lease.clone())
            .collect::<Vec<_>>()
    };
    if leases.is_empty() {
        return None;
    }
    // Validation touches SQLite; keep it off the async workers.
    let worker = a.clone();
    blocking(move || {
        let db = worker.db.lock().unwrap();
        Ok(leases.into_iter().find(|lease| lease.validate(&db).is_ok()))
    })
    .await
    .ok()
    .flatten()
}
pub(super) async fn worker_access(a: &App, id: &str) -> Option<App> {
    let group = a
        .shared_playback
        .workers
        .lock()
        .unwrap()
        .get(id)
        .and_then(std::sync::Weak::upgrade)?;
    active_lease(a, &group)
        .await
        .map(|lease| a.clone().with_lease(lease))
}
pub(super) fn worker_known(a: &App, id: &str) -> bool {
    a.shared_playback.workers.lock().unwrap().contains_key(id)
}

fn render(group: &Group, id: &str, mut response: Value) -> Result<Value, ApiError> {
    let state = group.state.lock().unwrap();
    let viewer = state.viewers.get(id).ok_or("Viewer session ended")?;
    let file = response["url"]
        .as_str()
        .and_then(|u| u.rsplit('/').next())
        .ok_or("Playback output unavailable")?;
    response["url"] = json!(format!(
        "/media/{id}/{}-{}/{file}",
        viewer.capability,
        response["generation"].as_u64().unwrap_or(1)
    ));
    response["id"] = json!(id);
    response["shared_viewers"] = json!(state.viewers.len());
    response.as_object_mut().unwrap().remove("_source_key");
    Ok(response)
}
async fn no_viewers(a: &App, g: &Arc<Group>) {
    loop {
        if active_lease(a, g).await.is_none() {
            return;
        }
        // A viewer detach signals the group's notify; the fallback tick also
        // detects leases that expired without an explicit stop.
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(250)) => {},
            _ = g.notify.notified() => {},
        }
    }
}
async fn run(mut a: App, g: Arc<Group>, key: String, v: PlaybackRequest) {
    a.shared_playback
        .audiences
        .lock()
        .unwrap()
        .insert(g.audience.clone(), Arc::downgrade(&g));
    a.playback_audience = Some(g.audience.clone());
    crate::health::preempt(&a).await;
    // Preparation belongs to the audience, not to its first HTTP request.
    let mut result = None;
    for _ in 0..3 {
        let Some(lease) = active_lease(&a, &g).await else {
            break;
        };
        let options = {
            let state = g.state.lock().unwrap();
            state
                .viewers
                .values()
                .find(|viewer| {
                    viewer.lease.session_id == lease.session_id
                        && App::scoped_key(&viewer.lease.principal)
                            == App::scoped_key(&lease.principal)
                })
                .map(|viewer| viewer.options.clone())
                .unwrap_or_else(|| v.clone())
        };
        let owner = a.clone().with_lease(lease.clone());
        let work = start_playback_inner(owner, options);
        let attempt = tokio::select! {result=tokio::time::timeout(Duration::from_secs(70),work)=>Some(result),_=no_viewers(&a,&g)=>None};
        let Some(attempt) = attempt else {
            break;
        };
        match attempt {
            Ok(Ok(response)) => {
                result = Some(Ok(response.0));
                break;
            }
            Ok(Err(error)) => {
                if lease.validate_media(&a).await.is_err()
                    && active_lease(&a, &g).await.is_some()
                {
                    continue;
                }
                result = Some(Err(error));
                break;
            }
            Err(_) => {
                result = Some(Err(ApiError(
                    StatusCode::GATEWAY_TIMEOUT,
                    "Playback startup timed out".into(),
                )));
                break;
            }
        }
    }
    let worker = match result {
        Some(Ok(response)) => {
            let id = response["id"].as_str().unwrap().to_owned();
            a.shared_playback
                .workers
                .lock()
                .unwrap()
                .insert(id.clone(), Arc::downgrade(&g));
            // Keep the internal worker capability private. Every client read is
            // instead authorized using that client's independently issued lease.
            a.resource_owners
                .lock()
                .unwrap()
                .remove(&format!("playback:{id}"));
            g.state.lock().unwrap().response = Some(response);
            Some(id)
        }
        Some(Err(ApiError(code, message))) => {
            g.state.lock().unwrap().error = Some((code, message));
            None
        }
        None => {
            g.state.lock().unwrap().error =
                Some((StatusCode::CONFLICT, "Playback startup cancelled".into()));
            None
        }
    };
    g.notify.notify_waiters();
    if let Some(worker) = &worker {
        loop {
            // Viewer expiry validation touches SQLite; run it on the blocking
            // pool instead of locking the database from the async worker.
            let group = g.clone();
            let worker_app = a.clone();
            let expired = blocking(move || {
                let db = worker_app.db.lock().unwrap();
                let state = group.state.lock().unwrap();
                Ok(state
                    .viewers
                    .iter()
                    .filter(|(_, v)| {
                        v.touched.elapsed() >= worker_app.playback.session_ttl()
                            || v.lease.validate(&db).is_err()
                    })
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<_>>())
            })
            .await
            .unwrap_or_default();
            for id in expired {
                a.shared_playback.detach(&id);
                a.resource_owners
                    .lock()
                    .unwrap()
                    .remove(&format!("playback:{id}"));
            }
            if g.state.lock().unwrap().viewers.is_empty() || a.playback.is_shutting_down() {
                break;
            }
            if let Some(value) = live::snapshot(&a, worker) {
                g.state.lock().unwrap().response = Some(value);
            }
            a.playback.heartbeat(worker).await;
            tokio::select! {_=tokio::time::sleep(Duration::from_millis(250))=>{},_=g.notify.notified()=>{}}
        }
        if !live::stop(&a, worker).await {
            a.playback.stop(worker).await;
        }
        a.playback.settle_cancelled_inputs().await;
        a.shared_playback.workers.lock().unwrap().remove(worker);
    }
    let ids = {
        let state = g.state.lock().unwrap();
        state.viewers.keys().cloned().collect::<Vec<_>>()
    };
    let _ = g.finish.send(true);
    for id in ids {
        a.shared_playback.detach(&id);
        a.resource_owners
            .lock()
            .unwrap()
            .remove(&format!("playback:{id}"));
    }
    a.shared_playback
        .audiences
        .lock()
        .unwrap()
        .remove(&g.audience);
    let mut groups = a.shared_playback.groups.lock().unwrap();
    if groups.get(&key).is_some_and(|old| Arc::ptr_eq(old, &g)) {
        groups.remove(&key);
    }
    g.notify.notify_waiters();
}
pub(super) async fn stop(a: &App, id: &str) -> bool {
    let Some(g) = a.shared_playback.group(id) else {
        return false;
    };
    a.shared_playback.detach(id);
    let last = g.state.lock().unwrap().viewers.is_empty();
    if last {
        let mut finished = g.finished.clone();
        while !*finished.borrow() {
            let _ = finished.changed().await;
        }
    }
    true
}
pub(super) async fn heartbeat(a: &App, id: &str, recover: Option<u64>) -> Option<ApiResult> {
    let group = a.shared_playback.group(id)?;
    if let Err(e) = validate_request(a).await {
        return Some(Err(e));
    }
    let (worker, count) = {
        let mut state = group.state.lock().unwrap();
        let viewer = state.viewers.get_mut(id)?;
        viewer.touched = Instant::now();
        (
            state.response.as_ref()?.get("id")?.as_str()?.to_owned(),
            state.viewers.len(),
        )
    };
    // A client fault alone must not interrupt another device's healthy output.
    let recover = if recover.is_some() && count > 1 {
        let mut snapshot = live::snapshot(a, &worker);
        if snapshot
            .as_ref()
            .is_some_and(|v| v["worker_state"] == "playing" && v["generation"].as_u64() == recover)
        {
            // EOF can reach a client just before the supervisor's 250ms tick.
            // Let it confirm a shared stall before classifying a device fault.
            tokio::time::sleep(Duration::from_millis(350)).await;
            if let Err(error) = validate_request(a).await {
                return Some(Err(error));
            }
            snapshot = live::snapshot(a, &worker);
        }
        let engine = snapshot
            .as_ref()
            .and_then(|v| v["url"].as_str())
            .and_then(|url| url.split('/').nth(2));
        let ended = if let Some(engine) = engine {
            !a.playback.input_running(engine).await
        } else {
            false
        };
        if snapshot.as_ref().is_some_and(|v| {
            v["worker_state"] == "recovering" || v["generation"].as_u64() != recover
        }) {
            // Decoder EOF during shared recovery is expected. Keep this viewer
            // attached and report the ongoing or already-completed generation.
            None
        } else if ended {
            recover
        } else {
            return Some(Ok(axum::Json(
                json!({"ok":true,"managed_live":true,"state":"failed","reason":"player_failed","generation":recover,"playback":Value::Null}),
            )));
        }
    } else {
        recover
    };
    if let Some(result) = live::heartbeat(a, &worker, recover).await {
        return Some(result.and_then(|mut response| {
            if response.0["playback"].is_object() {
                group.state.lock().unwrap().response = Some(response.0["playback"].clone());
                response.0["playback"] = render(&group, id, response.0["playback"].clone())?;
            }
            Ok(response)
        }));
    }
    Some(if a.playback.heartbeat(&worker).await {
        Ok(axum::Json(json!({"ok":true})))
    } else {
        Err(ApiError(
            StatusCode::NOT_FOUND,
            "Playback output ended".into(),
        ))
    })
}
pub(super) fn original_target(
    a: &App,
    id: &str,
    cap: &str,
) -> Option<Result<(String, String), String>> {
    let group = a.shared_playback.group(id)?;
    let mut state = group.state.lock().unwrap();
    let response = state.response.clone()?;
    let generation = response["generation"].as_u64().unwrap_or(1);
    let viewer = state.viewers.get_mut(id)?;
    if format!("{}-{generation}", viewer.capability) != cap {
        return Some(Err("Media expired".into()));
    }
    viewer.touched = Instant::now();
    let parts = response["url"].as_str()?.split('/').collect::<Vec<_>>();
    if parts.len() != 5 {
        return Some(Err("Media expired".into()));
    }
    Some(Ok((parts[2].to_owned(), parts[3].to_owned())))
}

pub(super) async fn serve(
    a: &App,
    id: &str,
    cap: &str,
    file: &str,
) -> Option<Result<axum::response::Response, String>> {
    let group = a.shared_playback.group(id)?;
    let response = {
        let mut state = group.state.lock().unwrap();
        let generation = state
            .response
            .as_ref()
            .and_then(|r| r["generation"].as_u64())
            .unwrap_or(1);
        let viewer = state.viewers.get_mut(id)?;
        if format!("{}-{generation}", viewer.capability) != cap {
            return Some(Err("Media not found".into()));
        }
        viewer.touched = Instant::now();
        state.response.clone()?
    };
    let url = response["url"].as_str()?;
    let parts = url.split('/').collect::<Vec<_>>();
    if parts.len() != 5 {
        return Some(Err("Media not found".into()));
    }
    let result = a.playback.serve(parts[2], parts[3], file).await;
    let state = group.state.lock().unwrap();
    let current = state
        .response
        .as_ref()
        .and_then(|r| r["generation"].as_u64())
        .unwrap_or(1);
    if !state
        .viewers
        .get(id)
        .is_some_and(|v| format!("{}-{current}", v.capability) == cap)
    {
        return Some(Err("Media not found".into()));
    }
    Some(result)
}
pub(crate) fn diagnostics(a: &App) -> Value {
    let groups = a
        .shared_playback
        .audiences
        .lock()
        .unwrap()
        .values()
        .filter_map(std::sync::Weak::upgrade)
        .collect::<Vec<_>>();
    json!({"viewers":a.shared_playback.viewers.lock().unwrap().len(),"workers":groups.iter().filter(|g|g.state.lock().unwrap().response.is_some()).count(),"groups":groups.iter().map(|g|{let s=g.state.lock().unwrap();json!({"viewers":s.viewers.len(),"state":if s.response.is_some(){"playing"}else if s.error.is_some(){"failed"}else{"starting"}})}).collect::<Vec<_>>()})
}

pub(crate) fn active_candidate(a: &App, candidate: &str) -> Option<String> {
    let groups = a
        .shared_playback
        .workers
        .lock()
        .unwrap()
        .values()
        .filter_map(std::sync::Weak::upgrade)
        .collect::<Vec<_>>();
    for group in &groups {
        let state = group.state.lock().unwrap();
        if let Some(response) = &state.response {
            if response["candidate_id"] == candidate && !state.viewers.is_empty() {
                return response["url"]
                    .as_str()
                    .and_then(|url| url.split('/').nth(2))
                    .map(str::to_owned);
            }
        }
    }
    None
}

pub(super) fn authorize_audience(a: &App, id: &str, db: &Connection) -> Result<(), ApiError> {
    let group = a
        .shared_playback
        .audiences
        .lock()
        .unwrap()
        .get(id)
        .and_then(std::sync::Weak::upgrade)
        .ok_or("Playback audience ended")?;
    let state = group.state.lock().unwrap();
    if state.viewers.values().any(|viewer| {
        viewer.touched.elapsed() < a.playback.session_ttl() && viewer.lease.validate(db).is_ok()
    }) {
        Ok(())
    } else {
        Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "Playback audience no longer authorized".into(),
        ))
    }
}

#[cfg(all(test, unix))]
mod tests;
