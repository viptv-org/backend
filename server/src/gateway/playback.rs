//! Backend-owned playback leases. Video bytes travel directly to the selected
//! delivery endpoint; this module never invokes or relays the embedded engine.
use super::{
    client::Client,
    protocol,
    registry::{self, AuthorizedGateway},
};
use crate::{
    account_api::Error,
    app_state::{App, ResourceLease},
    util, ApiError,
};
use axum::{
    extract::{rejection::JsonRejection, Path, State},
    http::StatusCode,
    Extension, Json,
};
use futures::{stream, FutureExt, StreamExt};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

const LEASE: Duration = Duration::from_secs(60);
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Platform {
    Android,
    AndroidTv,
    Desktop,
    Web,
    Tizen,
    Webos,
    Roku,
    Vizio,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Facts {
    platform: Platform,
    #[serde(default)]
    can_play_direct: bool,
    max_width: u32,
    max_height: u32,
    video_codecs: Vec<String>,
    audio_codecs: Vec<String>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Start {
    request_id: String,
    stream_id: String,
    client: Facts,
    #[serde(default)]
    position: f64,
    #[serde(default)]
    force_gateway: bool,
    audio_track: Option<u32>,
    subtitle_track: Option<u32>,
}
#[derive(Clone)]
struct Proof {
    lease: ResourceLease,
    producer: String,
    configuration: [u8; 32],
    kind: String,
}
struct Source {
    proof: Proof,
    url: String,
    headers: BTreeMap<String, String>,
    live: bool,
    provider_id: Option<i64>,
    identity: [u8; 32],
}
#[derive(Clone)]
struct Remote {
    target: Arc<AuthorizedGateway>,
    viewer: String,
    client: Client,
    expires_at: u64,
}
struct StateData {
    status: &'static str,
    delivery: Option<Value>,
    error: Option<&'static str>,
    touched: Instant,
    remote: Option<Remote>,
}
struct Entry {
    id: String,
    scope: String,
    request_id: String,
    request_hash: [u8; 32],
    lease: ResourceLease,
    proof: Proof,
    identity: [u8; 32],
    cancelled: AtomicBool,
    state: Mutex<StateData>,
    permit: Mutex<Option<tokio::sync::OwnedSemaphorePermit>>,
}
pub(crate) struct Registry {
    entries: Mutex<HashMap<String, Arc<Entry>>>,
}
impl Registry {
    pub(crate) fn new(db: Arc<Mutex<Connection>>) -> Arc<Self> {
        let registry = Arc::new(Self {
            entries: Mutex::new(HashMap::new()),
        });
        let weak = Arc::downgrade(&registry);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let Some(registry) = weak.upgrade() else {
                    break;
                };
                let facts = registry
                    .entries
                    .lock()
                    .unwrap()
                    .values()
                    .map(|entry| {
                        let state = entry.state.lock().unwrap();
                        (
                            entry.id.clone(),
                            entry.lease.clone(),
                            entry.proof.clone(),
                            expired(&state),
                            state.status,
                            state.remote.as_ref().map(|remote| {
                                (
                                    remote.target.gateway.id.clone(),
                                    remote.target.gateway.revision,
                                )
                            }),
                        )
                    })
                    .collect::<Vec<_>>();
                let database = db.clone();
                let failed_checks = facts
                    .iter()
                    .filter(|(_, _, _, _, status, _)| matches!(*status, "starting" | "ready"))
                    .map(|(id, ..)| (id.clone(), "authorization_expired"))
                    .collect::<Vec<_>>();
                let stale = tokio::task::spawn_blocking(move || {
                    let database = database.lock();
                    facts
                        .into_iter()
                        .filter_map(|(id, lease, proof, expired, status, remote)| {
                            if !matches!(status, "starting" | "ready") {
                                return None;
                            }
                            if expired {
                                return Some((id, "playback_expired"));
                            }
                            let Ok(db) = database.as_ref() else {
                                return Some((id, "gateway_storage_unavailable"));
                            };
                            if let Err(error) = lease.validate(db) {
                                return Some((
                                    id,
                                    error.api_error_code().unwrap_or("authorization_expired"),
                                ));
                            }
                            if let Err(error) = validate_source(db, &proof) {
                                return Some((id, failure_code(error)));
                            }
                            if remote.is_some_and(|(gateway, revision)| {
                                !gateway_current(db, &lease, &gateway, revision)
                            }) {
                                return Some((id, "gateway_not_found"));
                            }
                            None
                        })
                        .collect::<Vec<_>>()
                })
                .await
                .unwrap_or(failed_checks);
                let mut releases = Vec::new();
                for (id, reason) in stale {
                    if let Some(entry) = registry.entries.lock().unwrap().get(&id).cloned() {
                        if let Some(remote) = terminate(&entry, "expired", Some(reason)) {
                            releases.push(remote);
                        }
                    }
                }
                registry.entries.lock().unwrap().retain(|_, entry| {
                    let state = entry.state.lock().unwrap();
                    matches!(state.status, "starting" | "ready")
                        || state.touched.elapsed() < Duration::from_secs(300)
                });
                drop(registry);
                stream::iter(releases)
                    .for_each_concurrent(4, |remote| async move {
                        release_remote(&remote.client, &remote.target, &remote.viewer).await;
                    })
                    .await;
            }
        });
        registry
    }
    fn snapshot(&self, id: &str, lease: &ResourceLease) -> Result<Arc<Entry>, Error> {
        self.entries
            .lock()
            .unwrap()
            .get(id)
            .filter(|entry| {
                entry.scope == App::scoped_key(&lease.principal)
                    && entry.lease.session_id == lease.session_id
            })
            .cloned()
            .ok_or(Error::Code("playback_not_found"))
    }
    fn affinity(&self, account: i64, identity: [u8; 32]) -> Option<(String, i64)> {
        self.entries.lock().unwrap().values().find_map(|entry| {
            if entry.identity != identity || entry.lease.principal.account_id() != Some(account) {
                return None;
            }
            let state = entry.state.lock().unwrap();
            if state.status != "ready" || expired(&state) {
                return None;
            }
            state.remote.as_ref().map(|remote| {
                (
                    remote.target.gateway.id.clone(),
                    remote.target.gateway.revision,
                )
            })
        })
    }
}
fn account(lease: &ResourceLease) -> i64 {
    lease.principal.account_id().expect("authenticated account")
}
fn failure_code(error: Error) -> &'static str {
    match error {
        Error::Code(code) => code,
        Error::Auth(error) => error.api_error_code().unwrap_or("authorization_expired"),
    }
}
fn gateway_current(db: &Connection, lease: &ResourceLease, id: &str, revision: i64) -> bool {
    registry::list(db, account(lease)).is_ok_and(|gateways| {
        gateways
            .iter()
            .any(|gateway| gateway.id == id && gateway.revision == revision && gateway.enabled)
    })
}
fn validate_source(db: &Connection, proof: &Proof) -> Result<(), Error> {
    proof.lease.validate(db)?;
    let Some((kind, id)) = proof.producer.split_once(':') else {
        return Err(Error::Code("source_not_found"));
    };
    let id = id
        .parse::<i64>()
        .map_err(|_| Error::Code("source_not_found"))?;
    let allowed:bool=match kind {
        "iptv"=>db.query_row("SELECT EXISTS(SELECT 1 FROM providers p JOIN provider_ownership o ON o.provider_id=p.id WHERE p.id=?1 AND o.account_id=?2 AND p.enabled=1 AND ((?3='live' AND p.enable_live=1) OR (?3='movie' AND p.enable_movies=1) OR (?3='series' AND p.enable_series=1)))",params![id,account(&proof.lease),proof.kind],|row|row.get(0)),
        "addon"=>db.query_row("SELECT EXISTS(SELECT 1 FROM addons WHERE id=?1 AND account_id=?2 AND enabled=1)",params![id,account(&proof.lease)],|row|row.get(0)),
        _=>return Err(Error::Code("source_not_found")),
    }.map_err(|_|Error::Code("provider_storage_unavailable"))?;
    if !allowed {
        return Err(Error::Code("source_not_found"));
    }
    if crate::sources::source_configuration(db, &proof.producer)? != Some(proof.configuration) {
        return Err(Error::Code("source_configuration_changed"));
    }
    Ok(())
}
async fn source(app: &App, lease: &ResourceLease, id: String) -> Result<Source, Error> {
    let app = app.clone().with_lease(lease.clone());
    tokio::task::spawn_blocking(move || {
        app.check_resource(&app.identity(), "stream", &id)?;
        let source_lease = app
            .resource_lease("stream", &id)
            .ok_or(Error::Code("source_not_found"))?;
        let (proof, url, headers, live, provider_id) = {
            let streams = app.streams.lock().unwrap();
            let entry = streams
                .get(&id)
                .filter(|entry| entry.created.elapsed() < Duration::from_secs(1800))
                .ok_or(Error::Code("source_not_found"))?;
            (
                Proof {
                    lease: source_lease,
                    producer: entry.producer.clone(),
                    configuration: entry.configuration.ok_or(Error::Code("source_not_found"))?,
                    kind: entry.kind.clone(),
                },
                entry.url.clone(),
                entry
                    .headers
                    .iter()
                    .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
                    .collect::<BTreeMap<_, _>>(),
                entry.live,
                entry.provider_id,
            )
        };
        let db = app.db.lock().unwrap();
        app.require_media(&db)?;
        validate_source(&db, &proof)?;
        if headers
            .keys()
            .any(|key| key == "x-viptv-egress-proxy" || key == "x-playback-egress-proxy")
        {
            return Err(Error::Code("source_route_migration_required"));
        }
        let identity = Sha256::digest(
            json!([account(&proof.lease), url, headers, live])
                .to_string()
                .as_bytes(),
        )
        .into();
        Ok(Source {
            proof,
            url,
            headers,
            live,
            provider_id,
            identity,
        })
    })
    .await
    .map_err(|_| Error::Code("provider_storage_unavailable"))?
}
fn response(entry: &Entry) -> Value {
    let state = entry.state.lock().unwrap();
    let error = state.error.map(crate::account_api::description);
    let local_expiry = util::now() as u64 + LEASE.saturating_sub(state.touched.elapsed()).as_secs();
    let expires_at = if matches!(state.status, "starting" | "ready") {
        state
            .remote
            .as_ref()
            .map_or(local_expiry, |remote| remote.expires_at.min(local_expiry))
    } else {
        util::now() as u64
    };
    json!({"id":entry.id,"status":state.status,"delivery":state.delivery,"error_code":state.error,"error":error,"expires_at":expires_at,"renew_after_seconds":20})
}
fn expired(state: &StateData) -> bool {
    state.touched.elapsed() >= LEASE
        || state
            .remote
            .as_ref()
            .is_some_and(|remote| remote.expires_at <= util::now() as u64)
}
fn terminate(entry: &Entry, status: &'static str, error: Option<&'static str>) -> Option<Remote> {
    entry.cancelled.store(true, Ordering::Release);
    entry.permit.lock().unwrap().take();
    let mut state = entry.state.lock().unwrap();
    if !matches!(state.status, "starting" | "ready") {
        return None;
    }
    state.status = status;
    state.error = error;
    state.delivery = None;
    state.touched = Instant::now();
    state.remote.take()
}
async fn release_remote(client: &Client, target: &AuthorizedGateway, viewer: &str) {
    if !protocol::identifier(viewer) {
        return;
    }
    let _ = client
        .request(
            &target.gateway.endpoint,
            target.key.expose(),
            reqwest::Method::DELETE,
            &format!("v1/sessions/{viewer}"),
            None,
            None,
            Duration::from_secs(5),
        )
        .await;
}
struct Cleanup {
    client: Client,
    target: Arc<AuthorizedGateway>,
    viewer: String,
    armed: bool,
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        if self.armed {
            let client = self.client.clone();
            let target = self.target.clone();
            let viewer = self.viewer.clone();
            tokio::spawn(async move {
                release_remote(&client, &target, &viewer).await;
            });
        }
    }
}

