use super::*;

pub(crate) type ApiResult = Result<axum::Json<Value>, ApiError>;
pub(crate) async fn blocking<T: Send + 'static>(
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
        // Interruption and capacity conditions are not client mistakes. Reporting
        // them as 400 makes the client show a generic failure, and reporting
        // capacity as 429 tells the viewer to retry something that will never
        // succeed until they stop a session.
        let status = match s.as_str() {
            "Playback capacity reached" => StatusCode::SERVICE_UNAVAILABLE,
            // Delivery refusals say this client/server pair cannot deliver this
            // source; the request itself was well formed. A 400 invites the
            // client to retry with escalating transports (each re-preparing and
            // re-probing the source), so they are terminal 406s instead.
            "Playback could not start; try forced transcoding or another stream"
            | "Playback engine unavailable"
            | "Could not inspect source video safely; try another stream" => {
                StatusCode::NOT_ACCEPTABLE
            }
            _ => StatusCode::BAD_REQUEST,
        };
        Self(status, s)
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
pub(crate) fn db_error(_: rusqlite::Error) -> ApiError {
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
    pub(crate) jobs: Arc<Mutex<HashMap<String, Arc<Job>>>>,
    pub(crate) streams: Arc<Mutex<HashMap<String, StreamEntry>>>,
    // Request-local identity travels with discovery producers; ownership is never upstream-authored.
    pub(crate) principal: Option<auth::Principal>,
    pub(crate) resource_owners: Arc<Mutex<HashMap<String, ResourceOwner>>>,
    pub(crate) lease: Option<ResourceLease>,
    pub(crate) startup_requests: Arc<Mutex<HashMap<String, session::StartupRequest>>>,
    pub(crate) family_matching_gate: Arc<Mutex<()>>,
    pub(crate) automation_life: Arc<()>,
    pub(crate) catalog_control: Arc<automation::Control>,
    pub(crate) health_control: Arc<health::Control>,
    pub(crate) guide_control: Arc<guides::Control>,
    pub(crate) live_sessions: Arc<Mutex<HashMap<String, Arc<session::LiveSession>>>>,
    pub(crate) shared_playback: Arc<session::shared::Registry>,
    pub(crate) playback_audience: Option<String>,
}
#[derive(Clone)]
pub(crate) struct ResourceLease {
    pub(crate) policy_revision: i64,
    pub(crate) principal: auth::Principal,
    // Stable database session identifier, never a bearer credential or media capability.
    pub(crate) session_id: Option<String>,
}
#[derive(Clone)]
pub(crate) struct ResourceOwner {
    pub(crate) key: String,
    pub(crate) lease: ResourceLease,
    pub(crate) created: Instant,
}
impl ResourceLease {
    /// Full lease validation in one database round trip.
    ///
    /// Session and account liveness, the role/kind binding, profile
    /// ownership and the kids policy revision were previously five separate
    /// queries behind the process-wide mutex. They are fused here because
    /// every hot path — request middleware, the shared playback worker
    /// loop and each media chunk validation — pays for this call. Failure
    /// precedence matches the previous step order: a scope failure reports
    /// the auth middleware's `Unauthorized`, a policy revision change
    /// reports `Profile policy changed`, and an unbound or mismatched
    /// session reports an expired authorization.
    pub(crate) fn validate(&self, db: &Connection) -> Result<(), ApiError> {
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
            role,
            profile_id,
            ..
        } = &self.principal;
        let outcome: String = db
            .query_row(
                "SELECT CASE
                    WHEN NOT EXISTS(SELECT 1 FROM auth_sessions s JOIN auth_accounts a ON a.id=s.account_id
                        WHERE s.id=?1 AND s.account_id=?2 AND s.profile_id IS ?3
                          AND s.refresh_expires>?4 AND a.disabled=0
                          AND ((?5='device' AND s.kind='device') OR (?5=a.role AND s.kind='browser')))
                        THEN 'scope'
                    WHEN NOT (?3 IS NULL OR EXISTS(SELECT 1 FROM profile_owners o JOIN profiles p ON p.id=o.profile_id
                        WHERE o.account_id=?2 AND o.profile_id=?3 AND p.presentation_complete=1))
                        THEN 'profile'
                    WHEN COALESCE((SELECT revision FROM kids_profiles WHERE profile_id=?3),0) <> ?6
                        THEN 'policy'
                    ELSE 'ok' END",
                params![
                    session_id,
                    account_id,
                    profile_id,
                    util::now(),
                    role,
                    self.policy_revision
                ],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        match outcome.as_str() {
            "ok" => Ok(()),
            "policy" => Err(ApiError(
                StatusCode::FORBIDDEN,
                "Profile policy changed".into(),
            )),
            "profile" => Err(ApiError(StatusCode::FORBIDDEN, "Forbidden".into())),
            _ => Err(ApiError(StatusCode::UNAUTHORIZED, "Unauthorized".into())),
        }
    }
}
pub(crate) struct Job {
    // Validated original API request kind, never an upstream stream field.
    pub(crate) kind: String,
    pub(crate) created: Instant,
    pub(crate) state: Mutex<JobState>,
    pub(crate) notify: Notify,
}
pub(crate) struct JobState {
    pub(crate) events: Vec<Value>,
    pub(crate) pending: usize,
}
pub(crate) struct StreamEntry {
    pub(crate) provider_id: Option<i64>,
    pub(crate) kind: String,
    pub(crate) live: bool,
    pub(crate) url: String,
    pub(crate) headers: HashMap<String, String>,
    pub(crate) created: Instant,
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
    pub(crate) fn identity(&self) -> auth::Principal {
        self.principal
            .clone()
            .expect("account middleware must establish request identity")
    }
    pub(crate) fn scoped_key(principal: &auth::Principal) -> String {
        let auth::Principal::Account { profile_id, .. } = principal;
        format!("{}:profile:{profile_id:?}", principal.key())
    }
    pub(crate) fn with_lease(mut self, lease: ResourceLease) -> Self {
        self.addons = self
            .addons
            .for_account(lease.principal.account_id().expect("account identity"));
        self.principal = Some(lease.principal.clone());
        self.lease = Some(lease);
        self
    }
    pub(crate) fn require_media(&self, db: &Connection) -> Result<(), ApiError> {
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
    pub(crate) fn require_profile(&self, db: &Connection, profile: i64) -> Result<(), ApiError> {
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
    pub(crate) fn request_lease(&self) -> ResourceLease {
        self.lease.clone().unwrap_or_else(|| ResourceLease {
            policy_revision: 0,
            principal: self.identity(),
            session_id: None,
        })
    }
    pub(crate) fn own_resource(&self, kind: &str, id: &str) {
        self.resource_owners.lock().unwrap().insert(
            format!("{kind}:{id}"),
            ResourceOwner {
                key: Self::scoped_key(&self.identity()),
                lease: self.request_lease(),
                created: Instant::now(),
            },
        );
    }
    pub(crate) fn resource_lease(&self, kind: &str, id: &str) -> Option<ResourceLease> {
        self.resource_owners
            .lock()
            .unwrap()
            .get(&format!("{kind}:{id}"))
            .map(|owner| owner.lease.clone())
    }
    pub(crate) fn check_resource(
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
    pub(crate) fn retain_playback_owners(
        &self,
        active: &HashSet<String>,
        snapshot_started: Instant,
    ) {
        self.resource_owners.lock().unwrap().retain(|key, owner| {
            !key.starts_with("playback:")
                // A playback created after the active-ID snapshot started may not be
                // in that snapshot. Never discard its authorization record.
                || owner.created >= snapshot_started
                || active.contains(key.trim_start_matches("playback:"))
        });
    }
    pub(crate) async fn prune_playback_owners(&self) {
        let snapshot_started = Instant::now();
        let mut active: std::collections::HashSet<String> =
            self.playback.active_ids().await.into_iter().collect();
        active.extend(self.live_sessions.lock().unwrap().keys().cloned());
        active.extend(self.shared_playback.ids());
        self.retain_playback_owners(&active, snapshot_started);
    }
    pub(crate) fn prune(&self) {
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
}
