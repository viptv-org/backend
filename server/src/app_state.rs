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
// Messages that control wire behavior. Status codes and client error codes
// are derived by matching these constants in exactly one place each; never
// compare a display message inline.
pub(crate) const MSG_PROFILE_REQUIRED: &str = "Profile selection required";
pub(crate) const MSG_PARENT_REQUIRED: &str = "Parent PIN required";
pub(crate) const MSG_PARENT_PIN_INVALID: &str = "Incorrect parent PIN";
pub(crate) const MSG_PROFILE_POLICY_CHANGED: &str = "Profile policy changed";
impl ApiError {
    /// Stable recovery reasons shared by all platform clients.
    pub(crate) fn api_error_code(&self) -> Option<&'static str> {
        match self.1.as_str() {
            "client_update_required" => Some("client_update_required"),
            MSG_PROFILE_REQUIRED => Some("profile_required"),
            MSG_PARENT_REQUIRED => Some("parent_required"),
            MSG_PARENT_PIN_INVALID => Some("parent_pin_invalid"),
            MSG_PROFILE_POLICY_CHANGED => Some("profile_policy_changed"),
            "Provider connection limit reached" => Some("provider_connection_limit"),
            _ => None,
        }
    }
}
impl From<String> for ApiError {
    fn from(s: String) -> Self {
        let status = match s.as_str() {
            "client_update_required" => StatusCode::CONFLICT,
            "Provider connection limit reached" => StatusCode::TOO_MANY_REQUESTS,
            _ => StatusCode::BAD_REQUEST,
        };
        Self(status, s)
    }
}
impl From<&str> for ApiError {
    fn from(s: &str) -> Self {
        Self::from(s.to_owned())
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let secret_code = match self.1.as_str() {
            "secret_store_not_configured" => Some("secret_store_not_configured"),
            "secret_key_unavailable" => Some("secret_key_unavailable"),
            "secret_authentication_failed" => Some("secret_authentication_failed"),
            "invalid_secret_envelope" => Some("invalid_secret_envelope"),
            "addon_encryption_required" => Some("addon_encryption_required"),
            "source_credentials_migration_required" => {
                Some("source_credentials_migration_required")
            }
            "addon_storage_unavailable" => Some("addon_storage_unavailable"),
            _ => None,
        };
        if let Some(code) = secret_code {
            return account_api::Error::Code(code).into_response();
        }
        let message = match self.api_error_code() {
            Some("client_update_required") => "Update VIPTV to use this server's current catalog and playback APIs.",
            Some("provider_connection_limit") => "This IPTV provider has reached its connection limit. Stop another stream or choose another provider.",
            _ => &self.1,
        };
        let mut body = json!({"error":message});
        if let Some(code) = self.api_error_code() {
            body["error_code"] = json!(code);
        }
        (self.0, axum::Json(body)).into_response()
    }
}

