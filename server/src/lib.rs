mod activity;
pub mod addon;
pub mod auth;
mod automation;
mod continuation;
mod guides;
mod health;
mod kids;
mod library;
mod lineup;
mod live_catalog;
mod live_policy;
pub mod playback;
mod preferences;
pub mod provider;
mod service_health;
mod session;
pub mod util;
use crate::{addon::Addons, playback::PlaybackManager, provider::ProviderService};
use axum::{
    extract::{Path, Query, Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{delete, get, post},
    Extension, Router,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use session::{heartbeat, media, start_playback, stop_playback, PlaybackRequest};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    convert::Infallible,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::Notify;
use uuid::Uuid;

#[cfg(test)]
mod auth_integration_tests;
#[cfg(test)]
mod contract_tests;

type ApiResult = Result<axum::Json<Value>, ApiError>;
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(work).await.map_err(|_| {
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Database worker stopped".into(),
        )
    })?
}
#[derive(Debug)]
pub struct ApiError(pub StatusCode, pub String);
impl From<String> for ApiError {
    fn from(s: String) -> Self {
        Self(StatusCode::BAD_REQUEST, s)
    }
}
impl From<&str> for ApiError {
    fn from(s: &str) -> Self {
        Self(StatusCode::BAD_REQUEST, s.into())
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let code = match self.1.as_str() {
            "Profile selection required" => Some("profile_required"),
            "Parent PIN required" => Some("parent_required"),
            "Incorrect parent PIN" => Some("parent_pin_invalid"),
            "Profile policy changed" => Some("profile_policy_changed"),
            _ => None,
        };
        let mut body = json!({"error":self.1});
        if let Some(code) = code {
            body["error_code"] = json!(code);
        }
        (self.0, axum::Json(body)).into_response()
    }
}
fn db_error(_: rusqlite::Error) -> ApiError {
    ApiError(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Database operation failed".into(),
    )
}
#[derive(Clone)]
pub struct App {
    pub db: Arc<Mutex<Connection>>,
    pub addons: Addons,
    pub providers: ProviderService,
    pub playback: Arc<PlaybackManager>,
    jobs: Arc<Mutex<HashMap<String, Arc<Job>>>>,
    streams: Arc<Mutex<HashMap<String, StreamEntry>>>,
    // Request-local identity travels with discovery producers; ownership is never upstream-authored.
    principal: Option<auth::Principal>,
    resource_owners: Arc<Mutex<HashMap<String, ResourceOwner>>>,
    lease: Option<ResourceLease>,
    startup_requests: Arc<Mutex<HashMap<String, session::StartupRequest>>>,
    family_matching_gate: Arc<Mutex<()>>,
    automation_life: Arc<()>,
    catalog_control: Arc<automation::Control>,
    health_control: Arc<health::Control>,
    guide_control: Arc<guides::Control>,
    live_sessions: Arc<Mutex<HashMap<String, Arc<session::LiveSession>>>>,
    shared_playback: Arc<session::shared::Registry>,
    playback_audience: Option<String>,
}
#[derive(Clone)]
struct ResourceLease {
    policy_revision: i64,
    principal: auth::Principal,
    // Stable database session identifier, never a bearer credential or media capability.
    session_id: Option<String>,
}
#[derive(Clone)]
struct ResourceOwner {
    key: String,
    lease: ResourceLease,
    created: Instant,
}
fn can_access_profile(
    principal: &auth::Principal,
    db: &Connection,
    profile: i64,
) -> Result<bool, ApiError> {
    match principal.require_profile(db, profile) {
        Ok(()) => Ok(true),
        Err(error) if error.0 == StatusCode::FORBIDDEN => Ok(false),
        Err(error) => Err(error),
    }
}
impl ResourceLease {
    fn validate(&self, db: &Connection) -> Result<(), ApiError> {
        self.principal.validate_scope(db)?;
        if self.policy_revision != kids::revision(db, &self.principal)? {
            return Err(ApiError(
                StatusCode::FORBIDDEN,
                "Profile policy changed".into(),
            ));
        }
        let denied = || {
            ApiError(
                StatusCode::UNAUTHORIZED,
                "Resource authorization expired".into(),
            )
        };
        let Some(session_id) = self.session_id.as_ref() else {
            return Err(denied());
        };
        if self.principal.session_id() != Some(session_id.as_str()) {
            return Err(denied());
        }
        let auth::Principal::Account {
            account_id,
            profile_id,
            ..
        } = &self.principal;
        let active: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM auth_sessions s JOIN auth_accounts a ON a.id=s.account_id WHERE s.id=?1 AND a.id=?2 AND a.disabled=0 AND s.refresh_expires>?3 AND s.profile_id IS ?4 AND (?4 IS NULL OR EXISTS(SELECT 1 FROM profile_owners g WHERE g.account_id=a.id AND g.profile_id=?4)))",
            params![session_id,account_id,util::now(),profile_id], |r| r.get(0),
        ).map_err(db_error)?;
        if !active {
            return Err(denied());
        }
        if let Some(profile_id) = profile_id {
            if !can_access_profile(&self.principal, db, *profile_id)? {
                return Err(denied());
            }
        }
        Ok(())
    }
}
struct Job {
    // Validated original API request kind, never an upstream stream field.
    kind: String,
    created: Instant,
    state: Mutex<JobState>,
    notify: Notify,
}
struct JobState {
    events: Vec<Value>,
    pending: usize,
}
struct StreamEntry {
    provider_id: Option<i64>,
    kind: String,
    live: bool,
    url: String,
    headers: HashMap<String, String>,
    created: Instant,
}
impl App {
    pub fn new(
        db: Connection,
        client: reqwest::Client,
        playback: Arc<PlaybackManager>,
    ) -> Result<Self, String> {
        db.busy_timeout(Duration::from_secs(5))
            .map_err(|_| "Database setup failed")?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; CREATE TABLE IF NOT EXISTS profiles(id INTEGER PRIMARY KEY,name TEXT NOT NULL); CREATE TABLE IF NOT EXISTS favorites(profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,id TEXT NOT NULL,type TEXT NOT NULL,name TEXT NOT NULL,poster TEXT,PRIMARY KEY(profile_id,type,id)); CREATE TABLE IF NOT EXISTS progress(profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,id TEXT NOT NULL,type TEXT NOT NULL,name TEXT NOT NULL,poster TEXT,position REAL NOT NULL,duration REAL NOT NULL,updated_at INTEGER NOT NULL,PRIMARY KEY(profile_id,type,id));").map_err(|_|"Database initialization failed")?;
        library::init(&db).map_err(|_| "Library database migration failed")?;
        init_progress_context(&db).map_err(|_| "Progress database migration failed")?;
        preferences::init(&db).map_err(|_| "Playback preferences migration failed")?;
        continuation::init(&db).map_err(|_| "Viewing queue migration failed")?;
        provider::init(&db).map_err(|_| "Provider database initialization failed")?;
        auth::init(&db).map_err(|_| "Authentication database initialization failed")?;
        automation::init(&db).map_err(|_| "Automation database initialization failed")?;
        let db = Arc::new(Mutex::new(db));
        let addons = Addons::new(db.clone(), client.clone())?;
        let providers = ProviderService::new(db.clone(), client);
        Ok(Self {
            db,
            addons,
            providers,
            playback,
            jobs: Default::default(),
            streams: Default::default(),
            principal: None,
            resource_owners: Default::default(),
            lease: None,
            startup_requests: Default::default(),
            family_matching_gate: Default::default(),
            automation_life: Default::default(),
            catalog_control: Default::default(),
            health_control: Default::default(),
            guide_control: Default::default(),
            live_sessions: Default::default(),
            shared_playback: Default::default(),
            playback_audience: None,
        })
    }
    fn identity(&self) -> auth::Principal {
        self.principal
            .clone()
            .expect("account middleware must establish request identity")
    }
    fn scoped_key(principal: &auth::Principal) -> String {
        let auth::Principal::Account { profile_id, .. } = principal;
        format!("{}:profile:{profile_id:?}", principal.key())
    }
    fn with_lease(mut self, lease: ResourceLease) -> Self {
        self.addons = self
            .addons
            .for_account(lease.principal.account_id().expect("account identity"));
        self.principal = Some(lease.principal.clone());
        self.lease = Some(lease);
        self
    }
    fn require_media(&self, db: &Connection) -> Result<(), ApiError> {
        if matches!(
            self.identity(),
            auth::Principal::Account {
                profile_id: None,
                ..
            }
        ) {
            return Err(ApiError(
                StatusCode::FORBIDDEN,
                "Profile selection required".into(),
            ));
        }
        self.request_lease().validate(db)
    }
    fn require_profile(&self, db: &Connection, profile: i64) -> Result<(), ApiError> {
        self.require_media(db)?;
        let auth::Principal::Account { profile_id, .. } = self.identity();
        if profile_id == Some(profile) {
            Ok(())
        } else {
            Err(ApiError(
                StatusCode::FORBIDDEN,
                "Profile access denied".into(),
            ))
        }
    }
    fn request_lease(&self) -> ResourceLease {
        self.lease.clone().unwrap_or_else(|| ResourceLease {
            policy_revision: 0,
            principal: self.identity(),
            session_id: None,
        })
    }
    fn own_resource(&self, kind: &str, id: &str) {
        self.resource_owners.lock().unwrap().insert(
            format!("{kind}:{id}"),
            ResourceOwner {
                key: Self::scoped_key(&self.identity()),
                lease: self.request_lease(),
                created: Instant::now(),
            },
        );
    }
    fn resource_lease(&self, kind: &str, id: &str) -> Option<ResourceLease> {
        self.resource_owners
            .lock()
            .unwrap()
            .get(&format!("{kind}:{id}"))
            .map(|owner| owner.lease.clone())
    }
    fn check_resource(
        &self,
        principal: &auth::Principal,
        kind: &str,
        id: &str,
    ) -> Result<(), ApiError> {
        let owner = self
            .resource_owners
            .lock()
            .unwrap()
            .get(&format!("{kind}:{id}"))
            .cloned();
        if let Some(owner) = owner.filter(|owner| {
            owner.key == Self::scoped_key(principal)
                && owner.lease.session_id == self.request_lease().session_id
        }) {
            owner.lease.validate(&self.db.lock().unwrap())
        } else {
            Err(ApiError(StatusCode::NOT_FOUND, "Resource not found".into()))
        }
    }
    fn retain_playback_owners(&self, active: &HashSet<String>, snapshot_started: Instant) {
        self.resource_owners.lock().unwrap().retain(|key, owner| {
            !key.starts_with("playback:")
                // A playback created after the active-ID snapshot started may not be
                // in that snapshot. Never discard its authorization record.
                || owner.created >= snapshot_started
                || active.contains(key.trim_start_matches("playback:"))
        });
    }
    async fn prune_playback_owners(&self) {
        let snapshot_started = Instant::now();
        let mut active: std::collections::HashSet<String> =
            self.playback.active_ids().await.into_iter().collect();
        active.extend(self.live_sessions.lock().unwrap().keys().cloned());
        active.extend(self.shared_playback.ids());
        self.retain_playback_owners(&active, snapshot_started);
    }
    fn prune(&self) {
        self.jobs
            .lock()
            .unwrap()
            .retain(|_, j| j.created.elapsed() < Duration::from_secs(600));
        self.streams
            .lock()
            .unwrap()
            .retain(|_, s| s.created.elapsed() < Duration::from_secs(1800));
        self.resource_owners.lock().unwrap().retain(|key, owner| {
            if key.starts_with("job:") {
                owner.created.elapsed() < Duration::from_secs(600)
            } else if key.starts_with("stream:") {
                owner.created.elapsed() < Duration::from_secs(1800)
            } else {
                true
            }
        });
    }
    fn register(&self, source: &str, raw: Vec<Value>, kind: &str) -> (Vec<Value>, Option<String>) {
        // This label comes from the configured producer, never upstream release metadata.
        let source_name = source
            .split_once(':')
            .and_then(|(kind, id)| {
                let table = match kind {
                    "addon" => "addons",
                    "iptv" => "providers",
                    _ => return None,
                };
                let id = id.parse::<i64>().ok()?;
                self.db
                    .lock()
                    .unwrap()
                    .query_row(
                        &format!("SELECT name FROM {table} WHERE id=?1"),
                        [id],
                        |row| row.get::<_, String>(0),
                    )
                    .ok()
            })
            .unwrap_or_else(|| source.to_owned());
        let routing = match source
            .strip_prefix("iptv:")
            .and_then(|v| v.parse::<i64>().ok())
        {
            Some(id) => match provider::egress::headers(&self.db.lock().unwrap(), id) {
                Ok(h) => h,
                Err(_) => return (vec![], Some("Provider WARP route unavailable".into())),
            },
            None => HashMap::new(),
        };
        let mut out = vec![];
        let mut unsupported = 0;
        let mut entries = self.streams.lock().unwrap();
        for r in raw.into_iter().take(100) {
            let Some(url) = r["url"].as_str() else {
                unsupported += 1;
                continue;
            };
            if util::validate_url(url).is_err() {
                unsupported += 1;
                continue;
            }
            if entries.len() >= 20000 {
                break;
            }
            let id = Uuid::new_v4().to_string();
            let mut headers = HashMap::new();
            if let Some(h) = r
                .pointer("/behaviorHints/proxyHeaders/request")
                .and_then(Value::as_object)
            {
                for (k, v) in h {
                    let key = k.to_ascii_lowercase();
                    if [
                        "user-agent",
                        "referer",
                        "origin",
                        "authorization",
                        "accept",
                        "accept-language",
                        "x-requested-with",
                        "x-csrf-token",
                    ]
                    .contains(&key.as_str())
                    {
                        if let Some(v) = v.as_str() {
                            if v.len() <= 4096 && !v.chars().any(char::is_control) {
                                headers.insert(key, v.into());
                            }
                        }
                    }
                }
            }
            headers.extend(routing.clone());
            let mut public = source_card(&r, source, &id, url, &headers);
            // IDs/ownership are server-authored; upstream metadata cannot replace them.
            public["id"] = json!(id);
            public["source"] = json!(source);
            // Stable across rediscovery, without persisting expiring playback URLs.
            let identity = if public["filename"].as_str().is_some_and(|v| !v.is_empty()) {
                json!([source, public["name"], public["filename"]])
            } else {
                json!([source, public["name"], public["title"]])
            };
            if source.starts_with("addon:") || source.starts_with("iptv:") {
                public["source_addon_id"] = json!(source);
            }
            public["source_fingerprint"] = json!(format!(
                "{:x}",
                Sha256::digest(identity.to_string().as_bytes())
            ));
            public["source_name"] = json!(source_display_text(&source_name, 256, url, &headers));
            entries.insert(
                id.clone(),
                StreamEntry {
                    provider_id: source.strip_prefix("iptv:").and_then(|s| s.parse().ok()),
                    kind: kind.to_owned(),
                    live: kind == "live",
                    url: url.into(),
                    headers,
                    created: Instant::now(),
                },
            );
            self.own_resource("stream", &id);
            out.push(public);
        }
        let error=(unsupported>0).then(||format!("{unsupported} source(s) unsupported: torrent, external-player, or non-HTTP streams require an external resolver"));
        (out, error)
    }
}