async fn choose(
    app: App,
    lease: ResourceLease,
    identity: [u8; 32],
) -> Result<Arc<AuthorizedGateway>, Error> {
    let actor = account(&lease);
    let vault = app
        .secret_vault
        .clone()
        .ok_or(Error::Code("gateway_required"))?;
    let database = app.db.clone();
    let lease = lease.clone();
    let candidates = tokio::task::spawn_blocking(move || {
        let db = database.lock().unwrap();
        lease.validate(&db)?;
        let candidates = registry::list(&db, actor)?
            .into_iter()
            .filter(|gateway| gateway.enabled)
            .map(|gateway| registry::authorized(&db, &vault, actor, &gateway.id).map(Arc::new))
            .collect::<Vec<_>>();
        Ok::<_, Error>(candidates)
    })
    .await
    .map_err(|_| Error::Code("gateway_storage_unavailable"))??;
    if candidates.is_empty() {
        return Err(Error::Code("gateway_required"));
    }
    if let Some((id, revision)) = app.gateway_playbacks.affinity(actor, identity) {
        if let Some(target) = candidates
            .iter()
            .filter_map(|candidate| candidate.as_ref().ok())
            .find(|candidate| candidate.gateway.id == id && candidate.gateway.revision == revision)
        {
            return Ok(target.clone());
        }
    }
    let client = app.gateway_client.clone();
    let mut valid = Vec::new();
    let mut prior_error = "gateway_unavailable";
    for candidate in candidates {
        match candidate {
            Ok(candidate) => valid.push(candidate),
            Err(error) => prior_error = error,
        }
    }
    tokio::time::timeout(Duration::from_secs(8), async move {
        let mut candidates = stream::iter(valid)
            .map(|candidate: Arc<AuthorizedGateway>| {
                let client = client.clone();
                async move {
                    let capabilities = client
                        .capabilities(
                            &candidate.gateway.endpoint,
                            candidate.key.expose(),
                            &candidate.gateway.namespace,
                        )
                        .await?;
                    let available = capabilities.available.ok_or("gateway_protocol_invalid")?;
                    if available.inputs == 0 || available.outputs == 0 || available.viewers == 0 {
                        return Err("gateway_capacity");
                    }
                    Ok::<_, &'static str>(candidate)
                }
                .boxed()
            })
            .buffered(4);
        let mut error = prior_error;
        while let Some(result) = candidates.next().await {
            match result {
                Ok(candidate) => return Ok(candidate),
                Err(reason) => error = reason,
            }
        }
        Err(Error::Code(error))
    })
    .await
    .map_err(|_| Error::Code("gateway_unavailable"))?
}
fn validate(request: &Start) -> Result<(), Error> {
    if !protocol::identifier(&request.request_id)
        || request.stream_id.is_empty()
        || request.stream_id.len() > 128
        || !request.position.is_finite()
        || !(0.0..=604800.0).contains(&request.position)
        || !(2..=16384).contains(&request.client.max_width)
        || !(2..=16384).contains(&request.client.max_height)
        || request.audio_track.is_some_and(|index| index > 65535)
        || request.subtitle_track.is_some_and(|index| index > 65535)
        || [&request.client.video_codecs, &request.client.audio_codecs]
            .iter()
            .any(|values| {
                values.is_empty()
                    || values.len() > 16
                    || values.iter().any(|value| {
                        value.is_empty()
                            || value.len() > 32
                            || !value
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                    })
            })
    {
        return Err(Error::Code("invalid_playback_request"));
    }
    Ok(())
}
pub(crate) async fn start(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    body: Result<Json<Start>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), Error> {
    let Json(request) = body.map_err(|_| Error::Code("invalid_playback_request"))?;
    validate(&request)?;
    let hash: [u8; 32] = Sha256::digest(
        serde_json::to_vec(&request).map_err(|_| Error::Code("invalid_playback_request"))?,
    )
    .into();
    let scope = App::scoped_key(&lease.principal);
    let existing = app
        .gateway_playbacks
        .entries
        .lock()
        .unwrap()
        .values()
        .find(|entry| entry.scope == scope && entry.request_id == request.request_id)
        .cloned();
    if let Some(entry) = existing {
        if entry.request_hash != hash {
            return Err(Error::Code("playback_conflict"));
        }
        inspect_entry(&app, &entry).await?;
        return Ok((StatusCode::OK, Json(response(&entry))));
    }
    let input = source(&app, &lease, request.stream_id.clone()).await?;
    if input.live && request.position != 0.0 {
        return Err(Error::Code("invalid_playback_request"));
    }
    let native = request.client.can_play_direct
        && !matches!(request.client.platform, Platform::Roku | Platform::Vizio)
        && (!matches!(request.client.platform, Platform::Web)
            || (input.url.starts_with("https://") && input.headers.is_empty()));
    let direct = native && !request.force_gateway;
    if !direct {
        let db = app.db.lock().unwrap();
        lease.validate(&db)?;
        if registry::list(&db, account(&lease))?
            .iter()
            .all(|gateway| !gateway.enabled)
        {
            return Err(Error::Code("gateway_required"));
        }
    }
    let entry = {
        let mut entries = app.gateway_playbacks.entries.lock().unwrap();
        if let Some(entry) = entries
            .values()
            .find(|entry| entry.scope == scope && entry.request_id == request.request_id)
        {
            if entry.request_hash != hash {
                return Err(Error::Code("playback_conflict"));
            }
            return Ok((StatusCode::OK, Json(response(entry))));
        }
        if entries.len() >= 4096 {
            return Err(Error::Code("playback_capacity"));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let entry = Arc::new(Entry {
            id: id.clone(),
            scope,
            request_id: request.request_id.clone(),
            request_hash: hash,
            lease: lease.clone(),
            proof: input.proof.clone(),
            identity: input.identity,
            cancelled: AtomicBool::new(false),
            state: Mutex::new(StateData {
                status: "starting",
                delivery: None,
                error: None,
                touched: Instant::now(),
                remote: None,
            }),
            permit: Mutex::new(None),
        });
        entries.insert(id, entry.clone());
        entry
    };
    let worker = entry.clone();
    tokio::spawn(async move {
        let outcome = tokio::time::timeout(
            Duration::from_secs(45),
            prepare(app.clone(), worker.clone(), input, request, direct),
        )
        .await;
        let failure = match outcome {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(failure_code(error)),
            Err(_) => Some("gateway_startup_timeout"),
        };
        if let Some(code) = failure {
            if let Some(remote) = terminate(&worker, "failed", Some(code)) {
                release_remote(&remote.client, &remote.target, &remote.viewer).await;
            }
        }
    });
    Ok((StatusCode::ACCEPTED, Json(response(&entry))))
}
async fn validate_entry(app: &App, entry: &Entry) -> Result<(), Error> {
    if entry.cancelled.load(Ordering::Acquire) || expired(&entry.state.lock().unwrap()) {
        return Err(Error::Code("playback_expired"));
    }
    let database = app.db.clone();
    let lease = entry.lease.clone();
    let proof = entry.proof.clone();
    let target = entry.state.lock().unwrap().remote.as_ref().map(|remote| {
        (
            remote.target.gateway.id.clone(),
            remote.target.gateway.revision,
        )
    });
    tokio::task::spawn_blocking(move || {
        let db = database.lock().unwrap();
        lease.validate(&db)?;
        validate_source(&db, &proof)?;
        if target.is_some_and(|(id, revision)| !gateway_current(&db, &lease, &id, revision)) {
            return Err(Error::Code("gateway_not_found"));
        }
        Ok(())
    })
    .await
    .map_err(|_| Error::Code("gateway_storage_unavailable"))?
}
async fn inspect_entry(app: &App, entry: &Entry) -> Result<(), Error> {
    let terminal = !matches!(entry.state.lock().unwrap().status, "starting" | "ready");
    if !terminal {
        return validate_entry(app, entry).await;
    }
    let database = app.db.clone();
    let lease = entry.lease.clone();
    tokio::task::spawn_blocking(move || {
        lease
            .validate(&database.lock().unwrap())
            .map_err(Error::from)
    })
    .await
    .map_err(|_| Error::Code("gateway_storage_unavailable"))?
}
async fn prepare(
    app: App,
    entry: Arc<Entry>,
    source: Source,
    request: Start,
    direct: bool,
) -> Result<(), Error> {
    validate_entry(&app, &entry).await?;
    let delivery = if direct {
        if let Some(provider) = source.provider_id {
            let permit = app
                .providers
                .acquire_playback_for_kind(provider, &source.proof.kind)
                .await
                .map_err(ApiError::from)?;
            *entry.permit.lock().unwrap() = Some(permit);
        }
        json!({"kind":"direct","url":source.url,"headers":source.headers,"position":request.position,"live":source.live,"format":"original"})
    } else {
        if !request
            .client
            .video_codecs
            .iter()
            .any(|codec| codec == "h264")
            || !request
                .client
                .audio_codecs
                .iter()
                .any(|codec| codec == "aac")
        {
            return Err(Error::Code("delivery_unsupported"));
        }
        let target = choose(app.clone(), entry.lease.clone(), source.identity).await?;
        validate_entry(&app, &entry).await?;
        let body = json!({"namespace":target.gateway.namespace,"input":{"url":source.url,"headers":source.headers,"live":source.live},"output":{"protocol":"hls","video_codecs":request.client.video_codecs,"audio_codecs":request.client.audio_codecs,"max_width":request.client.max_width,"max_height":request.client.max_height,"audio_track":request.audio_track,"subtitle_track":request.subtitle_track},"position_seconds":request.position});
        let value = app
            .gateway_client
            .request(
                &target.gateway.endpoint,
                target.key.expose(),
                reqwest::Method::POST,
                "v1/sessions",
                Some(&body),
                Some(&entry.id),
                Duration::from_secs(35),
            )
            .await?;
        let mut remote = protocol::Session::parse(value)?;
        let mut cleanup = Cleanup {
            client: app.gateway_client.clone(),
            target: target.clone(),
            viewer: remote.id.clone(),
            armed: true,
        };
        {
            let mut state = entry.state.lock().unwrap();
            if entry.cancelled.load(Ordering::Acquire) {
                return Err(Error::Code("playback_expired"));
            }
            state.remote = Some(Remote {
                target: target.clone(),
                viewer: remote.id.clone(),
                client: app.gateway_client.clone(),
                expires_at: remote.expires_at,
            });
        }
        loop {
            validate_entry(&app, &entry).await?;
            if remote.expires_at <= util::now() as u64 {
                return Err(Error::Code("playback_expired"));
            }
            if remote.status == "ready" {
                break;
            }
            if remote.status != "starting" {
                return Err(Error::Code(remote.failure()));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
            let value = app
                .gateway_client
                .request(
                    &target.gateway.endpoint,
                    target.key.expose(),
                    reqwest::Method::GET,
                    &format!("v1/sessions/{}", cleanup.viewer),
                    None,
                    None,
                    Duration::from_secs(5),
                )
                .await?;
            remote = protocol::Session::parse(value)?;
            if remote.id != cleanup.viewer {
                return Err(Error::Code("gateway_protocol_invalid"));
            }
        }
        let delivery = remote
            .delivery(&target.gateway.endpoint)?
            .ok_or(Error::Code("gateway_protocol_invalid"))?;
        cleanup.armed = false;
        delivery
    };
    validate_entry(&app, &entry).await?;
    let mut state = entry.state.lock().unwrap();
    if entry.cancelled.load(Ordering::Acquire) {
        return Err(Error::Code("playback_expired"));
    }
    state.delivery = Some(delivery);
    state.status = "ready";
    state.touched = Instant::now();
    Ok(())
}
pub(crate) async fn get(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Error> {
    let entry = app.gateway_playbacks.snapshot(&id, &lease)?;
    inspect_entry(&app, &entry).await?;
    Ok(Json(response(&entry)))
}
pub(crate) async fn renew(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Error> {
    let entry = app.gateway_playbacks.snapshot(&id, &lease)?;
    validate_entry(&app, &entry).await?;
    entry.state.lock().unwrap().touched = Instant::now();
    let remote = entry.state.lock().unwrap().remote.clone();
    if let Some(Remote { target, viewer, .. }) = remote {
        let value = app
            .gateway_client
            .request(
                &target.gateway.endpoint,
                target.key.expose(),
                reqwest::Method::POST,
                &format!("v1/sessions/{viewer}/renew"),
                None,
                None,
                Duration::from_secs(5),
            )
            .await?;
        let remote = protocol::Session::parse(value)?;
        if remote.id != viewer {
            return Err(Error::Code("gateway_protocol_invalid"));
        }
        if !matches!(remote.status.as_str(), "starting" | "ready")
            || remote.expires_at <= util::now() as u64
        {
            let failure = remote.failure();
            if let Some(remote) = terminate(&entry, "failed", Some(failure)) {
                release_remote(&remote.client, &remote.target, &remote.viewer).await;
            }
            return Err(Error::Code(failure));
        }
        {
            let mut state = entry.state.lock().unwrap();
            if let Some(current) = state.remote.as_mut() {
                if current.viewer == viewer {
                    current.expires_at = remote.expires_at;
                }
            }
        }
        validate_entry(&app, &entry).await?;
    }
    entry.state.lock().unwrap().touched = Instant::now();
    Ok(Json(response(&entry)))
}
pub(crate) async fn stop(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Error> {
    let entry = app.gateway_playbacks.snapshot(&id, &lease)?;
    if let Some(remote) = terminate(&entry, "released", None) {
        release_remote(&remote.client, &remote.target, &remote.viewer).await;
    }
    Ok(Json(json!({"ok":true})))
}