#[cfg(test)]
mod display_error_tests {
    use super::*;
    #[tokio::test]
    async fn capacity_errors_explain_recovery_without_conflating_rate_limits() {
        let response = ApiError::from("Provider connection limit reached").into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error_code"], "provider_connection_limit");
        assert!(body["error"]
            .as_str()
            .unwrap()
            .contains("Stop another stream"));
        assert_eq!(
            ApiError(StatusCode::TOO_MANY_REQUESTS, "Too many requests".into()).api_error_code(),
            None
        );
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
    pub(crate) secret_vault: Option<Arc<secret_store::Vault>>,
    pub(crate) gateway_client: gateway::client::Client,
    pub(crate) gateway_playbacks: Arc<gateway::playback::Registry>,
    pub(crate) stremio_import: Arc<stremio_import::Service>,
    pub(crate) jobs: Arc<Mutex<HashMap<String, Arc<Job>>>>,
    pub(crate) streams: Arc<Mutex<HashMap<String, StreamEntry>>>,
    // Request-local identity travels with discovery producers; ownership is never upstream-authored.
    pub(crate) principal: Option<auth::Principal>,
    pub(crate) resource_owners: Arc<Mutex<HashMap<String, ResourceOwner>>>,
    pub(crate) lease: Option<ResourceLease>,
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
                MSG_PROFILE_POLICY_CHANGED.into(),
            )),
            "profile" => Err(ApiError(StatusCode::FORBIDDEN, "Forbidden".into())),
            _ => Err(ApiError(StatusCode::UNAUTHORIZED, "Unauthorized".into())),
        }
    }
}
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ExactVod {
    pub(crate) title: String,
    pub(crate) series: Option<String>,
    pub(crate) season: Option<u32>,
    pub(crate) episode: Option<u32>,
}
pub(crate) struct Job {
    pub(crate) exact_vod: Option<ExactVod>,
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
    pub(crate) exact_vod: Option<ExactVod>,
    // Set only by exact, account-authorized raw live-channel resolution.
    pub(crate) live_channel_id: Option<String>,
    pub(crate) producer: String,
    pub(crate) configuration: Option<[u8; 32]>,
    pub(crate) provider_id: Option<i64>,
    pub(crate) kind: String,
    pub(crate) live: bool,
    pub(crate) url: String,
    pub(crate) file_index: Option<u32>,
    pub(crate) discovery_trackers: Vec<String>,
    pub(crate) info_hash: Option<String>,
    pub(crate) requires_torrent_gateway: bool,
    pub(crate) headers: HashMap<String, String>,
    pub(crate) created: Instant,
}
impl App {
    pub fn new(db: Connection, client: reqwest::Client) -> Result<Self, String> {
        let secret_vault = secret_store::Vault::from_environment()?.map(Arc::new);
        db.busy_timeout(Duration::from_secs(5))
            .map_err(|_| "Database setup failed")?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; CREATE TABLE IF NOT EXISTS profiles(id INTEGER PRIMARY KEY,name TEXT NOT NULL); CREATE TABLE IF NOT EXISTS favorites(profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,id TEXT NOT NULL,type TEXT NOT NULL,name TEXT NOT NULL,poster TEXT,PRIMARY KEY(profile_id,type,id)); CREATE TABLE IF NOT EXISTS progress(profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,id TEXT NOT NULL,type TEXT NOT NULL,name TEXT NOT NULL,poster TEXT,position REAL NOT NULL,duration REAL NOT NULL,updated_at INTEGER NOT NULL,PRIMARY KEY(profile_id,type,id));").map_err(|_|"Database initialization failed")?;
        library::init(&db).map_err(|_| "Library database migration failed")?;
        init_progress_context(&db).map_err(|_| "Progress database migration failed")?;
        preferences::init(&db).map_err(|_| "Playback preferences migration failed")?;
        continuation::init(&db).map_err(|_| "Viewing queue migration failed")?;
        provider::init(&db).map_err(|_| "Provider database initialization failed")?;
        auth::init(&db).map_err(|_| "Authentication database initialization failed")?;
        provider::v2::init(&db).map_err(|_| "Account IPTV schema initialization failed")?;
        provider::refresh_v2::prepare(&db).map_err(|_| "IPTV refresh initialization failed")?;
        gateway::playback::init(&db)
            .map_err(|_| "Playback request schema initialization failed")?;
        gateway::registry::init(&db).map_err(|_| "Gateway schema initialization failed")?;
        stremio_import::init(&db).map_err(|_| "Import schema initialization failed")?;
        let stremio_import = stremio_import::Service::new()?;
        let db = Arc::new(Mutex::new(db));
        let mut addons = Addons::new(db.clone(), client.clone())?;
        addons.vault = secret_vault.clone();
        let mut providers = ProviderService::new(db.clone(), client);
        providers.vault = secret_vault.clone();
        provider::refresh_v2::start(&providers);
        let gateway_playbacks = gateway::playback::Registry::new(db.clone());
        Ok(Self {
            db,
            addons,
            providers,
            secret_vault,
            gateway_client: Default::default(),
            gateway_playbacks,
            stremio_import,
            jobs: Default::default(),
            streams: Default::default(),
            principal: None,
            resource_owners: Default::default(),
            lease: None,
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
            return Err(ApiError(StatusCode::FORBIDDEN, MSG_PROFILE_REQUIRED.into()));
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
    #[cfg(test)]
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