fn source_display_text(
    text: &str,
    limit: usize,
    url: &str,
    headers: &HashMap<String, String>,
) -> String {
    let mut text = text.replace(url, "[link omitted]");
    // Ordinary negotiation/client-identification values are display text too:
    // e.g. Accept-Language: en must not erase en/eng or letters inside French.
    // Origin/Referer links are handled by the URL redaction below.
    for (_, value) in headers.iter().filter(|(key, value)| {
        (key.eq_ignore_ascii_case("authorization") || key.eq_ignore_ascii_case("x-csrf-token"))
            && !value.is_empty()
    }) {
        text = text.replace(value, "[private value omitted]");
        if let Some((scheme, credential)) = value.split_once(' ') {
            if (scheme.eq_ignore_ascii_case("bearer") || scheme.eq_ignore_ascii_case("basic"))
                && !credential.is_empty()
            {
                text = text.replace(credential, "[private value omitted]");
            }
        }
    }
    let text = text
        .lines()
        .map(|line| {
            line.split_whitespace()
                .map(|word| {
                    if word.contains("://") || word.to_ascii_lowercase().contains("magnet:") {
                        "[link omitted]".to_owned()
                    } else {
                        word.chars().filter(|c| !c.is_control()).collect::<String>()
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let text = text.trim();
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

fn source_card(
    raw: &Value,
    source: &str,
    id: &str,
    url: &str,
    headers: &HashMap<String, String>,
) -> Value {
    let clean = |text: &str, limit| source_display_text(text, limit, url, headers);
    let description = clean(raw["description"].as_str().unwrap_or(""), 2048);
    let title = ["title", "description"]
        .into_iter()
        .filter_map(|key| raw[key].as_str().map(str::trim))
        .find(|text| !text.is_empty())
        .unwrap_or("HTTP stream");
    let name = raw["name"]
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or(source);
    let mut card = json!({"id":id,"source":source,"name":clean(name,256),"title":clean(title,1024),"description":description,"audio_language_status":"unknown"});
    if let Some(filename) = raw
        .pointer("/behaviorHints/filename")
        .and_then(Value::as_str)
    {
        if !filename.contains("://") && !filename.contains(url) {
            let filename = filename
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or("")
                .split(['?', '#'])
                .next()
                .unwrap_or("");
            let filename = clean(filename, 512);
            if !filename.is_empty() {
                if let Some(group) = continuation::release_group(&filename) {
                    card["source_release_group"] = json!(group);
                }
                card["filename"] = json!(filename);
            }
        }
    }
    if let Some(group) = raw
        .pointer("/behaviorHints/bingeGroup")
        .and_then(Value::as_str)
    {
        let group = clean(group, 128);
        if !group.is_empty() {
            card["source_binge_group"] = json!(group);
        }
    }
    if let Some(size) = raw
        .pointer("/behaviorHints/videoSize")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0 && *n <= (1_u64 << 50))
    {
        card["size_bytes"] = json!(size);
    }
    let mut languages = Vec::new();
    for key in ["language", "languages"] {
        let values: Vec<&Value> = match &raw[key] {
            Value::String(_) => vec![&raw[key]],
            Value::Array(values) => values.iter().take(16).collect(),
            _ => vec![],
        };
        for value in values {
            if let Some(language) = value.as_str() {
                let language = language.trim();
                if !language.is_empty()
                    && language.len() <= 32
                    && language
                        .chars()
                        .all(|c| c.is_alphabetic() || matches!(c, '-' | '_' | ' '))
                {
                    let language = clean(language, 32);
                    if !language.is_empty()
                        && !languages.contains(&language)
                        && languages.len() < 16
                    {
                        languages.push(language);
                    }
                }
            }
        }
    }
    if !languages.is_empty() {
        card["reported_languages"] = json!(languages);
        card["audio_language_status"] = json!("unverified");
    }
    card
}

/// Backward-compatible application router with the optional account dashboard at `/`.
pub fn router(app: App, dashboard: Option<std::path::PathBuf>) -> Router {
    router_with_tv(app, dashboard, None)
}

/// Mount the account dashboard at `/` and the TV React bundle at `/tv` without
/// changing API/media origins or enabling cross-origin credentials.
pub fn router_with_tv(
    app: App,
    dashboard: Option<std::path::PathBuf>,
    tv_dashboard: Option<std::path::PathBuf>,
) -> Router {
    automation::start(&app);
    health::start(&app);
    guides::start(&app);
    let api = Router::new()
        .route(
            "/profiles",
            get(profiles_authenticated).post(create_profile_authenticated),
        )
        .route(
            "/profiles/:id",
            axum::routing::patch(update_profile_authenticated).delete(delete_profile_authenticated),
        )
        .route("/playback/startups/:id", delete(session::cancel_startup))
        .route("/playback/:id/recover", post(session::recover_live))
        .route(
            "/automation/catalog",
            get(automation::list).patch(automation::configure),
        )
        .route("/guides", get(guides::list).patch(guides::configure))
        .route("/guides/sources", post(guides::add_source))
        .route(
            "/guides/sources/:id",
            axum::routing::patch(guides::enable_source),
        )
        .route(
            "/guides/channels/:id",
            axum::routing::patch(guides::map_channel),
        )
        .route("/guides/channels/:id/repair", post(guides::repair_channel))
        .route("/guides/run", post(guides::run_now))
        .route("/guides/cancel", post(guides::cancel))
        .route("/activity", get(activity::list))
        .route("/activity/pause", post(activity::pause))
        .route("/activity/undo/:id", post(activity::undo))
        .route("/stream-health", get(health::list).patch(health::configure))
        .route(
            "/stream-health/:id",
            axum::routing::patch(health::override_candidate),
        )
        .route("/stream-health/:id/check", post(health::check))
        .route("/automation/catalog/run", post(automation::run))
        .route("/automation/catalog/cancel", post(automation::cancel))
        .route("/lineup", get(lineup::list).post(lineup::create))
        .route(
            "/lineup/matching",
            get(lineup::matching::list).patch(lineup::matching::configure),
        )
        .route("/lineup/matching/run", post(lineup::matching::run))
        .route(
            "/lineup/matching/groups/:id",
            axum::routing::patch(lineup::matching::group),
        )
        .route(
            "/lineup/:id/matching",
            axum::routing::patch(lineup::matching::correct),
        )
        .route("/lineup/settings", axum::routing::patch(lineup::configure))
        .route("/lineup/candidates", get(lineup::inventory))
        .route("/lineup/:id", axum::routing::patch(lineup::update))
        .route("/providers", get(providers).post(add_provider))
        .route("/providers/import", post(provider::accounts::import))
        .route("/account-pools", get(provider::pools::list))
        .route("/providers/:id/status", post(provider::pools::refresh))
        .route(
            "/account-pools/:id",
            axum::routing::patch(provider::pools::configure),
        )
        .route(
            "/providers/:id/pool",
            axum::routing::patch(provider::pools::assign),
        )
        .route(
            "/providers/:id/credentials",
            post(provider::accounts::renew),
        )
        .route(
            "/providers/:id",
            delete(delete_provider).patch(update_provider),
        )
        .route("/providers/:id/sync", post(sync_provider))
        .route(
            "/live-policy",
            get(live_policy::list).put(live_policy::update),
        )
        .route("/addons", get(addons).post(add_addon))
        .route("/addons/:id", delete(delete_addon).patch(update_addon))
        .route("/catalogs", get(catalogs))
        .route("/discover", get(discover))
        .route("/meta/:kind/:id", get(meta))
        .route("/streams", post(start_streams_authenticated))
        .route("/streams/:id", get(poll_streams_authenticated))
        .route("/streams/:id/events", get(stream_events_authenticated))
        .route("/live", get(live))
        .route("/live/categories", get(live_categories))
        .route("/guide/:id", get(guide))
        .route(
            "/profiles/:id/favorites",
            get(favorites_authenticated).put(save_favorite_authenticated),
        )
        .route("/profiles/:id/favorites/page", get(library::favorites_page))
        .route("/profiles/:id/favorites/toggle", post(library::toggle))
        .route(
            "/profiles/:id/favorites/:kind/:item",
            delete(delete_favorite_authenticated),
        )
        .route(
            "/profiles/:id/progress",
            get(progress_authenticated).put(save_progress_authenticated),
        )
        .route(
            "/profiles/:id/preferences",
            get(preferences::get).put(preferences::put),
        )
        .route(
            "/profiles/:id/approvals",
            get(kids::approvals).post(kids::approve),
        )
        .route("/parent/search", get(kids::search))
        .route("/parent/status", get(kids::status))
        .route("/parent/pin", axum::routing::put(kids::set_pin))
        .route("/parent/unlock", post(kids::unlock))
        .route(
            "/profiles/:id/kids",
            get(kids::get_policy).put(kids::set_policy),
        )
        .route(
            "/profiles/:id/progress/series",
            get(library::series_history),
        )
        .route("/profiles/:id/progress/page", get(library::history_page))
        .route(
            "/profiles/:id/progress/correct",
            axum::routing::put(library::correct),
        )
        .route("/profiles/:id/continue/page", get(continuation::page))
        .route("/profiles/:id/continue/next", post(continuation::next))
        .route(
            "/profiles/:id/continue/visibility",
            axum::routing::put(continuation::visibility),
        )
        .route(
            "/profiles/:id/continue/settings",
            get(continuation::settings).put(continuation::save_settings),
        )
        .route(
            "/profiles/:id/continue",
            get(continue_watching_authenticated),
        )
        .route("/matches", get(matches).put(override_match))
        .route("/playback", post(start_playback_authenticated))
        .route("/playback/:id", delete(stop_playback))
        .route("/playback/:id/heartbeat", post(heartbeat))
        .route("/status", get(status))
        .route("/service-health", get(service_health::list))
        .fallback(|| async { ApiError(StatusCode::NOT_FOUND, "API route not found".into()) })
        .route_layer(middleware::from_fn_with_state(
            app.clone(),
            authorize_resources,
        ))
        .merge(auth::router())
        .route_layer(middleware::from_fn_with_state(
            app.clone(),
            auth::authenticate,
        ))
        .layer(middleware::from_fn(json_errors));
    let mut r = Router::new()
        .nest("/api", api)
        .route(
            "/api/health",
            get(|| async {
                axum::Json(json!({"status":"ok","version":env!("CARGO_PKG_VERSION")}))
            }),
        )
        .route("/media/:id/:cap/:file", get(media))
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
        .with_state(app);
    r = r
        .route("/privacy", get(privacy_page))
        .route("/terms", get(terms_page));
    if let Some(path) = dashboard {
        let root_index = path.join("index.html");
        let device_index = root_index.clone();
        r = r
            .route("/", get(move || dashboard_entry(root_index.clone())))
            .route(
                "/device",
                get(move || dashboard_entry(device_index.clone())),
            )
            .fallback_service(
                tower_http::services::ServeDir::new(&path).not_found_service(
                    tower_http::services::ServeFile::new(path.join("index.html")),
                ),
            );
    }
    if let Some(path) = tv_dashboard {
        let tv_entry = path.join("index.html");
        r = r
            .route("/tv", get(move || dashboard_entry(tv_entry.clone())))
            .nest_service(
                "/tv/",
                tower_http::services::ServeDir::new(&path).fallback(
                    tower_http::services::ServeFile::new(path.join("index.html")),
                ),
            );
    }
    r
}
// Static HTML responses share one non-cacheable envelope.
fn html_page(body: impl IntoResponse) -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response()
}
async fn privacy_page() -> Response {
    html_page(
        r#"<!DOCTYPE html><html lang="en"><head><meta charset="utf-8"><title>VIPTV Privacy Policy</title><meta name="viewport" content="width=device-width,initial-scale=1"><style>body{font-family:system-ui,sans-serif;max-width:640px;margin:40px auto;padding:0 20px;line-height:1.6;color:#e0e0e0;background:#101112}h1{color:#fff}a{color:#90a0ff}</style></head><body><h1>Privacy Policy</h1><p>VIPTV is a private streaming application. We do not collect, store, or share personal information beyond what is strictly necessary to provide the service.</p><p>Account credentials are stored securely and are never shared with third parties. Playback sessions are ephemeral and not logged.</p><p>For questions, contact vynxcai@gmail.com.</p><p><a href="/">Back to VIPTV</a></p></body></html>"#,
    )
}

async fn terms_page() -> Response {
    html_page(
        r#"<!DOCTYPE html><html lang="en"><head><meta charset="utf-8"><title>VIPTV Terms of Use</title><meta name="viewport" content="width=device-width,initial-scale=1"><style>body{font-family:system-ui,sans-serif;max-width:640px;margin:40px auto;padding:0 20px;line-height:1.6;color:#e0e0e0;background:#101112}h1{color:#fff}a{color:#90a0ff}</style></head><body><h1>Terms of Use</h1><p>VIPTV is provided as-is for personal, non-commercial use. Users are responsible for their own content and streaming sources.</p><p>The service is for authorized users only. Unauthorized access or redistribution is prohibited.</p><p>We reserve the right to modify or discontinue the service at any time.</p><p><a href="/">Back to VIPTV</a></p></body></html>"#,
    )
}

async fn dashboard_entry(path: PathBuf) -> Response {
    const MAX_INDEX_BYTES: usize = 2 * 1024 * 1024;
    match tokio::fs::read(path).await {
        Ok(bytes) if bytes.len() <= MAX_INDEX_BYTES => html_page(bytes),
        Ok(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CACHE_CONTROL, "no-store")],
            "Dashboard entry exceeds 2 MiB",
        )
            .into_response(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (
            StatusCode::NOT_FOUND,
            [(header::CACHE_CONTROL, "no-store")],
            "Dashboard entry not found",
        )
            .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CACHE_CONTROL, "no-store")],
            "Dashboard entry unavailable",
        )
            .into_response(),
    }
}
async fn json_errors(req: Request, next: Next) -> Response {
    let response = next.run(req).await;
    if response.status().is_client_error()
        && !response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|s| s.starts_with("application/json"))
    {
        let status = response.status();
        return ApiError(
            status,
            status
                .canonical_reason()
                .unwrap_or("Invalid request")
                .into(),
        )
        .into_response();
    }
    response
}
fn capture_lease(
    app: &App,
    token: Option<&str>,
    principal: auth::Principal,
) -> Result<ResourceLease, ApiError> {
    // Authentication already succeeded. Resolve its credential once to a stable lease;
    // downstream media requests carry only the playback capability, not this credential.
    let token = token.ok_or_else(auth::unauthorized)?;
    let db = app.db.lock().unwrap();
    let session_id = db.query_row("SELECT id FROM auth_sessions WHERE access_hash=?1 AND account_id=?2 AND id IS ?3 AND access_expires>?4", params![format!("{:x}",Sha256::digest(token.as_bytes())), principal.account_id(), principal.session_id(), util::now()], |r| r.get::<_,String>(0)).optional().map_err(db_error)?;
    let policy_revision = kids::revision(&db, &principal)?;
    let lease = ResourceLease {
        policy_revision,
        principal,
        session_id,
    };
    lease.validate(&db)?;
    Ok(lease)
}
async fn authorize_resources(State(mut app): State<App>, mut req: Request, next: Next) -> Response {
    let Some(principal) = req.extensions().get::<auth::Principal>().cloned() else {
        return auth::unauthorized().into_response();
    };
    let credential = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::to_owned)
        .or_else(|| {
            req.headers()
                .get(header::COOKIE)
                .and_then(|value| value.to_str().ok())
                .and_then(|cookies| {
                    cookies
                        .split(';')
                        .filter_map(|value| value.trim().split_once('='))
                        .find(|(name, _)| *name == "viptv_session")
                        .map(|(_, value)| value.to_owned())
                })
        });
    let worker_app = app.clone();
    let worker_principal = principal.clone();
    let lease =
        match blocking(move || capture_lease(&worker_app, credential.as_deref(), worker_principal))
            .await
        {
            Ok(lease) => lease,
            Err(error) => return error.into_response(),
        };
    req.extensions_mut().insert(lease.clone());
    app = app.with_lease(lease);
    let worker_app = app.clone();
    let worker_principal = principal.clone();
    let path = req
        .uri()
        .path()
        .strip_prefix("/api")
        .unwrap_or(req.uri().path())
        .to_owned();
    let result = blocking(move || {
        let segments: Vec<_> = path.trim_matches('/').split('/').collect();
        let media_route = matches!(
            segments.as_slice(),
            ["catalogs", ..]
                | ["discover", ..]
                | ["meta", ..]
                | ["streams", ..]
                | ["live", ..]
                | ["guide", ..]
                | ["playback", ..]
        );
        if media_route
            && matches!(
                &worker_principal,
                auth::Principal::Account {
                    profile_id: None,
                    ..
                }
            )
        {
            return Err(ApiError(
                StatusCode::FORBIDDEN,
                "Profile selection required".into(),
            ));
        }
        match segments.as_slice() {
            ["providers", ..]
            | ["matches", ..]
            | ["lineup", ..]
            | ["account-pools", ..]
            | ["automation", ..]
            | ["health", ..]
            | ["service-health", ..]
            | ["activity", ..]
            | ["guides", ..]
            | ["live-policy", ..]
                if !worker_principal.is_owner() =>
            {
                return Err(ApiError(
                    StatusCode::FORBIDDEN,
                    "Owner access required".into(),
                ));
            }
            ["profiles", id, "favorites", ..]
            | ["profiles", id, "progress", ..]
            | ["profiles", id, "continue", ..]
            | ["profiles", id, "preferences", ..] => {
                let profile = id
                    .parse::<i64>()
                    .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "Invalid profile id".into()))?;
                let db = worker_app.db.lock().map_err(|_| {
                    ApiError(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Database unavailable".into(),
                    )
                })?;
                worker_app.require_profile(&db, profile)?;
            }
            ["streams", id, ..] => worker_app.check_resource(&worker_principal, "job", id)?,
            ["playback", "startups", ..] => {}
            ["playback", id, ..] => worker_app.check_resource(&worker_principal, "playback", id)?,
            _ => {}
        }
        Ok(())
    })
    .await;
    if let Err(error) = result {
        return error.into_response();
    }
    let policy_path = req
        .uri()
        .path()
        .strip_prefix("/api")
        .unwrap_or(req.uri().path())
        .to_owned();
    match kids::before(&app, &mut req).await {
        Ok(Some(value)) => return axum::Json(value).into_response(),
        Ok(None) => (),
        Err(error) => return error.into_response(),
    }
    let response = next.run(req).await;
    kids::after(&app, &policy_path, response).await
}

