//! Viewer leases are independent of shared input ownership. Only this module
//! translates a viewer's private capability into the current worker generation.
use super::*;
use sha2::{Digest, Sha256};
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
}
struct GroupState {
    viewers: HashMap<String, Viewer>,
    response: Option<Value>,
    error: Option<(StatusCode, String)>,
    finished: bool,
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
fn active_lease(a: &App, g: &Group) -> Option<ResourceLease> {
    let leases = {
        let state = g.state.lock().unwrap();
        state
            .viewers
            .values()
            .filter(|v| v.touched.elapsed() < a.playback.session_ttl())
            .map(|v| v.lease.clone())
            .collect::<Vec<_>>()
    };
    let db = a.db.lock().unwrap();
    leases.into_iter().find(|lease| lease.validate(&db).is_ok())
}
pub(super) fn worker_access(a: &App, id: &str) -> Option<App> {
    let group = a
        .shared_playback
        .workers
        .lock()
        .unwrap()
        .get(id)
        .and_then(std::sync::Weak::upgrade)?;
    active_lease(a, &group).map(|lease| a.clone().with_lease(lease))
}
pub(super) fn worker_known(a: &App, id: &str) -> bool {
    a.shared_playback.workers.lock().unwrap().contains_key(id)
}
fn validate_source_scope(a: &App, v: &PlaybackRequest, db: &Connection) -> Result<(), ApiError> {
    let provider_kind = if let Some(id) = &v.stream_id {
        let streams = a.streams.lock().unwrap();
        let source = streams.get(id).ok_or("Stream expired; discover again")?;
        source.provider_id.map(|p| (p, source.kind.clone()))
    } else if let Some(id) = &v.channel_id {
        if lineup::family_id(db, id)?.is_some() {
            None
        } else {
            db.query_row(
                "SELECT provider_id FROM provider_live WHERE id=?1",
                [id],
                |r| r.get::<_, i64>(0),
            )
            .ok()
            .map(|p| (p, "live".into()))
        }
    } else {
        None
    };
    if let Some((provider, kind)) = provider_kind {
        let allowed: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM providers WHERE id=?1 AND enabled=1 AND CASE ?2 WHEN 'live' THEN enable_live WHEN 'movie' THEN enable_movies WHEN 'series' THEN enable_series ELSE 0 END=1)",params![provider,kind],|r|r.get(0)).map_err(db_error)?;
        if !allowed {
            return Err("Provider not found or disabled".into());
        }
    }
    Ok(())
}
fn source_revision(
    a: &App,
    v: &PlaybackRequest,
    db: &Connection,
) -> Result<Option<String>, ApiError> {
    let streams = a.streams.lock().unwrap();
    v.stream_id
        .as_ref()
        .and_then(|id| streams.get(id))
        .and_then(|s| s.provider_id)
        .map(|id| {
            db.query_row(
                "SELECT json_array(url,username,password) FROM providers WHERE id=?1",
                [id],
                |r| r.get::<_, String>(0),
            )
            .map(|value| format!("{:x}", Sha256::digest(value)))
            .map_err(db_error)
        })
        .transpose()
}
async fn identity(a: &App, v: &PlaybackRequest) -> Result<(String, Option<String>), ApiError> {
    validate_request(a).await?;
    {
        let db = a.db.lock().unwrap();
        validate_source_scope(a, v, &db)?;
    }
    if !v.position.is_finite() || v.position < 0.0 || v.position > 604800.0 {
        return Err("Invalid playback position".into());
    }
    let source = match (&v.channel_id, &v.stream_id) {
        (Some(channel), None) => {
            let family = {
                let db = a.db.lock().unwrap();
                lineup::family_id(&db, channel)?
                    .map(|family| json!(["family", family, lineup::recovery_policy(&db).ok()]))
            };
            if let Some(value) = family {
                value
            } else {
                let id = channel.clone();
                let (url, provider) = a.providers.blocking(move |p| p.channel_source(&id)).await?;
                json!(["provider-live", provider, url])
            }
        }
        (None, Some(id)) => {
            a.check_resource(&a.identity(), "stream", id)?;
            let streams = a.streams.lock().unwrap();
            let source = streams.get(id).ok_or("Stream expired; discover again")?;
            let mut headers = source.headers.iter().collect::<Vec<_>>();
            headers.sort();
            let auth::Principal::Account { account_id, .. } = a.identity();
            // Provider catalogs are server-owned; addon sources remain confined
            // to the VIPTV account that discovered them, including its headers.
            json!([
                "source",
                source.url,
                headers,
                source.live,
                source.kind,
                source.provider_id,
                source.provider_id.is_none().then_some(account_id)
            ])
        }
        _ => return Err("Provide exactly one stream_id or channel_id".into()),
    };
    let provider_revision = source_revision(a, v, &a.db.lock().unwrap())?;
    let material = json!([
        source,
        provider_revision,
        v.position,
        v.capabilities.clone().unwrap_or_default(),
        v.force_transcode,
        v.audio_track_index,
        v.audio_language,
        v.subtitle_track_index,
        v.subtitles_off,
        v.preferences.audio_language,
        v.preferences.subtitle_language,
        v.preferences.subtitles_enabled
    ]);
    Ok((
        format!("{:x}", Sha256::digest(material.to_string())),
        provider_revision,
    ))
}
pub(super) async fn start(a: App, v: PlaybackRequest) -> ApiResult {
    let (key, revision) = identity(&a, &v).await?;
    let registry = a.shared_playback.clone();
    let prior = registry.groups.lock().unwrap().get(&key).cloned();
    if let Some(group) = prior {
        let response = group.state.lock().unwrap().response.clone();
        let reusable = if let Some(response) = response {
            if response["live"] == true {
                let db = a.db.lock().unwrap();
                response["worker_state"] != "failed"
                    && response["candidate_id"]
                        .as_str()
                        .map(|candidate| {
                            let channel = response["channel_id"].as_str().unwrap_or("");
                            lineup::eligible(&db, channel, candidate).unwrap_or(false)
                                && provider::selection::current_source(&db, candidate)
                                    .ok()
                                    .as_deref()
                                    == response["_source_key"].as_str()
                        })
                        .unwrap_or(true)
            } else if let Some(id) = response["id"].as_str() {
                a.playback.timeline_origin_available(id).await
            } else {
                false
            }
        } else {
            true
        };
        if !reusable {
            let mut groups = registry.groups.lock().unwrap();
            if groups.get(&key).is_some_and(|g| Arc::ptr_eq(g, &group)) {
                groups.remove(&key);
            }
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    let capability = uuid::Uuid::new_v4().simple().to_string();
    let (group, created) = {
        let db = a.db.lock().unwrap();
        validate_source_scope(&a, &v, &db)?;
        if source_revision(&a, &v, &db)? != revision {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "Source changed; retry playback".into(),
            ));
        }
        let mut groups = registry.groups.lock().unwrap();
        let mut viewers = registry.viewers.lock().unwrap();
        if viewers.len() >= 4096 {
            return Err(ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "Viewer capacity reached".into(),
            ));
        }
        let existing = groups
            .get(&key)
            .filter(|g| {
                let state = g.state.lock().unwrap();
                !state.finished
                    && state.response.as_ref().is_none_or(|response| {
                        response["candidate_id"].as_str().is_none_or(|candidate| {
                            lineup::eligible(
                                &db,
                                response["channel_id"].as_str().unwrap_or(""),
                                candidate,
                            )
                            .unwrap_or(false)
                                && provider::selection::current_source(&db, candidate)
                                    .ok()
                                    .as_deref()
                                    == response["_source_key"].as_str()
                        })
                    })
            })
            .cloned();
        let created = existing.is_none();
        let group = existing.unwrap_or_else(|| {
            Arc::new(Group {
                audience: uuid::Uuid::new_v4().to_string(),
                state: Mutex::new(GroupState {
                    viewers: HashMap::new(),
                    response: None,
                    error: None,
                    finished: false,
                }),
                notify: tokio::sync::Notify::new(),
            })
        });
        group.state.lock().unwrap().viewers.insert(
            id.clone(),
            Viewer {
                lease: a.request_lease(),
                capability,
                touched: Instant::now(),
                options: v.clone(),
            },
        );
        viewers.insert(id.clone(), group.clone());
        groups.insert(key.clone(), group.clone());
        (group, created)
    };
    let mut pending = Pending {
        registry: registry.clone(),
        id: id.clone(),
        armed: true,
    };
    if created {
        let app = a.clone();
        let work = group.clone();
        tokio::spawn(async move {
            run(app, work, key, v).await;
        });
    }
    loop {
        let notified = group.notify.notified();
        let (response, error) = {
            let state = group.state.lock().unwrap();
            (state.response.clone(), state.error.clone())
        };
        if let Some((code, message)) = error {
            validate_request(&a).await?;
            return Err(ApiError(code, message));
        }
        if let Some(response) = response {
            validate_request(&a).await?;
            let result = render(&group, &id, response)?;
            a.own_resource("playback", &id);
            pending.armed = false;
            return Ok(axum::Json(result));
        }
        tokio::select! {_=notified=>{},_=tokio::time::sleep(Duration::from_millis(50))=>{}}
    }
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
async fn no_viewers(a: &App, g: &Group) {
    loop {
        if active_lease(a, g).is_none() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
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
        let Some(lease) = active_lease(&a, &g) else {
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
                if lease.validate(&a.db.lock().unwrap()).is_err() && active_lease(&a, &g).is_some()
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
            let expired = {
                let db = a.db.lock().unwrap();
                let state = g.state.lock().unwrap();
                state
                    .viewers
                    .iter()
                    .filter(|(_, v)| {
                        v.touched.elapsed() >= a.playback.session_ttl()
                            || v.lease.validate(&db).is_err()
                    })
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<_>>()
            };
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
        let mut state = g.state.lock().unwrap();
        state.finished = true;
        state.viewers.keys().cloned().collect::<Vec<_>>()
    };
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
        while !g.state.lock().unwrap().finished {
            tokio::time::sleep(Duration::from_millis(10)).await;
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
) -> Option<Result<(String, Vec<u8>), String>> {
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
