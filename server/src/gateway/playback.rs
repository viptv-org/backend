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
    extract::{Path, State},
    http::StatusCode,
    Extension, Json,
};
use futures::{stream, FutureExt, StreamExt};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

const LEASE: Duration = Duration::from_secs(60);
const REQUEST_QUOTA: usize = 4096;
const MAX_NATIVE_GRANTS_PER_SESSION: usize = 2;
pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS playback_request_authority(
      session_id TEXT NOT NULL REFERENCES auth_sessions(id) ON DELETE CASCADE,
      request_id TEXT NOT NULL, scope TEXT NOT NULL, request_hash BLOB,
      playback_id TEXT, cancelled INTEGER NOT NULL DEFAULT 0,
      PRIMARY KEY(session_id,request_id));",
    )
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct NativeCapability {
    version: u32,
    network_policy: String,
}
fn native_capability<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<NativeCapability>, D::Error> {
    NativeCapability::deserialize(d).map(Some)
}

#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Conversion {
    #[default]
    Auto,
    Audio,
    Video,
    AudioVideo,
}
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
    #[serde(
        default,
        deserialize_with = "native_capability",
        skip_serializing_if = "Option::is_none"
    )]
    native_torrent: Option<NativeCapability>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Start {
    #[serde(skip)]
    decoder_start: bool,
    #[serde(default)]
    conversion: Conversion,
    request_id: String,
    stream_id: String,
    client: Facts,
    #[serde(default)]
    position: f64,
    #[serde(default)]
    force_gateway: bool,
    audio_track: Option<u32>,
    subtitle_track: Option<u32>,
    audio_language: Option<String>,
    preferred_audio_language: Option<String>,
    preferred_subtitle_language: Option<String>,
    #[serde(default)]
    subtitles_off: bool,
}
#[derive(Clone)]
struct Proof {
    live_channel_id: Option<String>,
    lease: ResourceLease,
    producer: String,
    configuration: [u8; 32],
    kind: String,
    exact_vod: Option<crate::app_state::ExactVod>,
}
struct Source {
    proof: Proof,
    url: String,
    file_index: Option<u32>,
    info_hash: Option<String>,
    trackers: Vec<String>,
    requires_torrent_gateway: bool,
    headers: BTreeMap<String, String>,
    live: bool,
    provider_id: Option<i64>,
    identity: [u8; 32],
}
#[derive(Clone)]
struct Remote {
    client_frame_ack: bool,
    target: Arc<AuthorizedGateway>,
    viewer: String,
    client: Client,
    expires_at: u64,
}
struct NativeState {
    expires_at: u64,
    last_server_time: u64,
    grant: Option<NativeGrant>,
    position: f64,
    preferences: viptv_core::native_torrent::NativeTorrentPreferences,
}