// Authenticated wrappers attach the validated request lease to handler state.
async fn poll_streams_authenticated(
    State(mut app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    path: Path<String>,
    query: Query<Cursor>,
) -> ApiResult {
    app.lease = Some(lease);
    poll_streams(State(app), path, query).await
}
async fn stream_events_authenticated(
    State(mut app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    path: Path<String>,
    query: Query<Cursor>,
) -> Result<impl IntoResponse, ApiError> {
    app.lease = Some(lease);
    stream_events(State(app), path, query).await
}
async fn profiles_authenticated(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
) -> ApiResult {
    profiles(State(app.with_lease(lease))).await
}
async fn create_profile_authenticated(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    value: axum::Json<Value>,
) -> ApiResult {
    create_profile(State(app.with_lease(lease)), value).await
}
async fn update_profile_authenticated(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    path: Path<i64>,
    value: axum::Json<Value>,
) -> ApiResult {
    update_profile(State(app.with_lease(lease)), path, value).await
}
async fn delete_profile_authenticated(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<i64>,
) -> ApiResult {
    let app = app.with_lease(lease);
    let worker = app.clone();
    blocking(move || {
        let mut db = worker.db.lock().unwrap();
        let tx = db.transaction().map_err(db_error)?;
        worker.request_lease().validate(&tx)?;
        let principal = worker.identity();
        kids::require_parent(&tx, &principal)?;
        let account = principal.account_id().ok_or("Account required")?;
        let owns: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM profile_owners WHERE account_id=?1 AND profile_id=?2)",
                params![account, id],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        if !owns {
            return Err(ApiError(
                StatusCode::FORBIDDEN,
                "Profile access denied".into(),
            ));
        }
        if auth::primary_profile(&tx, account)? == Some(id) {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "The primary profile cannot be deleted".into(),
            ));
        }
        tx.execute("DELETE FROM profiles WHERE id=?1", [id])
            .map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        Ok(())
    })
    .await?;
    // Captured leases retain the old profile identity and cannot serve media after deletion.
    // Release only affected clients; shared workers used by another profile survive.
    let resources: Vec<String> = app
        .resource_owners
        .lock()
        .unwrap()
        .iter()
        .filter_map(|(key, owner)| {
            let auth::Principal::Account { profile_id, .. } = owner.lease.principal;
            (profile_id == Some(id)).then(|| key.clone())
        })
        .collect();
    for resource in resources {
        if let Some(session) = resource.strip_prefix("playback:") {
            session::stop_owned(&app, session).await;
        }
        if let Some(job) = resource.strip_prefix("job:") {
            app.jobs.lock().unwrap().remove(job);
        }
        if let Some(stream) = resource.strip_prefix("stream:") {
            app.streams.lock().unwrap().remove(stream);
        }
        app.resource_owners.lock().unwrap().remove(&resource);
    }
    Ok(axum::Json(json!({"deleted":true})))
}
async fn favorites_authenticated(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    path: Path<i64>,
) -> ApiResult {
    favorites(State(app.with_lease(lease)), path).await
}
async fn save_favorite_authenticated(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    path: Path<i64>,
    value: axum::Json<Value>,
) -> ApiResult {
    save_favorite(State(app.with_lease(lease)), path, value).await
}
async fn delete_favorite_authenticated(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    path: Path<(i64, String, String)>,
) -> ApiResult {
    delete_favorite(State(app.with_lease(lease)), path).await
}
async fn progress_authenticated(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    path: Path<i64>,
) -> ApiResult {
    progress(State(app.with_lease(lease)), path).await
}
async fn save_progress_authenticated(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    path: Path<i64>,
    value: axum::Json<Value>,
) -> ApiResult {
    save_progress(State(app.with_lease(lease)), path, value).await
}
async fn start_streams_authenticated(
    State(mut app): State<App>,
    Extension(p): Extension<auth::Principal>,
    Extension(lease): Extension<ResourceLease>,
    value: axum::Json<Value>,
) -> ApiResult {
    app.principal = Some(p);
    app = app.with_lease(lease);
    start_streams(State(app), value).await
}
async fn start_playback_authenticated(
    State(mut app): State<App>,
    Extension(p): Extension<auth::Principal>,
    Extension(lease): Extension<ResourceLease>,
    value: axum::Json<PlaybackRequest>,
) -> ApiResult {
    app.principal = Some(p);
    app = app.with_lease(lease);
    start_playback(State(app), value).await
}
fn text<'a>(v: &'a Value, key: &str, max: usize) -> Result<&'a str, ApiError> {
    let s = v[key]
        .as_str()
        .ok_or_else(|| ApiError::from(format!("Missing {key}")))?;
    if s.trim().is_empty() || s.len() > max {
        return Err(format!("Invalid {key}").into());
    }
    Ok(s)
}
fn media_type(v: &Value) -> Result<&str, ApiError> {
    let k = text(v, "type", 16)?;
    if !["movie", "series", "live"].contains(&k) {
        return Err("Invalid media type".into());
    }
    Ok(k)
}
async fn profiles(State(a): State<App>) -> ApiResult {
    blocking(move || {
        let db = a.db.lock().unwrap();
        a.request_lease().validate(&db)?;
        let account_id = a.identity().account_id().ok_or_else(auth::unauthorized)?;
        Ok(axum::Json(json!(auth::list_profiles(&db, account_id)?)))
    })
    .await
}
async fn create_profile(State(a): State<App>, axum::Json(v): axum::Json<Value>) -> ApiResult {
    blocking(move || {
        let mut db = a.db.lock().unwrap();
        let tx = db.transaction().map_err(db_error)?;
        a.request_lease().validate(&tx)?;
        let account_id = a.identity().account_id().ok_or_else(auth::unauthorized)?;
        kids::require_parent(&tx, &a.identity())?;
        let profile = auth::create_profile(&tx, account_id, &v)?;
        tx.commit().map_err(db_error)?;
        Ok(axum::Json(profile))
    })
    .await
}
async fn update_profile(
    State(a): State<App>,
    Path(id): Path<i64>,
    axum::Json(v): axum::Json<Value>,
) -> ApiResult {
    blocking(move || {
        let mut db = a.db.lock().unwrap();
        let tx = db.transaction().map_err(db_error)?;
        a.request_lease().validate(&tx)?;
        let account_id = a.identity().account_id().ok_or_else(auth::unauthorized)?;
        kids::require_parent(&tx, &a.identity())?;
        let profile = auth::update_profile(&tx, account_id, id, &v)?;
        tx.commit().map_err(db_error)?;
        Ok(axum::Json(profile))
    })
    .await
}
async fn providers(State(a): State<App>) -> ApiResult {
    blocking(move || Ok(axum::Json(a.providers.list()?))).await
}
async fn add_provider(State(a): State<App>, axum::Json(v): axum::Json<Value>) -> ApiResult {
    blocking(move || Ok(axum::Json(a.providers.add(v)?))).await
}
async fn delete_provider(State(a): State<App>, Path(id): Path<i64>) -> ApiResult {
    blocking(move || {
        a.providers.delete(id)?;
        Ok(axum::Json(json!({"ok":true})))
    })
    .await
}
async fn update_provider(
    State(a): State<App>,
    Path(id): Path<i64>,
    axum::Json(v): axum::Json<Value>,
) -> ApiResult {
    blocking(move || {
        a.providers
            .update(id, v)
            .map(axum::Json)
            .map_err(|message| {
                if message == "Stop provider playback before changing max_connections" {
                    ApiError(StatusCode::CONFLICT, message)
                } else {
                    ApiError::from(message)
                }
            })
    })
    .await
}
async fn sync_provider(State(a): State<App>, Path(id): Path<i64>) -> ApiResult {
    Ok(axum::Json(a.providers.sync(id).await?))
}
async fn addons(State(a): State<App>, Extension(lease): Extension<ResourceLease>) -> ApiResult {
    let a = a.with_lease(lease);
    blocking(move || Ok(axum::Json(a.addons.list()?))).await
}
async fn update_addon(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<i64>,
    axum::Json(v): axum::Json<Value>,
) -> ApiResult {
    let a = a.with_lease(lease);
    blocking(move || Ok(axum::Json(a.addons.update(id, v)?))).await
}
async fn add_addon(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    axum::Json(v): axum::Json<Value>,
) -> ApiResult {
    let a = a.with_lease(lease);
    Ok(axum::Json(
        a.addons.add(text(&v, "manifest_url", 4096)?).await?,
    ))
}
async fn delete_addon(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<i64>,
) -> ApiResult {
    let a = a.with_lease(lease);
    blocking(move || {
        a.addons.delete(id)?;
        Ok(axum::Json(json!({"ok":true})))
    })
    .await
}
async fn catalogs(State(a): State<App>, Extension(lease): Extension<ResourceLease>) -> ApiResult {
    let a = a.with_lease(lease);
    blocking(move || Ok(axum::Json(a.addons.catalogs()?))).await
}
#[derive(Deserialize)]
struct Discover {
    #[serde(rename = "type", default = "movie")]
    kind: String,
    catalog: Option<String>,
    addon_id: Option<i64>,
    #[serde(default)]
    skip: usize,
    search: Option<String>,
    genre: Option<String>,
    extras: Option<String>,
}
fn movie() -> String {
    "movie".into()
}
async fn discover(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Query(q): Query<Discover>,
) -> ApiResult {
    let a = a.with_lease(lease);
    if q.search.as_ref().is_some_and(|s| s.chars().count() > 256) {
        return Err("Search too long".into());
    }
    if q.genre.as_ref().is_some_and(|s| s.chars().count() > 128) {
        return Err("Genre too long".into());
    }
    let extras = match q.extras {
        Some(value) if value.len() <= 8192 => {
            serde_json::from_str::<HashMap<String, String>>(&value)
                .map_err(|_| ApiError::from("Invalid catalog options"))?
        }
        Some(_) => return Err("Catalog options too large".into()),
        None => HashMap::new(),
    };
    Ok(axum::Json(
        a.addons
            .discover_with_options(addon::DiscoverOptions {
                kind: q.kind,
                catalog: q.catalog,
                addon: q.addon_id,
                skip: q.skip,
                search: q.search,
                genre: q.genre,
                extras,
            })
            .await?,
    ))
}
async fn meta(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path((kind, id)): Path<(String, String)>,
) -> ApiResult {
    let a = a.with_lease(lease);
    if id.len() > 512 || !["movie", "series"].contains(&kind.as_str()) {
        return Err("Invalid metadata request".into());
    }
    Ok(axum::Json(a.addons.meta(&kind, &id).await?))
}
async fn start_streams(State(a): State<App>, axum::Json(mut v): axum::Json<Value>) -> ApiResult {
    // Explicit IPTV continuation scopes discovery, without changing ordinary source browsing.
    let only_provider = match v.get("only_provider_id") {
        None => None,
        Some(value) => Some(
            value
                .as_i64()
                .filter(|id| *id > 0)
                .ok_or("Invalid provider scope")?,
        ),
    };
    let only_addons = match v.get("only_addons") {
        None => false,
        Some(value) => value.as_bool().ok_or("Invalid addon scope")?,
    };
    if only_addons && only_provider.is_some() {
        return Err("Conflicting discovery scopes".into());
    }
    let context = matching_context(&v)?;
    v.as_object_mut()
        .ok_or("Invalid stream request")?
        .extend(context.as_object().unwrap().clone());
    let kind = media_type(&v)?.to_string();
    let id = text(&v, "id", 512)?.to_string();
    a.prune();
    let addons = a.addons.clone();
    let sources = blocking(move || Ok(addons.entries()?))
        .await?
        .into_iter()
        .filter(|(_, _, m)| only_provider.is_none() && addon::supports(m, "stream", &kind, &id))
        .take(32)
        .collect::<Vec<_>>();
    a.require_media(&a.db.lock().unwrap())?;
    let job = Arc::new(Job {
        kind: kind.clone(),
        created: Instant::now(),
        state: Mutex::new(JobState {
            events: vec![],
            pending: sources.len() + usize::from(!only_addons),
        }),
        notify: Notify::new(),
    });
    let jid = Uuid::new_v4().to_string();
    {
        let mut jobs = a.jobs.lock().unwrap();
        if jobs.len() >= 256 {
            return Err(ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many discovery jobs; retry later".into(),
            ));
        }
        jobs.insert(jid.clone(), job.clone());
        a.own_resource("job", &jid);
    }
    for (aid, u, _) in sources {
        let a = a.clone();
        let j = job.clone();
        let kind = kind.clone();
        let id = id.clone();
        tokio::spawn(async move {
            let source = format!("addon:{aid}");
            let r = tokio::time::timeout(Duration::from_secs(30), a.addons.streams(&u, &kind, &id))
                .await
                .unwrap_or_else(|_| Err("Addon timed out".into()));
            emit(&a, &j, &source, r);
        });
    }
    if !only_addons {
        tokio::spawn(async move {
            enrich_matching(&mut v, &Value::Null);
            if only_provider.is_none()
                && kind != "live"
                && (blank_name(&v)
                    || v["year"].is_null()
                    || v["imdb_id"].is_null()
                    || v["tmdb_id"].is_null())
            {
                let base = enrichment_id(&v, &kind, &id);
                if let Ok(Ok(m)) =
                    tokio::time::timeout(Duration::from_secs(8), a.addons.meta(&kind, &base)).await
                {
                    enrich_matching(&mut v, &m["meta"]);
                }
            }
            let r = a
                .providers
                .stream_batches(v, |source, batch| {
                    emit_batch(&a, &job, &source, batch, false);
                })
                .await;
            // One pending token represents the entire IPTV producer, not each batch.
            // Final completion never discards already registered sources.
            emit(&a, &job, "iptv", r.map(|_| Vec::new()));
        });
    }
    Ok(axum::Json(json!({"id":jid})))
}
fn emit(a: &App, j: &Job, source: &str, result: Result<Vec<Value>, String>) {
    emit_batch(a, j, source, result, true);
}
fn emit_batch(a: &App, j: &Job, source: &str, result: Result<Vec<Value>, String>, complete: bool) {
    if a.request_lease().validate(&a.db.lock().unwrap()).is_err() {
        let mut state = j.state.lock().unwrap();
        state.events.clear();
        state.pending = 0;
        drop(state);
        j.notify.notify_waiters();
        return;
    }
    let (streams, error) = match result {
        Ok(r) => a.register(source, r, &j.kind),
        Err(e) => (vec![], Some(e)),
    };
    let mut state = j.state.lock().unwrap();
    let seq = state.events.len() + 1;
    let mut e = json!({"seq":seq,"source":source,"streams":streams});
    if let Some(error) = error {
        e["error"] = json!(error);
    }
    state.events.push(e);
    if complete {
        state.pending = state.pending.saturating_sub(1);
    }
    drop(state);
    j.notify.notify_waiters();
}
#[derive(Deserialize, Default)]
struct Cursor {
    #[serde(default)]
    after: usize,
}
fn job(a: &App, id: &str) -> Result<Arc<Job>, ApiError> {
    a.prune();
    a.jobs.lock().unwrap().get(id).cloned().ok_or(ApiError(
        StatusCode::NOT_FOUND,
        "Discovery job expired or not found".into(),
    ))
}
async fn poll_streams(
    State(a): State<App>,
    Path(id): Path<String>,
    Query(q): Query<Cursor>,
) -> ApiResult {
    let j = job(&a, &id)?;
    let lease = a.resource_lease("job", &id);
    let db = a.db.lock().unwrap();
    if let Some(lease) = lease {
        lease.validate(&db)?;
    }
    a.request_lease().validate(&db)?;
    let s = j.state.lock().unwrap();
    Ok(axum::Json(
        json!({"events":s.events.iter().skip(q.after).cloned().collect::<Vec<_>>(),"done":s.pending==0}),
    ))
}
async fn stream_events(
    State(a): State<App>,
    Path(id): Path<String>,
    Query(q): Query<Cursor>,
) -> Result<impl IntoResponse, ApiError> {
    let j = job(&a, &id)?;
    let lease = a.resource_lease("job", &id);
    let viewer = a.request_lease();
    if let Some(lease) = &lease {
        lease.validate(&a.db.lock().unwrap())?;
    }
    viewer.validate(&a.db.lock().unwrap())?;
    let stream = async_stream::stream! {
        let mut cursor = q.after;
        let started = Instant::now();
        loop {
            if viewer.validate(&a.db.lock().unwrap()).is_err() || lease.as_ref().is_some_and(|lease| lease.validate(&a.db.lock().unwrap()).is_err()) { break; }
            let notified = j.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let batch = {
                let db = a.db.lock().unwrap();
                if viewer.validate(&db).is_err() || lease.as_ref().is_some_and(|lease| lease.validate(&db).is_err()) { None } else {
                let s = j.state.lock().unwrap();
                Some((s.events.iter().skip(cursor).cloned().collect::<Vec<_>>(), s.pending == 0))
                }
            };
            let Some((events, done)) = batch else { break; };
            for e in events {
                if viewer.validate(&a.db.lock().unwrap()).is_err() || lease.as_ref().is_some_and(|lease| lease.validate(&a.db.lock().unwrap()).is_err()) { return; }
                cursor = e["seq"].as_u64().unwrap_or(0) as usize;
                yield Ok::<_, Infallible>(Event::default().event("streams").id(cursor.to_string()).data(e.to_string()));
            }
            if done {
                yield Ok(Event::default().event("done").data("{}"));
                break;
            }
            if started.elapsed() >= Duration::from_secs(45) {
                yield Ok(Event::default().event("timeout").data("{}"));
                break;
            }
            // Recheck revocation even when an upstream producer is silent.
            let _ = tokio::time::timeout(Duration::from_secs(1), notified).await;
        }
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(10))))
}
#[derive(Deserialize)]
struct LiveQuery {
    view: Option<String>,
    collection: Option<String>,
    category: Option<String>,
    search: Option<String>,
    #[serde(default)]
    offset: usize,
    #[serde(default = "hundred")]
    limit: usize,
}
fn hundred() -> usize {
    100
}
async fn live(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Query(q): Query<LiveQuery>,
) -> ApiResult {
    let a = a.with_lease(lease);
    blocking(move || {
        if q.view.as_deref() == Some("us") {
            let db = a.db.lock().unwrap();
            a.require_media(&db)?;
            let auth::Principal::Account { profile_id, .. } = a.identity();
            return Ok(axum::Json(live_catalog::browse(
                &db,
                q.category.as_deref(),
                q.search.as_deref(),
                q.collection.as_deref(),
                profile_id,
                q.offset,
                q.limit,
            )?));
        }
        Ok(axum::Json(a.providers.live(
            q.category,
            q.search,
            q.offset,
            q.limit.min(200),
        )?))
    })
    .await
}
async fn live_categories(State(a): State<App>, Query(q): Query<LiveQuery>) -> ApiResult {
    blocking(move || {
        if q.view.as_deref() == Some("us") {
            return Ok(axum::Json(live_catalog::categories(&a.db.lock().unwrap())?));
        }
        Ok(axum::Json(
            a.providers.live_categories(q.offset, q.limit.min(100))?,
        ))
    })
    .await
}
async fn guide(State(a): State<App>, Path(id): Path<String>) -> ApiResult {
    Ok(axum::Json(a.providers.guide(id).await?))
}
async fn matches(State(a): State<App>) -> ApiResult {
    blocking(move || Ok(axum::Json(a.providers.matches()?))).await
}
async fn override_match(State(a): State<App>, axum::Json(v): axum::Json<Value>) -> ApiResult {
    blocking(move || {
        a.providers.override_match(v)?;
        Ok(axum::Json(json!({"ok":true})))
    })
    .await
}
async fn favorites(State(a): State<App>, Path(id): Path<i64>) -> ApiResult {
    blocking(move || {
    let db = a.db.lock().unwrap();
    a.require_profile(&db, id)?;
    let mut q=db.prepare("SELECT id,type,name,poster FROM favorites WHERE profile_id=?1 ORDER BY name LIMIT 5000").map_err(db_error)?;
    let r=q.query_map([id],|r|Ok(json!({"id":r.get::<_,String>(0)?,"type":r.get::<_,String>(1)?,"name":r.get::<_,String>(2)?,"poster":r.get::<_,Option<String>>(3)?}))).map_err(db_error)?.collect::<Result<Vec<_>,_>>().map_err(db_error)?;
    Ok(axum::Json(json!(lineup::references(&db,r).map_err(db_error)?)))
    }).await
}
async fn save_favorite(
    State(a): State<App>,
    Path(profile): Path<i64>,
    axum::Json(v): axum::Json<Value>,
) -> ApiResult {
    blocking(move || {
    let id = text(&v, "id", 512)?;
    let kind = media_type(&v)?;
    let name = text(&v, "name", 512)?;
    let db = a.db.lock().unwrap();
    a.require_profile(&db, profile)?;
    db.execute("INSERT INTO favorites(profile_id,id,type,name,poster) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(profile_id,type,id) DO UPDATE SET name=excluded.name,poster=excluded.poster",params![profile,id,kind,name,v["poster"].as_str()]).map_err(db_error)?;
    Ok(axum::Json(json!({"ok":true})))
    }).await
}
async fn delete_favorite(
    State(a): State<App>,
    Path((profile, kind, id)): Path<(i64, String, String)>,
) -> ApiResult {
    blocking(move || {
        let db = a.db.lock().unwrap();
        a.require_profile(&db, profile)?;
        db.execute(
            "DELETE FROM favorites WHERE profile_id=?1 AND type=?2 AND (id=?3 OR (?2='live' AND id IN (SELECT live_id FROM family_aliases WHERE channel_id=?3)))",
            params![profile, kind, if kind == "live" { lineup::canonical_id(&db,&id).map_err(db_error)? } else { id }],
        )
        .map_err(db_error)?;
        Ok(axum::Json(json!({"ok":true})))
    })
    .await
}
// Additive migration: existing progress rows and old clients remain valid.
fn init_progress_context(db: &Connection) -> rusqlite::Result<()> {
    let mut query = db.prepare("PRAGMA table_info(progress)")?;
    let columns = query
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !columns.iter().any(|column| column == "context") {
        db.execute_batch("ALTER TABLE progress ADD COLUMN context TEXT NOT NULL DEFAULT '{}'")?;
    }
    Ok(())
}

const CONTEXT_FIELDS: [&str; 15] = [
    "series_id",
    "season",
    "episode",
    "year",
    "releaseInfo",
    "imdb_id",
    "tmdb_id",
    "source_addon_id",
    "source_name",
    "source_fingerprint",
    "source_binge_group",
    "source_release_group",
    "source_quality",
    "source_audio",
    "audio_language",
];

fn matching_context(value: &Value) -> Result<Value, ApiError> {
    let mut context = serde_json::Map::new();
    for key in CONTEXT_FIELDS {
        let field = &value[key];
        if field.is_null() {
            continue;
        }
        let invalid = || ApiError::from(format!("Invalid {key}"));
        let clean = match key {
            "season" | "episode" | "year" => {
                let number = field.as_i64().ok_or_else(invalid)?;
                let range = if key == "year" {
                    1870..=2200
                } else {
                    0..=100_000
                };
                if !range.contains(&number) {
                    return Err(invalid());
                }
                json!(number)
            }
            "tmdb_id" => {
                let text = if let Some(text) = field.as_str() {
                    text.trim().to_owned()
                } else if let Some(n) = field.as_u64() {
                    n.to_string()
                } else if let Some(n) = field.as_f64().filter(|n| {
                    n.is_finite() && n.fract() == 0.0 && *n > 0.0 && *n <= 2_147_483_647.0
                }) {
                    (n as u64).to_string()
                } else {
                    return Err(invalid());
                };
                let digits = text.strip_prefix("tmdb:").unwrap_or(&text);
                if digits.is_empty()
                    || digits.len() > 12
                    || !digits.bytes().all(|b| b.is_ascii_digit())
                {
                    return Err(invalid());
                }
                let number = digits.parse::<u64>().map_err(|_| invalid())?;
                if number == 0 || number > 2_147_483_647 {
                    return Err(invalid());
                }
                json!(number.to_string())
            }
            _ => {
                let text = field.as_str().ok_or_else(invalid)?.trim();
                let limit = match key {
                    "releaseInfo" | "source_addon_id" => 128,
                    "source_name" => 256,
                    "source_fingerprint" => 64,
                    _ => 512,
                };
                if text.is_empty() || text.len() > limit || text.chars().any(char::is_control) {
                    return Err(invalid());
                }
                if key == "imdb_id" && text.chars().any(char::is_whitespace) {
                    return Err(invalid());
                }
                if key == "imdb_id" {
                    let text = text.to_ascii_lowercase();
                    let digits = text
                        .strip_prefix("imdb:")
                        .unwrap_or(&text)
                        .strip_prefix("tt")
                        .ok_or_else(invalid)?;
                    if !(5..=12).contains(&digits.len())
                        || !digits.bytes().all(|b| b.is_ascii_digit())
                    {
                        return Err(invalid());
                    }
                    json!(format!("tt{digits}"))
                } else {
                    json!(text)
                }
            }
        };
        context.insert(key.to_owned(), clean);
    }
    Ok(Value::Object(context))
}