#[derive(Clone)]
enum NativeGrant {
    Legacy(viptv_core::native_torrent::NativeTorrentGrant),
    Runtime(viptv_core::torrent_runtime::TorrentRuntimeGrant),
}
impl NativeGrant {
    fn observe_time(&mut self, now: u64) {
        match self {
            Self::Legacy(g) => g.server_time = now,
            Self::Runtime(g) => g.server_time = now,
        }
    }
    fn ready(
        &self,
        id: &str,
        position: f64,
        preferences: &viptv_core::native_torrent::NativeTorrentPreferences,
    ) -> Result<String, viptv_core::CoreError> {
        match self {
            Self::Legacy(g) => {
                viptv_core::native_torrent::native_ready_response(id, position, preferences, g)
            }
            Self::Runtime(g) => {
                viptv_core::torrent_runtime::ready_response(id, position, preferences, g)
            }
        }
    }
    fn renew(
        &mut self,
        now: u64,
        expiry: u64,
        scope: &viptv_core::native_torrent_policy::NativeTorrentScope,
    ) -> Result<(), Error> {
        use viptv_core::native_torrent::NativeTorrentControlOperation::Heartbeat;
        match self {
            Self::Legacy(g) => {
                let mut next = g.clone();
                next.server_time = now;
                next.expires_at = expiry;
                viptv_core::native_torrent_policy::validate_native_transition(
                    g, &next, Heartbeat, scope, scope,
                )
                .map_err(|_| Error::Code("playback_expired"))?;
                *g = next;
            }
            Self::Runtime(g) => {
                let mut next = g.clone();
                next.server_time = now;
                next.expires_at = expiry;
                viptv_core::torrent_runtime::validate_transition(g, &next, Heartbeat)
                    .map_err(|_| Error::Code("playback_expired"))?;
                *g = next;
            }
        }
        Ok(())
    }
}
#[derive(Clone)]
struct GatewayPreparation {
    target: Arc<AuthorizedGateway>,
    client: Client,
}
struct StateData {
    native: Option<NativeState>,
    status: &'static str,
    delivery: Option<Value>,
    error: Option<&'static str>,
    touched: Instant,
    remote: Option<Remote>,
    preparing_gateway: Option<GatewayPreparation>,
}
struct Entry {
    id: String,
    scope: String,
    request_id: String,
    request_hash: [u8; 32],
    request_identity: viptv_core::native_torrent_policy::NativeTorrentRequestIdentity,
    lease: ResourceLease,
    proof: Proof,
    identity: [u8; 32],
    cancelled: AtomicBool,
    cancel_signal: tokio::sync::Notify,
    state: Mutex<StateData>,
    permit: Mutex<Option<tokio::sync::OwnedSemaphorePermit>>,
}
pub(crate) struct Registry {
    entries: Mutex<HashMap<String, Arc<Entry>>>,
    request_gate: Mutex<()>,
    negotiated: Mutex<HashSet<(String, String)>>,
    runtime_negotiated: Mutex<HashSet<(String, String)>>,
}
impl Registry {
    #[cfg(test)]
    pub(super) fn frame_authority_facts(&self, id: &str) -> (Instant, u64, bool) {
        let entries = self.entries.lock().unwrap();
        let state = entries[id].state.lock().unwrap();
        let remote = state.remote.as_ref().unwrap();
        (state.touched, remote.expires_at, remote.client_frame_ack)
    }
    pub(crate) fn new(db: Arc<Mutex<Connection>>) -> Arc<Self> {
        let registry = Arc::new(Self {
            entries: Mutex::new(HashMap::new()),
            request_gate: Mutex::new(()),
            negotiated: Mutex::new(HashSet::new()),
            runtime_negotiated: Mutex::new(HashSet::new()),
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
                let mut preparations = Vec::new();
                for (id, reason) in stale {
                    if let Some(entry) = registry.entries.lock().unwrap().get(&id).cloned() {
                        if let Some(remote) = terminate(&entry, "expired", Some(reason)) {
                            releases.push(remote);
                        }
                        preparations.push(entry);
                    }
                }
                for entry in preparations {
                    cancel_gateway_preparation(&entry).await;
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
    failure_code_ref(&error)
}
fn failure_code_ref(error: &Error) -> &'static str {
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
    if let Some(exact) = &proof.exact_vod {
        crate::kids::require_item(db, &proof.lease.principal, &proof.kind, &exact.title)?;
    }
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
    if let Some(channel) = &proof.live_channel_id {
        let exists: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM provider_live WHERE id=?1 AND provider_id=?2)",
                params![channel, id],
                |row| row.get(0),
            )
            .map_err(|_| Error::Code("provider_storage_unavailable"))?;
        if kind != "iptv" || proof.kind != "live" || !exists {
            return Err(Error::Code("source_not_found"));
        }
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
        let (
            proof,
            url,
            file_index,
            info_hash,
            trackers,
            requires_torrent_gateway,
            headers,
            live,
            provider_id,
        ) = {
            let streams = app.streams.lock().unwrap();
            let entry = streams
                .get(&id)
                .filter(|entry| entry.created.elapsed() < Duration::from_secs(1800))
                .ok_or(Error::Code("source_not_found"))?;
            (
                Proof {
                    live_channel_id: entry.live_channel_id.clone(),
                    lease: source_lease,
                    producer: entry.producer.clone(),
                    configuration: entry.configuration.ok_or(Error::Code("source_not_found"))?,
                    kind: entry.kind.clone(),
                    exact_vod: entry.exact_vod.clone(),
                },
                entry.url.clone(),
                entry.file_index,
                entry.info_hash.clone(),
                entry.discovery_trackers.clone(),
                entry.requires_torrent_gateway,
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
            json!([
                account(&proof.lease),
                url,
                headers,
                live,
                file_index,
                info_hash,
                trackers,
                proof
                    .exact_vod
                    .as_ref()
                    .map(|v| (&v.title, &v.series, v.season, v.episode))
            ])
            .to_string()
            .as_bytes(),
        )
        .into();
        Ok(Source {
            proof,
            url,
            file_index,
            info_hash,
            trackers,
            requires_torrent_gateway,
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
    let mut state = entry.state.lock().unwrap();
    if state.status == "ready" {
        if let Some(native) = state.native.as_mut() {
            let now = util::now() as u64;
            if let Some(grant) = native
                .grant
                .as_mut()
                .filter(|_| now >= native.last_server_time)
            {
                grant.observe_time(now);
                native.last_server_time = now;
                if let Ok(body) = grant.ready(&entry.id, native.position, &native.preferences) {
                    if let Ok(value) = serde_json::from_str(&body) {
                        return value;
                    }
                }
            }
            state.status = "expired";
            state.error = Some("playback_expired");
            state.delivery = None;
            state.native.as_mut().unwrap().grant = None;
            entry.cancelled.store(true, Ordering::Release);
        }
    }
    let error = state.error.map(crate::account_api::description);
    let local_expiry = util::now() as u64 + LEASE.saturating_sub(state.touched.elapsed()).as_secs();
    let expires_at = if matches!(state.status, "starting" | "ready") {
        state.native.as_ref().map_or_else(
            || {
                state
                    .remote
                    .as_ref()
                    .map_or(local_expiry, |remote| remote.expires_at.min(local_expiry))
            },
            |native| native.expires_at,
        )
    } else {
        util::now() as u64
    };
    tracing::info!(session_tag = %super::diagnostics::tag(&entry.id), status = state.status, expires_at, error_code = state.error.unwrap_or("none"), gateway_session_tag = %state.remote.as_ref().map(|remote| super::diagnostics::tag(&remote.viewer)).unwrap_or_default(), "Playback lease state");
    json!({"id":entry.id,"status":state.status,"delivery":state.delivery,"error_code":state.error,"error":error,"expires_at":expires_at,"renew_after_seconds":20})
}
fn expired(state: &StateData) -> bool {
    state.touched.elapsed() >= LEASE
        || state
            .native
            .as_ref()
            .is_some_and(|native| native.expires_at <= util::now() as u64)
        || state
            .remote
            .as_ref()
            .is_some_and(|remote| remote.expires_at <= util::now() as u64)
}
fn terminate(entry: &Entry, status: &'static str, error: Option<&'static str>) -> Option<Remote> {
    entry.cancelled.store(true, Ordering::Release);
    entry.cancel_signal.notify_one();
    entry.permit.lock().unwrap().take();
    let mut state = entry.state.lock().unwrap();
    if !matches!(state.status, "starting" | "ready") {
        return None;
    }
    tracing::warn!(session_tag = %super::diagnostics::tag(&entry.id), previous_status = state.status, terminal_status = status, error_code = error.unwrap_or("none"), "Playback session terminated");
    state.status = status;
    state.error = error;
    state.delivery = None;
    if let Some(native) = state.native.as_mut() {
        native.grant = None;
    }
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
async fn cancel_gateway_preparation(entry: &Arc<Entry>) {
    let target = entry.state.lock().unwrap().preparing_gateway.take();
    if let Some(preparation) = target {
        let target = preparation.target;
        let body = json!({"namespace":target.gateway.namespace});
        // Old gateways may lack this endpoint; their late viewer is still reconciled/released.
        let _ = preparation
            .client
            .request(
                &target.gateway.endpoint,
                target.key.expose(),
                reqwest::Method::DELETE,
                &format!("v1/preparations/{}", entry.id),
                Some(&body),
                None,
                Duration::from_secs(1),
            )
            .await;
    }
}
struct Cleanup {
    client: Client,
    target: Arc<AuthorizedGateway>,
    viewer: String,
    armed: bool,
}

async fn control_body(
    request: axum::extract::Request,
    limit: usize,
) -> Result<axum::body::Bytes, Error> {
    if request.uri().query().is_some() || request.uri().path().contains('%') {
        return Err(Error::Code("invalid_playback_request"));
    }
    if request
        .headers()
        .get_all("content-encoding")
        .iter()
        .any(|v| v != "identity")
    {
        return Err(Error::Code("invalid_playback_request"));
    }
    tokio::time::timeout(
        Duration::from_secs(10),
        axum::body::to_bytes(request.into_body(), limit),
    )
    .await
    .map_err(|_| Error::Code("invalid_playback_request"))?
    .map_err(|_| Error::Code("invalid_playback_request"))
}
fn native_scope(
    lease: &ResourceLease,
) -> Result<viptv_core::native_torrent_policy::NativeTorrentScope, Error> {
    Ok(viptv_core::native_torrent_policy::NativeTorrentScope {
        scope_key: App::scoped_key(&lease.principal),
        session_id: lease
            .session_id
            .clone()
            .ok_or(Error::Code("playback_expired"))?,
    })
}
fn core_request(request: &Start) -> Result<viptv_core::dto::PlaybackV2Request, Error> {
    fn camel(value: Value) -> Value {
        match value {
            Value::Object(values) => Value::Object(
                values
                    .into_iter()
                    .map(|(key, value)| {
                        let mut name = String::new();
                        let mut upper = false;
                        for character in key.chars() {
                            if character == '_' {
                                upper = true;
                            } else if upper {
                                name.push(character.to_ascii_uppercase());
                                upper = false;
                            } else {
                                name.push(character);
                            }
                        }
                        (name, camel(value))
                    })
                    .collect(),
            ),
            Value::Array(values) => Value::Array(values.into_iter().map(camel).collect()),
            value => value,
        }
    }
    serde_json::to_value(request)
        .and_then(|v| serde_json::from_value(camel(v)))
        .map_err(|_| Error::Code("invalid_playback_request"))
}
fn previous_request(
    app: &App,
    lease: &ResourceLease,
    scope: &str,
    request_id: &str,
    hash: &[u8; 32],
    incoming: &viptv_core::native_torrent_policy::NativeTorrentRequestIdentity,
) -> Result<Option<Arc<Entry>>, Error> {
    let _gate = app.gateway_playbacks.request_gate.lock().unwrap();
    previous_request_locked(app, lease, scope, request_id, hash, incoming)
}
fn previous_request_locked(
    app: &App,
    lease: &ResourceLease,
    scope: &str,
    request_id: &str,
    hash: &[u8; 32],
    incoming: &viptv_core::native_torrent_policy::NativeTorrentRequestIdentity,
) -> Result<Option<Arc<Entry>>, Error> {
    use rusqlite::OptionalExtension;
    let session = lease
        .session_id
        .as_ref()
        .ok_or(Error::Code("playback_expired"))?;
    let db = app.db.lock().unwrap();
    lease.validate(&db)?;
    let record = db.query_row("SELECT scope,request_hash,playback_id,cancelled FROM playback_request_authority WHERE session_id=?1 AND request_id=?2",
        params![session,request_id], |r| Ok((r.get::<_,String>(0)?,r.get::<_,Option<Vec<u8>>>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,bool>(3)?)))
        .optional().map_err(|_| Error::Code("provider_storage_unavailable"))?;
    drop(db);
    let Some((previous_scope, previous_hash, playback_id, cancelled)) = record else {
        return Ok(None);
    };
    if cancelled {
        return Err(Error::Code("playback_expired"));
    }
    if previous_scope != scope || previous_hash.as_deref() != Some(hash.as_slice()) {
        return Err(Error::Code("playback_conflict"));
    }
    let entry = playback_id.and_then(|id| {
        app.gateway_playbacks
            .entries
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
    });
    if let Some(entry) = &entry {
        if viptv_core::native_torrent_policy::native_request_transition(
            Some(&entry.request_identity),
            incoming,
            false,
        ) != viptv_core::native_torrent_policy::NativeTorrentRequestDecision::Idempotent
        {
            return Err(Error::Code("playback_conflict"));
        }
    }
    entry.map(Some).ok_or(Error::Code("playback_expired"))
}
fn magnet_hash(source: &Source) -> Option<String> {
    let url = url::Url::parse(&source.url).ok()?;
    if url.scheme() != "magnet" {
        return None;
    }
    let mut hashes = url
        .query_pairs()
        .filter(|(k, _)| k == "xt")
        .map(|(_, v)| v.into_owned());
    let hash = hashes
        .next()?
        .strip_prefix("urn:btih:")?
        .to_ascii_lowercase();
    (hashes.next().is_none() && viptv_core::native_torrent_policy::canonical_v1_hash(&hash))
        .then_some(hash)
}
fn native_candidate(app: &App, lease: &ResourceLease, source: &Source, request: &Start) -> bool {
    let Ok(request) = core_request(request) else {
        return false;
    };
    let Ok(scope) = native_scope(lease) else {
        return false;
    };
    let runtime = request
        .client
        .native_torrent
        .as_ref()
        .is_some_and(|cap| cap.version == 2);
    let negotiated = if runtime {
        &app.gateway_playbacks.runtime_negotiated
    } else {
        &app.gateway_playbacks.negotiated
    }
    .lock()
    .unwrap()
    .contains(&(scope.scope_key.clone(), scope.session_id.clone()));
    negotiated
        && request.client.native_torrent.is_some()
        && (if runtime {
            viptv_core::torrent_runtime::request_eligible(&request)
        } else {
            matches!(
                request.client.platform,
                viptv_core::dto::PlaybackPlatform::Android
                    | viptv_core::dto::PlaybackPlatform::AndroidTv
            ) && request
                .client
                .native_torrent
                .as_ref()
                .is_some_and(|cap| cap.version == 1)
                && viptv_core::native_torrent_policy::native_request_eligible(&request)
        })
        && !source.live
        && source.provider_id.is_none()
        && source.proof.exact_vod.is_some()
        && (runtime || source.file_index.is_some())
        && source.requires_torrent_gateway
        && (magnet_hash(source).is_some()
            || url::Url::parse(&source.url).is_ok_and(|u| {
                matches!(u.scheme(), "http" | "https")
                    && u.path().to_ascii_lowercase().ends_with(".torrent")
            }))
}
async fn prepare_native(
    app: &App,
    entry: &Entry,
    source: &Source,
    request: &Start,
) -> Result<(), Error> {
    if request
        .client
        .native_torrent
        .as_ref()
        .is_some_and(|cap| cap.version == 2)
    {
        return prepare_runtime(app, entry, source).await;
    }
    use base64::Engine;
    use viptv_core::{native_torrent::*, native_torrent_policy::*};
    let index = source
        .file_index
        .ok_or(Error::Code("source_format_unsupported"))?;
    let (hash, input, size) = if let Some(hash) = magnet_hash(source) {
        let uri = format!("magnet:?xt=urn:btih:{hash}");
        (
            hash,
            NativeTorrentInput {
                kind: "magnet".into(),
                value: uri,
            },
            None,
        )
    } else {
        let bytes = super::native_fetch::fetch(&source.url, &source.headers).await?;
        let metadata = torrent_policy::metainfo::vet_native_metainfo(&bytes)
            .map_err(|_| Error::Code("native_metainfo_invalid"))?;
        metadata
            .verify_selection(source.info_hash.as_deref(), index, None)
            .map_err(|_| Error::Code("source_not_found"))?;
        let size = metadata.file_sizes()[index as usize];
        (
            metadata.info_hash_hex(),
            NativeTorrentInput {
                kind: "metainfo".into(),
                value: base64::engine::general_purpose::STANDARD.encode(metadata.canonical_bytes()),
            },
            Some(size),
        )
    };
    validate_entry(app, entry).await?;
    let request_core = core_request(request)?;
    let scope = native_scope(&entry.lease)?;
    let source_scope = native_scope(&source.proof.lease)?;
    let facts = NativeTorrentAdmissionFacts {
        caller_scope: &scope,
        source_scope: &source_scope,
        request_scope: &scope,
        request: &request_core,
        backend_policy_enabled: true,
        qualified: true,
        negotiated: app
            .gateway_playbacks
            .negotiated
            .lock()
            .unwrap()
            .contains(&(scope.scope_key.clone(), scope.session_id.clone())),
        resource_authorized: true,
        source_proof_current: true,
        exact_vod: source.proof.exact_vod.is_some(),
        info_hash: Some(&hash),
        file_index: Some(index),
    };
    if native_admission(&facts) != NativeTorrentAdmissionDecision::Native {
        return Err(Error::Code("source_not_found"));
    }
    let db = app.db.lock().unwrap();
    entry.lease.validate(&db)?;
    validate_source(&db, &entry.proof)?;
    let mut state = entry.state.lock().unwrap();
    if entry.cancelled.load(Ordering::Acquire) || expired(&state) {
        return Err(Error::Code("playback_expired"));
    }
    let native = state
        .native
        .as_mut()
        .ok_or(Error::Code("playback_expired"))?;
    let grant = NativeTorrentGrant {
        version: 1,
        network_policy: "public_dht_tcp_v1".into(),
        id: uuid::Uuid::new_v4().to_string(),
        server_time: util::now() as u64,
        expires_at: native.expires_at,
        info_hash: hash,
        file_index: index,
        input,
        expected_file_size: size,
    };
    native_ready_response(&entry.id, native.position, &native.preferences, &grant)
        .map_err(|_| Error::Code("native_metainfo_invalid"))?;
    native.last_server_time = grant.server_time;
    native.grant = Some(NativeGrant::Legacy(grant));
    state.status = "ready";
    Ok(())
}

async fn prepare_runtime(app: &App, entry: &Entry, source: &Source) -> Result<(), Error> {
    use base64::Engine;
    use viptv_core::{native_torrent::NativeTorrentInput, torrent_runtime::TorrentRuntimeGrant};
    let mut trackers = source.trackers.clone();
    let (hash, input, size) = if let Some(hash) = magnet_hash(source) {
        let input = NativeTorrentInput {
            kind: "magnet".into(),
            value: format!("magnet:?xt=urn:btih:{hash}"),
        };
        (hash, input, None)
    } else {
        let bytes = super::native_fetch::fetch(&source.url, &source.headers).await?;
        let metadata = torrent_policy::metainfo::vet_native_metainfo(&bytes)
            .map_err(|_| Error::Code("native_metainfo_invalid"))?;
        if source
            .info_hash
            .as_ref()
            .is_some_and(|hash| metadata.info_hash_hex() != *hash)
        {
            return Err(Error::Code("source_not_found"));
        }
        let size = source
            .file_index
            .map(|index| {
                metadata
                    .file_sizes()
                    .get(index as usize)
                    .copied()
                    .ok_or(Error::Code("source_not_found"))
            })
            .transpose()?;
        for tracker in metadata.trackers() {
            if !trackers.contains(tracker) {
                trackers.push(tracker.clone())
            }
        }
        let input = NativeTorrentInput {
            kind: "metainfo".into(),
            value: base64::engine::general_purpose::STANDARD.encode(metadata.canonical_bytes()),
        };
        (metadata.info_hash_hex(), input, size)
    };
    validate_entry(app, entry).await?;
    let caller = native_scope(&entry.lease)?;
    let source_scope = native_scope(&source.proof.lease)?;
    if !viptv_core::native_torrent_policy::same_native_scope(&caller, &source_scope)
        || !app
            .gateway_playbacks
            .runtime_negotiated
            .lock()
            .unwrap()
            .contains(&(caller.scope_key, caller.session_id))
    {
        return Err(Error::Code("source_not_found"));
    }
    let db = app.db.lock().unwrap();
    entry.lease.validate(&db)?;
    validate_source(&db, &entry.proof)?;
    let mut state = entry.state.lock().unwrap();
    if entry.cancelled.load(Ordering::Acquire) || expired(&state) {
        return Err(Error::Code("playback_expired"));
    }
    let native = state
        .native
        .as_mut()
        .ok_or(Error::Code("playback_expired"))?;
    let grant = TorrentRuntimeGrant {
        id: uuid::Uuid::new_v4().to_string(),
        server_time: util::now() as u64,
        expires_at: native.expires_at,
        info_hash: hash,
        file_index: source.file_index,
        archive_index: None,
        input,
        trackers,
        expected_file_size: size,
    };
    viptv_core::torrent_runtime::ready_response(
        &entry.id,
        native.position,
        &native.preferences,
        &grant,
    )
    .map_err(|_| Error::Code("native_metainfo_invalid"))?;
    native.last_server_time = grant.server_time;
    native.grant = Some(NativeGrant::Runtime(grant));
    state.status = "ready";
    Ok(())
}

pub(crate) async fn cancel_request(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(request_id): Path<String>,
    request: axum::extract::Request,
) -> Result<Json<Value>, Error> {
    control_body(request, 0).await?;
    if !protocol::identifier(&request_id) {
        return Err(Error::Code("invalid_playback_request"));
    }
    let (remote, entry) = {
        let _gate = app.gateway_playbacks.request_gate.lock().unwrap();
        let db = app.db.lock().unwrap();
        lease.validate(&db)?;
        let session = lease
            .session_id
            .as_ref()
            .ok_or(Error::Code("playback_expired"))?;
        let scope = App::scoped_key(&lease.principal);
        // Even an absent request gets a durable, session-lifetime tombstone.
        // Cancellation never evicts a live record; admission refuses quota exhaustion.
        db.execute("INSERT INTO playback_request_authority(session_id,request_id,scope,cancelled) VALUES(?1,?2,?3,1) ON CONFLICT(session_id,request_id) DO UPDATE SET cancelled=1 WHERE playback_request_authority.scope=excluded.scope",params![session,request_id,scope])
            .map_err(|_| Error::Code("provider_storage_unavailable"))?;
        let entry = app
            .gateway_playbacks
            .entries
            .lock()
            .unwrap()
            .values()
            .find(|entry| {
                entry.scope == scope
                    && entry.lease.session_id == lease.session_id
                    && entry.request_id == request_id
            })
            .cloned();
        let remote = entry
            .as_ref()
            .and_then(|entry| terminate(entry, "released", None));
        (remote, entry)
    };
    if let Some(remote) = remote {
        release_remote(&remote.client, &remote.target, &remote.viewer).await;
    }
    if let Some(entry) = entry {
        cancel_gateway_preparation(&entry).await;
    }
    Ok(Json(json!({"ok":true})))
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
    requires_torrent: bool,
    client_frame_ack: bool,
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
    let mut affinity_error = None;
    if let Some((id, revision)) = app.gateway_playbacks.affinity(actor, identity) {
        if let Some(target) = candidates
            .iter()
            .filter_map(|candidate| candidate.as_ref().ok())
            .find(|candidate| candidate.gateway.id == id && candidate.gateway.revision == revision)
        {
            // Joining an existing output does not require fresh input capacity,
            // but still requires current support, namespace and all scopes.
            match app
                .gateway_client
                .capabilities(
                    &target.gateway.endpoint,
                    target.key.expose(),
                    &target.gateway.namespace,
                )
                .await
            {
                Ok(capabilities) if !requires_torrent || capabilities.torrent => {
                    if client_frame_ack {
                        match app
                            .gateway_client
                            .torrent_startup(&target.gateway.endpoint, target.key.expose())
                            .await
                        {
                            Ok(()) => return Ok(target.clone()),
                            Err(reason) => affinity_error = Some(reason),
                        }
                    } else {
                        return Ok(target.clone());
                    }
                }
                Ok(_) => affinity_error = Some("delivery_unsupported"),
                Err(reason) => affinity_error = Some(reason),
            }
        }
    }
    let client = app.gateway_client.clone();
    let mut valid = Vec::new();
    let mut prior_error = affinity_error.unwrap_or("gateway_unavailable");
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
                    if requires_torrent && !capabilities.torrent {
                        return Err("delivery_unsupported");
                    }
                    if client_frame_ack {
                        client
                            .torrent_startup(&candidate.gateway.endpoint, candidate.key.expose())
                            .await?;
                    }
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
    if request
        .client
        .native_torrent
        .as_ref()
        .is_some_and(|native| {
            let v1 = native.version == 1
                && native.network_policy == "public_dht_tcp_v1"
                && matches!(
                    request.client.platform,
                    Platform::Android | Platform::AndroidTv
                );
            let v2 = native.version == 2
                && native.network_policy == viptv_core::torrent_runtime::NETWORK_POLICY
                && matches!(
                    request.client.platform,
                    Platform::Android | Platform::AndroidTv | Platform::Desktop
                );
            !v1 && !v2
        })
        || !protocol::identifier(&request.request_id)
        || request.stream_id.is_empty()
        || request.stream_id.len() > 128
        || !request.position.is_finite()
        || !(0.0..=604800.0).contains(&request.position)
        || !(2..=16384).contains(&request.client.max_width)
        || !(2..=16384).contains(&request.client.max_height)
        || request.audio_track.is_some_and(|index| index > 65535)
        || request.subtitle_track.is_some_and(|index| index > 65535)
        || (request.subtitles_off
            && (request.subtitle_track.is_some() || request.preferred_subtitle_language.is_some()))
        || [
            &request.audio_language,
            &request.preferred_audio_language,
            &request.preferred_subtitle_language,
        ]
        .into_iter()
        .flatten()
        .any(|language| {
            language.is_empty()
                || language.len() > 35
                || !language
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
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
    request_body: axum::extract::Request,
) -> Result<(StatusCode, Json<Value>), Error> {
    start_mode(app, lease, request_body, false).await
}
pub(crate) async fn decoder_start(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    request: axum::extract::Request,
) -> Result<(StatusCode, Json<Value>), Error> {
    start_mode(app, lease, request, true).await
}
async fn start_mode(
    app: App,
    lease: ResourceLease,
    request_body: axum::extract::Request,
    decoder_start: bool,
) -> Result<(StatusCode, Json<Value>), Error> {
    let bytes = control_body(request_body, 16384).await?;
    let mut request: Start =
        serde_json::from_slice(&bytes).map_err(|_| Error::Code("invalid_playback_request"))?;
    request.decoder_start = decoder_start;
    if request.position == 0.0 {
        request.position = 0.0;
    }
    validate(&request)?;
    let request_identity = viptv_core::native_torrent_policy::NativeTorrentRequestIdentity {
        scope: native_scope(&lease)?,
        request: core_request(&request)?,
    };
    if viptv_core::native_torrent_policy::native_request_transition(None, &request_identity, false)
        != viptv_core::native_torrent_policy::NativeTorrentRequestDecision::New
    {
        return Err(Error::Code("invalid_playback_request"));
    }
    let hash: [u8; 32] = Sha256::digest(
        serde_json::to_vec(&(&request, decoder_start))
            .map_err(|_| Error::Code("invalid_playback_request"))?,
    )
    .into();
    let scope = App::scoped_key(&lease.principal);
    if let Some(entry) = previous_request(
        &app,
        &lease,
        &scope,
        &request.request_id,
        &hash,
        &request_identity,
    )? {
        inspect_entry(&app, &entry).await?;
        return Ok((StatusCode::OK, Json(response(&entry))));
    }
    let input = source(&app, &lease, request.stream_id.clone()).await?;
    // Snapshot profile defaults only for a new admission. The idempotency hash
    // above is the caller's body, so later preference edits cannot mutate an
    // existing playback or turn a safe retry into a conflicting request.
    let database = app.db.clone();
    let preference_lease = lease.clone();
    let preferences = tokio::task::spawn_blocking(move || {
        let db = database.lock().unwrap();
        preference_lease.validate(&db)?;
        let crate::auth::Principal::Account { profile_id, .. } = preference_lease.principal;
        profile_id
            .map(|id| crate::preferences::load(&db, id))
            .transpose()
            .map(Option::unwrap_or_default)
            .map_err(Error::from)
    })
    .await
    .map_err(|_| Error::Code("provider_storage_unavailable"))??;
    request
        .preferred_audio_language
        .get_or_insert(preferences.audio_language);
    if !request.subtitles_off && request.subtitle_track.is_none() && preferences.subtitles_enabled {
        request
            .preferred_subtitle_language
            .get_or_insert(preferences.subtitle_language);
    }
    // Do not apply Preferences::cap: v2 uses actual decoder limits only.
    validate(&request)?;
    if input.live && request.position != 0.0 {
        return Err(Error::Code("invalid_playback_request"));
    }
    let native = !input.requires_torrent_gateway
        && request.client.can_play_direct
        && !matches!(request.client.platform, Platform::Roku | Platform::Vizio)
        && (!matches!(request.client.platform, Platform::Web | Platform::Webos)
            || (input.url.starts_with("https://") && input.headers.is_empty()));
    let direct = native
        && !request.force_gateway
        && request.conversion == Conversion::Auto
        && request.audio_track.is_none()
        && request.subtitle_track.is_none()
        && request.audio_language.is_none()
        && !request.subtitles_off;
    let native_candidate = native_candidate(&app, &lease, &input, &request);
    if input.requires_torrent_gateway
        && request
            .client
            .native_torrent
            .as_ref()
            .is_some_and(|cap| cap.version == 2)
        && !native_candidate
    {
        return Err(Error::Code("source_format_unsupported"));
    }
    // The legacy gateway input has no metainfo hash-binding field. A paired URL
    // and hash was previously unsupported there; never drop that exact identity
    // when force-gateway/old-client gates bypass native admission.
    if !native_candidate && input.info_hash.is_some() && !input.url.starts_with("magnet:") {
        return Err(Error::Code("source_format_unsupported"));
    }
    if !direct && !native_candidate {
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
        let _gate = app.gateway_playbacks.request_gate.lock().unwrap();
        if let Some(entry) = previous_request_locked(
            &app,
            &lease,
            &scope,
            &request.request_id,
            &hash,
            &request_identity,
        )? {
            return Ok((StatusCode::OK, Json(response(&entry))));
        }
        let mut entries = app.gateway_playbacks.entries.lock().unwrap();
        if let Some(entry) = entries.values().find(|entry| {
            entry.scope == scope
                && entry.lease.session_id == lease.session_id
                && entry.request_id == request.request_id
        }) {
            if entry.request_hash != hash {
                return Err(Error::Code("playback_conflict"));
            }
            return Ok((StatusCode::OK, Json(response(entry))));
        }
        if entries.len() >= 4096 {
            return Err(Error::Code("playback_capacity"));
        }
        if native_candidate {
            let active = entries
                .values()
                .filter(|entry| {
                    entry.lease.session_id == lease.session_id
                        && !entry.cancelled.load(Ordering::Acquire)
                        && {
                            let state = entry.state.lock().unwrap();
                            state.native.is_some()
                                && matches!(state.status, "starting" | "ready")
                                && !expired(&state)
                        }
                })
                .count();
            if active >= MAX_NATIVE_GRANTS_PER_SESSION {
                return Err(Error::Code("playback_capacity"));
            }
        }
        let id = uuid::Uuid::new_v4().to_string();
        let issued_at = u64::try_from(util::now()).map_err(|_| Error::Code("playback_expired"))?;
        let native_expiry = issued_at
            .checked_add(60)
            .filter(|value| *value <= 9007199254740)
            .ok_or(Error::Code("playback_expired"))?;
        let db = app.db.lock().unwrap();
        lease.validate(&db)?;
        validate_source(&db, &input.proof)?;
        let session = lease
            .session_id
            .as_ref()
            .ok_or(Error::Code("playback_expired"))?;
        let count: usize = db
            .query_row(
                "SELECT count(*) FROM playback_request_authority WHERE session_id=?1",
                [session],
                |r| r.get(0),
            )
            .map_err(|_| Error::Code("provider_storage_unavailable"))?;
        if count >= REQUEST_QUOTA {
            return Err(Error::Code("playback_capacity"));
        }
        db.execute("INSERT INTO playback_request_authority(session_id,request_id,scope,request_hash,playback_id) VALUES(?1,?2,?3,?4,?5)", params![session, request.request_id, scope, hash.as_slice(), id]).map_err(|_| Error::Code("provider_storage_unavailable"))?;
        let entry = Arc::new(Entry {
            id: id.clone(),
            scope,
            request_id: request.request_id.clone(),
            request_hash: hash,
            request_identity,
            lease: lease.clone(),
            proof: input.proof.clone(),
            identity: input.identity,
            cancelled: AtomicBool::new(false),
            cancel_signal: tokio::sync::Notify::new(),
            state: Mutex::new(StateData {
                native: native_candidate.then(|| NativeState {
                    expires_at: native_expiry,
                    last_server_time: issued_at,
                    grant: None,
                    position: request.position,
                    preferences: viptv_core::native_torrent::NativeTorrentPreferences {
                        audio_language: request.preferred_audio_language.clone(),
                        subtitle_language: request.preferred_subtitle_language.clone(),
                        subtitles_enabled: request.preferred_subtitle_language.is_some(),
                    },
                }),
                status: "starting",
                delivery: None,
                error: None,
                touched: Instant::now(),
                remote: None,
                preparing_gateway: None,
            }),
            permit: Mutex::new(None),
        });
        entries.insert(id, entry.clone());
        entry
    };
    let worker = entry.clone();
    tracing::info!(session_tag = %super::diagnostics::tag(&entry.id), direct, position_seconds = request.position, "Playback admission accepted");
    let trace_context = service_telemetry::current_context();
    tokio::spawn(service_telemetry::in_context(trace_context, async move {
        let observation = service_telemetry::observe(service_telemetry::Operation::PlaybackStart);
        let startup_budget = if input.requires_torrent_gateway
            || request
                .client
                .native_torrent
                .as_ref()
                .is_some_and(|cap| cap.version == 2)
        {
            120
        } else {
            45
        };
        let preparation = tokio::time::timeout(
            Duration::from_secs(startup_budget),
            prepare(
                app.clone(),
                worker.clone(),
                input,
                request,
                direct,
                native_candidate,
            ),
        );
        let outcome = if native_candidate {
            tokio::select! { _ = worker.cancel_signal.notified() => return, result = preparation => result }
        } else {
            preparation.await
        };
        let failure = match outcome {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(failure_code(error)),
            Err(_) => Some("gateway_startup_timeout"),
        };
        observation.finish(match failure {
            Some(code) => {
                service_telemetry::Outcome::Failed(service_telemetry::Failure::from_code(code))
            }
            None => service_telemetry::Outcome::Success,
        });
        if let Some(code) = failure {
            tracing::warn!(session_tag = %super::diagnostics::tag(&worker.id), error_code = code, "Playback preparation failed");
            if let Some(remote) = terminate(&worker, "failed", Some(code)) {
                release_remote(&remote.client, &remote.target, &remote.viewer).await;
            }
        }
    }));
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
    native_candidate: bool,
) -> Result<(), Error> {
    validate_entry(&app, &entry).await?;
    if native_candidate {
        return prepare_native(&app, &entry, &source, &request).await;
    }
    let delivery = if direct {
        if let Some(provider) = source.provider_id {
            let permit = app
                .providers
                .acquire_playback_for_kind(provider, &source.proof.kind)
                .await
                .map_err(ApiError::from)?;
            *entry.permit.lock().unwrap() = Some(permit);
        }
        json!({"kind":"direct","url":source.url,"headers":source.headers,"position":request.position,"live":source.live,"format":"original","preferences":{"audio_language":request.preferred_audio_language,"subtitle_language":request.preferred_subtitle_language,"subtitles_enabled":request.preferred_subtitle_language.is_some()}})
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
        let client_frame_ack = request.decoder_start
            && source.requires_torrent_gateway
            && !url::Url::parse(&source.url)
                .is_ok_and(|url| url.path().to_ascii_lowercase().ends_with(".rar"));
        let target = choose(
            app.clone(),
            entry.lease.clone(),
            source.identity,
            source.requires_torrent_gateway,
            client_frame_ack,
        )
        .await?;
        validate_entry(&app, &entry).await?;
        if source.requires_torrent_gateway {
            entry.state.lock().unwrap().preparing_gateway = Some(GatewayPreparation {
                target: target.clone(),
                client: app.gateway_client.clone(),
            });
        }
        let mut input = json!({"url":source.url,"headers":source.headers,"live":source.live});
        if let Some(file_index) = source.file_index {
            input["file_index"] = json!(file_index);
        }
        if source.requires_torrent_gateway && !source.trackers.is_empty() {
            input["trackers"] = json!(source.trackers);
        }
        let body = json!({"namespace":target.gateway.namespace,"input":input,"output":{"protocol":"hls","video_codecs":request.client.video_codecs,"audio_codecs":request.client.audio_codecs,"max_width":request.client.max_width,"max_height":request.client.max_height,"audio_track":request.audio_track,"subtitle_track":request.subtitle_track,"conversion":request.conversion,"audio_language":request.audio_language,"preferred_audio_language":request.preferred_audio_language,"preferred_subtitle_language":request.preferred_subtitle_language,"subtitles_off":request.subtitles_off},"position_seconds":request.position});
        let value = app
            .gateway_client
            .request(
                &target.gateway.endpoint,
                target.key.expose(),
                reqwest::Method::POST,
                if client_frame_ack {
                    "v2/torrent-sessions"
                } else {
                    "v1/sessions"
                },
                Some(&body),
                Some(&entry.id),
                Duration::from_secs(if source.requires_torrent_gateway {
                    120
                } else {
                    35
                }),
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
                client_frame_ack,
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
    tokio::task::spawn_blocking(move || {
        let db = app.db.lock().map_err(|_| Error::Code("provider_storage_unavailable"))?;
        let tx = db.unchecked_transaction().map_err(|_| Error::Code("provider_storage_unavailable"))?;
        entry.lease.validate(&tx)?;
        validate_source(&tx, &entry.proof)?;
        let mut state = entry.state.lock().unwrap();
        if entry.cancelled.load(Ordering::Acquire) || expired(&state) {
            return Err(Error::Code("playback_expired"));
        }
        if state.remote.as_ref().is_some_and(|remote| !gateway_current(&tx, &entry.lease, &remote.target.gateway.id, remote.target.gateway.revision)) {
            return Err(Error::Code("gateway_not_found"));
        }
        // History belongs to the admitted profile and exact provider-qualified
        // channel. Retry/renewal never repeat this write; old progress survives.
        if let Some(channel) = &entry.proof.live_channel_id {
            let crate::auth::Principal::Account { profile_id: Some(profile), .. } = entry.lease.principal else {
                return Err(ApiError(StatusCode::FORBIDDEN, crate::MSG_PROFILE_REQUIRED.into()).into());
            };
            let updated = crate::library::activity_time(&tx, profile)?;
            tx.execute("INSERT INTO progress(profile_id,id,type,name,poster,position,duration,updated_at,title_id)
                SELECT ?1,id,'live',name,logo,0,0,?3,id FROM provider_live WHERE id=?2
                ON CONFLICT(profile_id,type,id) DO UPDATE SET updated_at=excluded.updated_at",
                params![profile, channel, updated]).map_err(|_| Error::Code("provider_storage_unavailable"))?;
        }
        tx.commit().map_err(|_| Error::Code("provider_storage_unavailable"))?;
        state.delivery = Some(delivery);
    state.status = "ready";
    tracing::info!(session_tag = %super::diagnostics::tag(&entry.id), "Playback ready");
        state.touched = Instant::now();
        Ok(())
    }).await.map_err(|_| Error::Code("provider_storage_unavailable"))?
}
pub(crate) async fn get(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
    request: axum::extract::Request,
) -> Result<Json<Value>, Error> {
    control_body(request, 0).await?;
    let entry = app.gateway_playbacks.snapshot(&id, &lease)?;
    if let Err(error) = inspect_entry(&app, &entry).await {
        let native = entry.state.lock().unwrap().native.is_some();
        let code = match &error {
            Error::Code(code) => *code,
            Error::Auth(error) => error.api_error_code().unwrap_or("authorization_expired"),
        };
        if let Some(remote) = terminate(&entry, "expired", Some(code)) {
            release_remote(&remote.client, &remote.target, &remote.viewer).await;
        }
        cancel_gateway_preparation(&entry).await;
        if !native || matches!(&error, Error::Auth(_)) {
            return Err(error);
        }
    }
    Ok(Json(response(&entry)))
}
/// Advisory progress is separately versioned and cannot extend playback authority.
pub(crate) async fn progress(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
    request: axum::extract::Request,
) -> Result<Json<Value>, Error> {
    control_body(request, 0).await?;
    let entry = app.gateway_playbacks.snapshot(&id, &lease)?;
    validate_entry(&app, &entry).await?;
    let (status, target) = {
        let state = entry.state.lock().unwrap();
        (
            state.status,
            state
                .preparing_gateway
                .as_ref()
                .map(|preparation| preparation.target.clone()),
        )
    };
    let mut stage = None;
    if let Some(target) = target {
        let current = {
            let db = app.db.lock().unwrap();
            gateway_current(
                &db,
                &entry.lease,
                &target.gateway.id,
                target.gateway.revision,
            )
        };
        if !current {
            return Err(Error::Code("gateway_not_found"));
        }
        if status == "ready" {
            stage = Some("buffering");
        } else if status == "starting" {
            if let Ok(value) = app
                .gateway_client
                .request(
                    &target.gateway.endpoint,
                    target.key.expose(),
                    reqwest::Method::GET,
                    &format!("v1/preparations/{}", entry.id),
                    None,
                    None,
                    Duration::from_secs(1),
                )
                .await
            {
                stage = match value.get("stage").and_then(Value::as_str) {
                    Some("finding_peers") => Some("finding_peers"),
                    Some("fetching_metadata") => Some("fetching_metadata"),
                    Some("opening_archive") => Some("opening_archive"),
                    Some("buffering") => Some("buffering"),
                    _ => None,
                };
            }
        }
    }
    validate_entry(&app, &entry).await?;
    Ok(Json(json!({"stage":stage})))
}
/// Decoder acknowledgement never renews backend or gateway authority.
pub(crate) async fn first_frame(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
    request: axum::extract::Request,
) -> Result<Json<Value>, Error> {
    if control_body(request, 2).await?.as_ref() != b"{}" {
        return Err(Error::Code("invalid_playback_request"));
    }
    let entry = app.gateway_playbacks.snapshot(&id, &lease)?;
    validate_entry(&app, &entry).await?;
    let remote = {
        let state = entry.state.lock().unwrap();
        if state.status != "ready" || state.native.is_some() {
            return Err(Error::Code("playback_expired"));
        }
        state.remote.clone()
    };
    if let Some(remote) = remote.filter(|remote| remote.client_frame_ack) {
        let outcome = remote
            .client
            .request(
                &remote.target.gateway.endpoint,
                remote.target.key.expose(),
                reqwest::Method::POST,
                &format!("v2/torrent-sessions/{}/first-frame", remote.viewer),
                Some(&json!({})),
                None,
                Duration::from_secs(5),
            )
            .await;
        if let Err(code) = outcome {
            if let Some(remote) = terminate(&entry, "failed", Some(code)) {
                release_remote(&remote.client, &remote.target, &remote.viewer).await;
            }
            return Err(Error::Code(code));
        }
    }
    validate_entry(&app, &entry).await?;
    Ok(Json(json!({"ok":true})))
}

pub(crate) async fn renew(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
    request: axum::extract::Request,
) -> Result<Json<Value>, Error> {
    // Existing direct/gateway clients send an empty JSON object. Native v1
    // remains bodyless; no other legacy request payload is accepted.
    let body = control_body(request, 2).await?;
    let entry = app.gateway_playbacks.snapshot(&id, &lease)?;
    if !body.is_empty() && (body.as_ref() != b"{}" || entry.state.lock().unwrap().native.is_some())
    {
        return Err(Error::Code("invalid_playback_request"));
    }
    if let Err(error) = validate_entry(&app, &entry).await {
        let code = match &error {
            Error::Code(code) => *code,
            Error::Auth(error) => error.api_error_code().unwrap_or("authorization_expired"),
        };
        if let Some(remote) = terminate(&entry, "expired", Some(code)) {
            release_remote(&remote.client, &remote.target, &remote.viewer).await;
        }
        cancel_gateway_preparation(&entry).await;
        return Err(error);
    }
    if entry.state.lock().unwrap().native.is_some() {
        let renewed = (|| -> Result<(), Error> {
            let db = app.db.lock().unwrap();
            lease.validate(&db)?;
            validate_source(&db, &entry.proof)?;
            let mut state = entry.state.lock().unwrap();
            if entry.cancelled.load(Ordering::Acquire) || expired(&state) {
                return Err(Error::Code("playback_expired"));
            }
            let now = util::now() as u64;
            let native = state.native.as_mut().unwrap();
            if now < native.last_server_time {
                return Err(Error::Code("playback_expired"));
            }
            let next_expiry = now
                .checked_add(60)
                .filter(|n| *n <= 9007199254740)
                .ok_or(Error::Code("playback_expired"))?;
            if let Some(grant) = native.grant.as_mut() {
                let scope = native_scope(&lease)?;
                grant.renew(now, next_expiry, &scope)?;
            }
            native.expires_at = next_expiry;
            native.last_server_time = now;
            state.touched = Instant::now();
            Ok(())
        })();
        if let Err(error) = renewed {
            terminate(&entry, "expired", Some(failure_code_ref(&error)));
            return Err(error);
        }
        return Ok(Json(response(&entry)));
    }
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
    request: axum::extract::Request,
) -> Result<Json<Value>, Error> {
    control_body(request, 0).await?;
    let entry = match app.gateway_playbacks.snapshot(&id, &lease) {
        Ok(entry) => entry,
        Err(_) => {
            lease.validate(&app.db.lock().unwrap())?;
            return Ok(Json(json!({"ok":true})));
        }
    };
    let remote = {
        let _gate = app.gateway_playbacks.request_gate.lock().unwrap();
        let db = app.db.lock().unwrap();
        lease.validate(&db)?;
        db.execute("UPDATE playback_request_authority SET cancelled=1 WHERE session_id=?1 AND request_id=?2",params![lease.session_id,entry.request_id]).map_err(|_| Error::Code("provider_storage_unavailable"))?;
        terminate(&entry, "released", None)
    };
    if let Some(remote) = remote {
        release_remote(&remote.client, &remote.target, &remote.viewer).await;
    }
    cancel_gateway_preparation(&entry).await;
    Ok(Json(json!({"ok":true})))
}

/// Reports implemented protocol support, never playback admission or device qualification.
pub(crate) async fn support(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    request: axum::extract::Request,
) -> Result<Json<Value>, Error> {
    support_version(app, lease, request, 1).await
}

pub(crate) async fn runtime_support(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    request: axum::extract::Request,
) -> Result<Json<Value>, Error> {
    support_version(app, lease, request, 2).await
}

async fn support_version(
    app: App,
    lease: ResourceLease,
    request: axum::extract::Request,
    version: u32,
) -> Result<Json<Value>, Error> {
    control_body(request, 0).await?;
    tokio::task::spawn_blocking(move || {
        let app = app.with_lease(lease.clone());
        let db = app
            .db
            .lock()
            .map_err(|_| Error::Code("provider_storage_unavailable"))?;
        lease.validate(&db)?;
        app.require_media(&db)?;
        // Negotiation belongs to the authenticated account/profile/session scope.
        let scope = App::scoped_key(&lease.principal);
        let session = lease
            .session_id
            .clone()
            .ok_or(Error::Code("playback_expired"))?;
        let negotiated = if version == 1 {
            &app.gateway_playbacks.negotiated
        } else {
            &app.gateway_playbacks.runtime_negotiated
        };
        negotiated.lock().unwrap().insert((scope, session));
        Ok(Json(
            json!({"version":version,"native_torrent_versions":[version]}),
        ))
    })
    .await
    .map_err(|_| Error::Code("provider_storage_unavailable"))?
}

/// Bound the complete control handler and always prevent caching private results.
pub(crate) async fn control_deadline(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let path = request.uri().path();
    if !(path.starts_with("/v2/playback")
        || path.starts_with("/api/v2/playback")
        || path.ends_with("/torrent-runtime-protocol"))
    {
        return next.run(request).await;
    }
    let seconds =
        if path.ends_with("playback-protocol") || path.ends_with("torrent-runtime-protocol") {
            5
        } else {
            10
        };
    let mut response =
        match tokio::time::timeout(Duration::from_secs(seconds), next.run(request)).await {
            Ok(response) => response,
            Err(_) => Error::Code("playback_control_timeout").into_response(),
        };
    response.headers_mut().insert(
        "cache-control",
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

#[cfg(test)]
#[path = "playback_native_tests.rs"]
mod native_tests;