fn blank_name(value: &Value) -> bool {
    value["name"]
        .as_str()
        .is_none_or(|name| name.trim().is_empty())
}

fn enrichment_id(value: &Value, kind: &str, id: &str) -> String {
    if kind == "series" {
        if let Some(parent) = value["series_id"].as_str() {
            return parent.to_owned();
        }
        let parts = id.rsplitn(3, ':').collect::<Vec<_>>();
        if parts.len() == 3 && parts[0].parse::<u32>().is_ok() && parts[1].parse::<u32>().is_ok() {
            return parts[2].to_owned();
        }
    }
    id.to_owned()
}

fn release_year(value: &Value) -> Option<Value> {
    let year = value.as_str()?.get(..4)?.parse::<i64>().ok()?;
    (1870..=2200).contains(&year).then(|| json!(year))
}

fn enrich_matching(request: &mut Value, meta: &Value) {
    if blank_name(request) {
        if let Some(name) = meta["name"].as_str().map(str::trim).filter(|name| {
            !name.is_empty() && name.len() <= 512 && !name.chars().any(char::is_control)
        }) {
            request["name"] = json!(name);
        }
    }
    // Supplied context wins. Ignore malformed optional addon fields independently.
    if request["year"].is_null() {
        if let Some(year) = release_year(&request["releaseInfo"]) {
            request["year"] = year;
        }
    }
    for key in ["year", "releaseInfo", "imdb_id", "tmdb_id"] {
        if request[key].is_null() && !meta[key].is_null() {
            if let Ok(clean) = matching_context(&json!({key:meta[key]})) {
                request[key] = clean[key].clone();
            }
        }
    }
    if request["year"].is_null() {
        if let Some(year) = release_year(&request["releaseInfo"]) {
            request["year"] = year;
        }
    }
}

async fn progress(State(a): State<App>, Path(id): Path<i64>) -> ApiResult {
    blocking(move || {
    let db = a.db.lock().unwrap();
    a.require_profile(&db, id)?;
    let mut q=db.prepare("SELECT id,type,name,poster,position,duration,updated_at,context FROM progress WHERE profile_id=?1 ORDER BY updated_at DESC,rowid DESC LIMIT 500").map_err(db_error)?;
    let r=q.query_map([id],|r| {
        let mut item = json!({"id":r.get::<_,String>(0)?,"type":r.get::<_,String>(1)?,"name":r.get::<_,String>(2)?,"poster":r.get::<_,Option<String>>(3)?,"position":r.get::<_,f64>(4)?,"duration":r.get::<_,f64>(5)?,"updated_at":r.get::<_,i64>(6)?});
        if let Ok(raw) = serde_json::from_str::<Value>(&r.get::<_,String>(7)?) {
            item["watched"]=json!(library::watched(&item,&raw));
            if let Ok(context) = matching_context(&raw) { item.as_object_mut().unwrap().extend(context.as_object().unwrap().clone()); }
        }
        Ok(item)
    }).map_err(db_error)?.collect::<Result<Vec<_>,_>>().map_err(db_error)?;
    Ok(axum::Json(json!(lineup::references(&db,r).map_err(db_error)?)))
    }).await
}
// Summarize newest activity per title before filtering completion. An older
// unfinished episode must not resurrect a show whose newest episode is finished.
fn legacy_series_id(id: &str) -> &str {
    // Only the documented IMDb episode shape is safe to infer. Addon IDs are opaque.
    let parts: Vec<_> = id.split(':').collect();
    if parts.len() == 3
        && parts[0]
            .strip_prefix("tt")
            .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        && parts[1..]
            .iter()
            .all(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
    {
        parts[0]
    } else {
        id
    }
}
async fn continue_watching_authenticated(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    path: Path<i64>,
) -> ApiResult {
    let axum::Json(page) = continuation::page(
        State(app),
        Extension(lease),
        path,
        Query(continuation::Page::default()),
    )
    .await?;
    Ok(axum::Json(page["items"].clone()))
}
async fn save_progress(
    State(a): State<App>,
    Path(profile): Path<i64>,
    axum::Json(v): axum::Json<Value>,
) -> ApiResult {
    blocking(move || {
    let id = text(&v, "id", 512)?;
    let kind = media_type(&v)?;
    let name = text(&v, "name", 512)?;
    let p = v["position"].as_f64().ok_or("Invalid position")?;
    let d = v["duration"].as_f64().ok_or("Invalid duration")?;
    if p < 0.0 || d < 0.0 || p > 1e9 || d > 1e9 {
        return Err("Invalid playback position".into());
    }
    let context = matching_context(&v)?;
    let db = a.db.lock().unwrap();
    a.require_profile(&db, profile)?;
    db.execute("INSERT INTO progress(profile_id,id,type,name,poster,position,duration,updated_at,context,title_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT(profile_id,type,id) DO UPDATE SET name=excluded.name,poster=excluded.poster,position=excluded.position,duration=excluded.duration,updated_at=excluded.updated_at,context=json_remove(json_patch(progress.context,excluded.context),'$.watched_override'),title_id=CASE WHEN json_extract(excluded.context,'$.series_id') IS NULL AND json_extract(progress.context,'$.series_id') IS NOT NULL THEN json_extract(progress.context,'$.series_id') ELSE excluded.title_id END",params![profile,id,kind,name,v["poster"].as_str(),p,d,library::activity_time(&db,profile)?,context.to_string(),continuation::title_id(&v)]).map_err(db_error)?;
    Ok(axum::Json(json!({"ok":true})))
    }).await
}
async fn status(State(a): State<App>, Extension(lease): Extension<ResourceLease>) -> ApiResult {
    let a = a.with_lease(lease);
    a.prune_playback_owners().await;
    let db_app = a.clone();
    let (providers, addons, profiles) = blocking(move || {
        let providers = db_app
            .providers
            .list()?
            .as_array()
            .map(Vec::len)
            .unwrap_or(0);
        let addons = db_app.addons.entries()?.len();
        let profiles: i64 = db_app
            .db
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM profiles", [], |r| r.get(0))
            .map_err(db_error)?;
        Ok((providers, addons, profiles))
    })
    .await?;
    Ok(axum::Json(
        json!({"providers":providers,"addons":addons,"profiles":profiles,"active_sessions":a.playback.active_count().await,"shared_playback":session::shared::diagnostics(&a),"ffmpeg_available":a.playback.ffmpeg_available().await,"video_acceleration":a.playback.acceleration_status()}),
    ))
}
