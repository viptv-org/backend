//! Read-only source fetch, private session-bound preview, then backup-first additive merge.
//! Never log this module's request, source records, identifiers or upstream errors.
use crate::{
    account_api::{self, Error},
    *,
};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use zeroize::{Zeroize, Zeroizing};

mod addon_review;
mod backup;
mod mapper;
#[cfg(test)]
mod tests;

const TTL: i64 = 600;
const MAX_ITEMS: usize = 10_000;
const MAX_REVIEW_ROWS: usize = 20_000;
const MAX_BYTES: usize = 8_000_000;
const UNAVAILABLE: &str = "stremio_source_unavailable";
const STORAGE: &str = "stremio_storage_unavailable";

pub(crate) enum ReviewError {
    Api(Error),
    AddonVerification { failed_addon_items: Vec<String> },
}
impl From<Error> for ReviewError {
    fn from(error: Error) -> Self {
        Self::Api(error)
    }
}
impl From<&'static str> for ReviewError {
    fn from(code: &'static str) -> Self {
        Self::Api(code.into())
    }
}
impl From<ApiError> for ReviewError {
    fn from(error: ApiError) -> Self {
        Self::Api(error.into())
    }
}
impl IntoResponse for ReviewError {
    fn into_response(self) -> Response {
        match self {
            Self::Api(error) => error.into_response(),
            Self::AddonVerification { failed_addon_items } => (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error":account_api::description("stremio_addon_unavailable"),
                    "error_code":"stremio_addon_unavailable", "failed_addon_items":failed_addon_items})),
            ).into_response(),
        }
    }
}

#[derive(Default, Clone, Serialize)]
struct Summary {
    source_items: usize,
    favorites_to_add: usize,
    progress_to_add: usize,
    progress_to_update: usize,
    favorites_added: usize,
    progress_added: usize,
    progress_updated: usize,
    existing_preserved: usize,
    already_imported: usize,
    needs_review: usize,
    skipped_items: usize,
}
struct ReviewOnly {
    name: String,
    kind: String,
    id: String,
    reason: &'static str,
    item_id: String,
}
struct Preview {
    addons: Option<Vec<addon_review::SourceAddon>>,
    selected_addons: Vec<addon_review::VerifiedAddon>,
    reviewed: bool,
    review_revision: u64,
    review_generation: u64,
    review_in_progress: bool,
    source_items: Vec<Value>,
    mapping_now: i64,
    row_ids: Vec<String>,
    row_handles: HashMap<(String, String), String>,
    pending_metadata: Vec<Value>,
    review_only: Vec<ReviewOnly>,
    import_library: bool,
    import_progress: bool,
    source: String,
    candidates: Vec<mapper::Candidate>,
    snapshot: [u8; 32],
    summary: Summary,
}
struct Slot {
    account: i64,
    scope: String,
    session: Option<String>,
    revision: i64,
    profile: i64,
    expires: i64,
    preview: Option<Preview>,
    completed: Option<Summary>,
    completed_exclusions: Option<Vec<String>>,
    completed_addons: usize,
    completed_existing: usize,
    completed_review_revision: Option<u64>,
}
impl Slot {
    fn belongs(&self, app: &App, profile: i64) -> bool {
        self.profile == profile
            && self.scope == App::scoped_key(&app.identity())
            && self.session == app.request_lease().session_id
            && self.revision == app.request_lease().policy_revision
    }
}
pub(crate) struct Service {
    pending: Mutex<HashMap<String, Slot>>,
    fetches: tokio::sync::Semaphore,
    client: reqwest::Client,
    // Fixed HTTPS endpoints in production; only isolated HTTP fixtures can override these.
    #[cfg(test)]
    endpoint: Option<String>,
    #[cfg(test)]
    public_metadata_endpoint: Option<String>,
    #[cfg(test)]
    metadata_deadline: Option<Duration>,
    #[cfg(test)]
    backup_directory: Option<PathBuf>,
}
impl Service {
    pub(crate) fn new() -> Result<Arc<Self>, &'static str> {
        Ok(Arc::new(Self {
            pending: Default::default(),
            fetches: tokio::sync::Semaphore::new(4),
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(12))
                .connect_timeout(Duration::from_secs(4))
                .user_agent("VIPTV-Stremio-import/1")
                .build()
                .map_err(|_| UNAVAILABLE)?,
            #[cfg(test)]
            endpoint: None,
            #[cfg(test)]
            public_metadata_endpoint: None,
            #[cfg(test)]
            metadata_deadline: None,
            #[cfg(test)]
            backup_directory: None,
        }))
    }
    fn reserve(self: &Arc<Self>, app: &App, profile: i64) -> Result<Reservation, Error> {
        let mut pending = self.pending.lock().map_err(|_| STORAGE)?;
        pending.retain(|_, slot| slot.expires > util::now());
        let account = app.identity().account_id().ok_or_else(auth::unauthorized)?;
        if pending.len() >= 64
            || pending
                .values()
                .filter(|slot| slot.account == account)
                .count()
                >= 2
        {
            return Err("stremio_import_busy".into());
        }
        let id = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        pending.insert(
            id.clone(),
            Slot {
                account,
                scope: App::scoped_key(&app.identity()),
                session: app.request_lease().session_id,
                revision: app.request_lease().policy_revision,
                profile,
                expires: util::now() + TTL,
                preview: None,
                completed: None,
                completed_exclusions: None,
                completed_addons: 0,
                completed_existing: 0,
                completed_review_revision: None,
            },
        );
        Ok(Reservation {
            service: self.clone(),
            id,
            retained: false,
        })
    }
    async fn post(&self, path: &str, body: &impl Serialize, login: bool) -> Result<Value, Error> {
        let endpoint = format!("https://api.strem.io/api/{path}");
        #[cfg(test)]
        let endpoint = self
            .endpoint
            .as_ref()
            .map(|s| format!("{s}/{path}"))
            .unwrap_or(endpoint);
        let mut response = self
            .client
            .post(endpoint)
            .json(body)
            .send()
            .await
            .map_err(|_| UNAVAILABLE)?;
        if login && matches!(response.status().as_u16(), 401 | 403) {
            return Err("stremio_credentials_invalid".into());
        }
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|n| n > MAX_BYTES as u64)
        {
            return Err(UNAVAILABLE.into());
        }
        let mut bytes = Zeroizing::new(Vec::new());
        while let Some(chunk) = response.chunk().await.map_err(|_| UNAVAILABLE)? {
            if bytes.len() + chunk.len() > MAX_BYTES {
                return Err(UNAVAILABLE.into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let mut value: Value = serde_json::from_slice(&bytes).map_err(|_| UNAVAILABLE)?;
        if !value.get("result").is_some_and(|v| !v.is_null()) {
            return Err(if login && value.get("error").is_some() {
                "stremio_credentials_invalid"
            } else {
                UNAVAILABLE
            }
            .into());
        }
        Ok(value["result"].take())
    }
    async fn fetch(
        &self,
        credentials: &Credentials,
    ) -> Result<(String, Vec<Value>, Option<Vec<addon_review::SourceAddon>>), Error> {
        #[derive(Serialize)]
        struct Login<'a> {
            r#type: &'static str,
            email: &'a str,
            password: &'a str,
            facebook: bool,
        }
        let mut result = self
            .post(
                "login",
                &Login {
                    r#type: "Login",
                    email: &credentials.email,
                    password: &credentials.password,
                    facebook: false,
                },
                true,
            )
            .await?;
        let token = Zeroizing::new(
            result["authKey"]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 4096)
                .ok_or(UNAVAILABLE)?
                .to_owned(),
        );
        if let Value::String(s) = &mut result["authKey"] {
            s.zeroize();
        }
        let uid = result["user"]["_id"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 256)
            .ok_or(UNAVAILABLE)?;
        // Stable account identity, not the email, token or source content. Never exposed on the wire.
        let source = format!("{:x}", Sha256::digest(uid.as_bytes()));
        drop(result);
        #[derive(Serialize)]
        struct Datastore<'a> {
            #[serde(rename = "authKey")]
            auth_key: &'a str,
            collection: &'static str,
            ids: [String; 0],
            all: bool,
        }
        let result = self
            .post(
                "datastoreGet",
                &Datastore {
                    auth_key: &token,
                    collection: "libraryItem",
                    ids: [],
                    all: true,
                },
                false,
            )
            .await?;
        let items = result
            .as_array()
            .filter(|v| v.len() <= MAX_ITEMS)
            .ok_or(UNAVAILABLE)?
            .clone();
        let addons = if credentials.inspect_addons {
            let result = self
                .post(
                    "addonCollectionGet",
                    &json!({"authKey":token.as_str(),"update":false,"addFromURL":[]}),
                    false,
                )
                .await?;
            Some(addon_review::parse_collection(result)?)
        } else {
            None
        };
        Ok((source, items, addons))
    }
}
struct Reservation {
    service: Arc<Service>,
    id: String,
    retained: bool,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.retained {
            if let Ok(mut slots) = self.service.pending.lock() {
                slots.remove(&self.id);
            }
        }
    }
}

// A cancelled or failed network review leaves the last published plan available.
// A newer generation owns its own flag, so an older response cannot clear it.
struct ReviewReservation {
    service: Arc<Service>,
    id: String,
    generation: u64,
}
impl Drop for ReviewReservation {
    fn drop(&mut self) {
        if let Ok(mut slots) = self.service.pending.lock() {
            if let Some(preview) = slots
                .get_mut(&self.id)
                .and_then(|slot| slot.preview.as_mut())
            {
                if preview.review_generation == self.generation {
                    preview.review_in_progress = false;
                }
            }
        }
    }
}

pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS stremio_import_receipts(
        profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
        source_account TEXT NOT NULL, type TEXT NOT NULL, id TEXT NOT NULL,
        favorite INTEGER NOT NULL DEFAULT 0, progress_timestamp INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY(profile_id,source_account,type,id));",
    )?;
    let versioned = db
        .prepare("PRAGMA table_info(stremio_import_receipts)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|name| name == "completion_version");
    if !versioned {
        db.execute_batch("ALTER TABLE stremio_import_receipts ADD COLUMN completion_version INTEGER NOT NULL DEFAULT 0")?;
    }
    Ok(())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Credentials {
    email: String,
    password: String,
    import_library: bool,
    import_progress: bool,
    #[serde(default)]
    inspect_addons: bool,
}
impl Drop for Credentials {
    fn drop(&mut self) {
        self.email.zeroize();
        self.password.zeroize();
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Confirmation {
    confirm: bool,
    #[serde(default)]
    review_revision: Option<u64>,
    #[serde(default)]
    excluded_items: Vec<String>,
}
fn authorize(db: &Connection, app: &App, profile: i64) -> Result<(), Error> {
    app.require_profile(db, profile)?;
    if kids::restricted(db, &app.identity())? {
        return Err("stremio_restricted_profile".into());
    }
    Ok(())
}
async fn check(app: &App, profile: i64) -> Result<(), Error> {
    let worker = app.clone();
    account_api::run(app.clone(), app.request_lease(), move |db, _| {
        authorize(db, &worker, profile)
    })
    .await
}
pub(crate) async fn preview(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    body: Result<Json<Credentials>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, Error> {
    let app = app.with_lease(lease);
    check(&app, profile).await?;
    let Json(credentials) = body.map_err(|_| "stremio_invalid_request")?;
    if credentials.email.is_empty()
        || credentials.email.len() > 320
        || credentials.password.is_empty()
        || credentials.password.len() > 1024
        || !(credentials.import_library
            || credentials.import_progress
            || credentials.inspect_addons)
    {
        return Err("stremio_invalid_request".into());
    }
    let inspect_addons = credentials.inspect_addons;
    let import_library = credentials.import_library;
    let import_progress = credentials.import_progress;
    let mut reservation = app.stremio_import.reserve(&app, profile)?;
    let _permit = app
        .stremio_import
        .fetches
        .try_acquire()
        .map_err(|_| "stremio_import_busy")?;
    let (source, items, addons) = tokio::time::timeout(
        Duration::from_secs(28),
        app.stremio_import.fetch(&credentials),
    )
    .await
    .map_err(|_| UNAVAILABLE)??;
    let mapping_now = util::now();
    let (candidates, summary) = mapper::map(
        &items,
        credentials.import_library,
        credentials.import_progress,
        mapping_now,
    );
    drop(credentials);
    let pending_metadata: Vec<Value> = if inspect_addons {
        items
            .iter()
            // Verified completion candidates take priority over optional identity review.
            .filter(|item| {
                item["type"] == "series"
                    && item["state"]["watched"]
                        .as_str()
                        .is_some_and(|s| !s.is_empty())
            })
            .chain(items.iter().filter(|item| {
                !(item["type"] == "series"
                    && item["state"]["watched"]
                        .as_str()
                        .is_some_and(|s| !s.is_empty()))
            }))
            .filter(|item| {
                let kind = item["type"].as_str().unwrap_or("");
                let id = item["_id"].as_str().unwrap_or("");
                matches!(kind, "movie" | "series")
                    && id.len() <= 128
                    && (!id.starts_with("tt")
                        || (kind == "series"
                            && item["state"]["watched"]
                                .as_str()
                                .is_some_and(|s| !s.is_empty())))
                    && !id.is_empty()
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'-' | b'_'))
                    && serde_json::to_vec(item).is_ok_and(|v| v.len() <= 16_384)
            })
            .take(MAX_ITEMS)
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    let review_only = if inspect_addons {
        items
            .iter()
            .filter_map(|item| {
                let kind = item["type"].as_str()?;
                if !matches!(kind, "movie" | "series") {
                    return None;
                }
                let id = item["_id"].as_str()?;
                let name = item["name"].as_str().filter(|s| {
                    !s.trim().is_empty()
                        && s.len() <= 512
                        && !s.chars().any(char::is_control)
                        && !s.contains("://")
                        && !s.contains('@')
                })?;
                let reason = if kind == "series"
                    && item["state"]["watched"]
                        .as_str()
                        .is_some_and(|s| !s.is_empty())
                {
                    "unverified_episode_bitfield"
                } else if !id.starts_with("tt") {
                    "unmatched_identity"
                } else {
                    return None;
                };
                Some(ReviewOnly {
                    name: name.to_owned(),
                    kind: kind.to_owned(),
                    id: id.to_owned(),
                    reason,
                    item_id: Uuid::new_v4().simple().to_string(),
                })
            })
            .take(MAX_ITEMS)
            .collect()
    } else {
        Vec::new()
    };
    let worker = app.clone();
    let id = reservation.id.clone();
    let result = account_api::run(app.clone(), app.request_lease(), move |db, _| {
        authorize(db, &worker, profile)?;
        // A read transaction gives both the plan and its snapshot the same SQLite view.
        let tx = rusqlite::Transaction::new_unchecked(db, rusqlite::TransactionBehavior::Deferred).map_err(|_| STORAGE)?;
        let snapshot = snapshot(&tx, profile)?;
        let summary = merge(&tx, profile, &source, &candidates, summary, false)?;
        tx.commit().map_err(|_| STORAGE)?;
        let mut slots = worker.stremio_import.pending.lock().map_err(|_| STORAGE)?;
        let slot = slots.get_mut(&id).ok_or("stremio_preview_not_found")?;
        if slot.expires <= util::now() { return Err("stremio_preview_expired".into()); }
        let expires = slot.expires;
        let addon_rows = if inspect_addons { Some(addon_review::display(&worker, db, addons.as_ref().ok_or(UNAVAILABLE)?)?) } else { None };
        let row_ids: Vec<String> = candidates.iter().map(|_| Uuid::new_v4().simple().to_string()).collect();
        let row_handles = candidates.iter().zip(&row_ids).map(|(c, id)| ((c.kind.clone(), c.id.clone()), id.clone())).collect();
        let mut response = json!({"preview_id":id,"profile_id":profile,"expires_at":expires,"review_revision":0,"summary":summary,"warnings":[
            "Only saved IMDb movies and series, timestamped movie progress and exact IMDb episode resumes are supported.",
            "Bulk episode watched history, unsupported identities and likes/loves are not imported. Viewing events and episode dates are not reconstructed.",
            "Existing manual corrections, newer or equal progress, source context and hidden queue titles are preserved."
        ]});
        if let Some(rows) = addon_rows { response["stage"] = json!("addons"); response["addons"] = json!(rows); }
        slot.preview = Some(Preview { source, source_items: items, mapping_now, candidates, snapshot, summary, addons, selected_addons: Vec::new(), reviewed: !inspect_addons, review_revision: 0, review_generation: 0, review_in_progress: false, row_ids, row_handles, pending_metadata, review_only, import_library, import_progress });
        Ok(Json(response))
    }).await?;
    reservation.retained = true;
    Ok(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReviewSelection {
    selected_addons: Vec<String>,
    #[serde(default)]
    expected_review_revision: Option<u64>,
}

pub(crate) async fn review(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path((profile, id)): Path<(i64, String)>,
    body: Result<Json<ReviewSelection>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, ReviewError> {
    let app = app.with_lease(lease);
    check(&app, profile).await?;
    let Json(selection) = body.map_err(|_| "stremio_invalid_request")?;
    if selection.selected_addons.len() > 32
        || selection.selected_addons.iter().any(|s| s.len() > 64)
    {
        return Err("stremio_invalid_request".into());
    }
    let mut unique = std::collections::HashSet::new();
    if !selection.selected_addons.iter().all(|s| unique.insert(s)) {
        return Err("stremio_invalid_request".into());
    }
    let (selected, pending_metadata, reserved_revision) = {
        let mut slots = app.stremio_import.pending.lock().map_err(|_| STORAGE)?;
        let slot = slots
            .get_mut(&id)
            .filter(|s| s.belongs(&app, profile))
            .ok_or("stremio_preview_not_found")?;
        if slot.expires <= util::now() {
            return Err("stremio_preview_expired".into());
        }
        let preview = slot.preview.as_mut().ok_or("stremio_preview_not_found")?;
        if selection.expected_review_revision != Some(preview.review_revision)
            && !(preview.review_revision == 0 && selection.expected_review_revision.is_none())
        {
            return Err("stremio_preview_stale".into());
        }
        let addons = preview.addons.as_ref().ok_or("stremio_invalid_request")?;
        let selected = selection
            .selected_addons
            .iter()
            .map(|id| {
                let addon = addons
                    .iter()
                    .find(|a| &a.id == id)
                    .ok_or("stremio_invalid_request")?;
                Ok((
                    addon.id.clone(),
                    addon.name.clone(),
                    addon.url.clone(),
                    addon.manifest_id.clone(),
                ))
            })
            .collect::<Result<Vec<_>, Error>>()?;
        preview.review_generation = preview
            .review_generation
            .checked_add(1)
            .ok_or("stremio_preview_stale")?;
        preview.review_in_progress = true;
        (
            selected,
            preview.pending_metadata.clone(),
            preview.review_generation,
        )
    };
    let _reservation = ReviewReservation {
        service: app.stremio_import.clone(),
        id: id.clone(),
        generation: reserved_revision,
    };
    let selected: Vec<_> = selected
        .into_iter()
        .map(|(id, name, url, manifest_id)| addon_review::SourceAddon {
            id,
            name,
            resources: Vec::new(),
            manifest_id,
            url,
        })
        .collect();
    let _permit = app
        .stremio_import
        .fetches
        .try_acquire()
        .map_err(|_| "stremio_import_busy")?;
    let verified = tokio::time::timeout(
        Duration::from_secs(34),
        addon_review::verify(&app, &selected.iter().collect::<Vec<_>>()),
    )
    .await
    .map_err(|_| UNAVAILABLE)??;
    let metadata = addon_review::metadata(&app, &pending_metadata, &verified).await;
    let worker = app.clone();
    account_api::run(app.clone(), app.request_lease(), move |db, _| {
        authorize(db, &worker, profile)?;
        let mut slots = worker.stremio_import.pending.lock().map_err(|_| STORAGE)?;
        let slot = slots.get_mut(&id).filter(|s| s.belongs(&worker, profile)).ok_or("stremio_preview_not_found")?;
        if slot.expires <= util::now() { return Err("stremio_preview_expired".into()); }
        let preview = slot.preview.as_mut().ok_or("stremio_preview_not_found")?;
        if preview.review_generation != reserved_revision || !preview.review_in_progress || snapshot(db, profile)? != preview.snapshot { return Err("stremio_preview_stale".into()); }
        let (candidates, mapped_summary) = mapper::map_verified(&preview.source_items,
            preview.import_library, preview.import_progress, preview.mapping_now, &metadata);
        let summary = merge(db, profile, &preview.source, &candidates, mapped_summary, false)?;
        let mut row_handles = preview.row_handles.clone();
        let row_ids: Vec<String> = candidates.iter().map(|c| row_handles.entry((c.kind.clone(),c.id.clone()))
            .or_insert_with(|| Uuid::new_v4().simple().to_string()).clone()).collect();
        let mut rows = review_rows(db, profile, &preview.source, &candidates, &row_ids)?;
        for item in &preview.review_only {
            if rows.len() >= MAX_REVIEW_ROWS { break; }
            if candidates.iter().any(|c| c.kind == item.kind && c.progress.is_some() &&
                (c.id == item.id || c.title == item.id)) { continue; }
            if item.reason == "unmatched_identity" && candidates.iter().any(|c|
                c.kind == item.kind && (c.id == item.id || c.title == item.id)) { continue; }
            rows.push(json!({"item_id":item.item_id,"name":item.name,"type":item.kind,
                "favorite_action":"none","progress_action":"none","status":"needs_review","reason":item.reason,
                "counts":{"favorites_to_add":0,"progress_to_add":0,"progress_to_update":0,
                    "existing_preserved":0,"already_imported":0},"selectable":false}));
        }
        let count = verified.iter().filter(|a| a.expected.is_none()).count();
        let next_revision = preview.review_revision.checked_add(1).ok_or("stremio_preview_stale")?;
        preview.candidates = candidates;
        preview.summary = summary;
        preview.row_ids = row_ids;
        preview.row_handles = row_handles;
        preview.selected_addons = verified;
        preview.review_revision = next_revision;
        preview.reviewed = true;
        preview.review_in_progress = false;
        Ok(Json(json!({"stage":"review","preview_id":id,"profile_id":profile,"expires_at":slot.expires,"review_revision":preview.review_revision,
            "summary":preview.summary,"review_items":rows,"addons_to_add":count})))
    }).await.map_err(Into::into)
}

fn review_rows(
    db: &Connection,
    profile: i64,
    source: &str,
    candidates: &[mapper::Candidate],
    row_ids: &[String],
) -> Result<Vec<Value>, Error> {
    candidates.iter().zip(row_ids).map(|(c, item_id)| {
        let counts = merge(db, profile, source, std::slice::from_ref(c), Summary::default(), false)?;
        let favorite_action = if !c.favorite { "none" } else if counts.favorites_to_add > 0 { "add" }
            else if counts.already_imported > 0 { "already_imported" } else { "preserve" };
        let progress_action = if c.progress.is_none() { "none" } else if counts.progress_to_add > 0 { "add" }
            else if counts.progress_to_update > 0 { "update" } else if counts.already_imported > 0 { "already_imported" } else { "preserve" };
        let status = if matches!(favorite_action, "add") || matches!(progress_action, "add" | "update") { "ready" } else { "preserved" };
        let (season, episode) = c.progress.as_ref().map(|p| (&p.context["season"], &p.context["episode"]))
            .unwrap_or((&Value::Null, &Value::Null));
        let mut projected = c.progress.as_ref().map(|p| {
            let mut item = json!({"position":p.position,"duration":p.duration});
            library::add_watch_fields(&mut item, &p.context);
            item
        });
        if let Some(p) = &c.progress {
            let unchanged = counts.progress_to_add == 0 && counts.progress_to_update == 0;
            let completion_repair = counts.progress_to_update > 0 && p.context["stremio_completion_only"] == true;
            if unchanged || completion_repair {
                let existing: Option<(f64,f64,String)> = db.query_row(
                    "SELECT position,duration,context FROM progress WHERE profile_id=?1 AND type=?2 AND id=?3",
                    params![profile,c.kind,c.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))
                    .optional().map_err(|_| STORAGE)?;
                if let Some((position,duration,raw)) = existing {
                    if let Ok(mut context) = serde_json::from_str::<Value>(&raw) {
                        if completion_repair && context.is_object() {
                            context["stremio_import_watched"] = json!(true);
                        }
                        let mut item = json!({"position":position,"duration":duration});
                        library::add_watch_fields(&mut item, &context);
                        projected = Some(item);
                    }
                }
            }
        }
        Ok(json!({"item_id":item_id,"name":c.name,"type":c.kind,"favorite_action":favorite_action,
            "progress_action":progress_action,"status":status,"season":season,"episode":episode,
            "position":projected.as_ref().map(|p| &p["position"]),"duration":projected.as_ref().map(|p| &p["duration"]),
            "watched":projected.as_ref().map(|p| &p["watched"]),
            "resume_active":projected.as_ref().map(|p| &p["resume_active"]),
            "completion_only":projected.as_ref().map(|p| &p["completion_only"]),
            "watch_date_known":projected.as_ref().map(|p| &p["watch_date_known"]),"counts":{
                "favorites_to_add":counts.favorites_to_add,"progress_to_add":counts.progress_to_add,
                "progress_to_update":counts.progress_to_update,"existing_preserved":counts.existing_preserved,
                "already_imported":counts.already_imported},"selectable":true}))
    }).collect()
}
pub(crate) async fn cancel(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path((profile, id)): Path<(i64, String)>,
) -> Result<Json<Value>, Error> {
    let app = app.with_lease(lease);
    let worker = app.clone();
    account_api::run(app.clone(), app.request_lease(), move |db, _| {
        authorize(db, &worker, profile)?;
        let mut slots = worker.stremio_import.pending.lock().map_err(|_| STORAGE)?;
        let slot = slots
            .get(&id)
            .filter(|s| s.belongs(&worker, profile))
            .ok_or("stremio_preview_not_found")?;
        let expired = slot.expires <= util::now();
        slots.remove(&id);
        if expired {
            return Err("stremio_preview_expired".into());
        }
        Ok(Json(json!({"profile_id":profile,"cancelled":true})))
    })
    .await
}
pub(crate) async fn apply(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path((profile, id)): Path<(i64, String)>,
    body: Result<Json<Confirmation>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, Error> {
    let app = app.with_lease(lease);
    let Json(confirmation) = body.map_err(|_| "stremio_invalid_request")?;
    if confirmation.excluded_items.len() > MAX_REVIEW_ROWS
        || confirmation.excluded_items.iter().any(|s| s.len() > 64)
    {
        return Err("stremio_invalid_request".into());
    }
    let mut exclusions = confirmation.excluded_items;
    exclusions.sort();
    if exclusions.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err("stremio_invalid_request".into());
    }
    if !confirmation.confirm {
        return Err("stremio_confirmation_required".into());
    }
    let worker = app.clone();
    account_api::run(app.clone(), app.request_lease(), move |db, _| {
        authorize(db, &worker, profile)?;
        let mut slots = worker.stremio_import.pending.lock().map_err(|_| STORAGE)?;
        let slot = slots
            .get_mut(&id)
            .filter(|s| s.belongs(&worker, profile))
            .ok_or("stremio_preview_not_found")?;
        if slot.expires <= util::now() {
            slots.remove(&id);
            return Err("stremio_preview_expired".into());
        }
        if let Some(summary) = &slot.completed {
            if slot.completed_exclusions.as_ref() != Some(&exclusions)
                || confirmation.review_revision != slot.completed_review_revision
            { return Err("stremio_preview_stale".into()); }
            return Ok(Json(
                json!({"profile_id":profile,"summary":summary,"already_completed":true,"excluded_items":exclusions.len(),"addons_added":slot.completed_addons,"addons_existing":slot.completed_existing}),
            ));
        }
        let preview = slot.preview.as_ref().ok_or("stremio_preview_not_found")?;
        if !preview.reviewed || preview.review_in_progress { return Err("stremio_invalid_request".into()); }
        if confirmation.review_revision != Some(preview.review_revision)
            && !(preview.review_revision == 0 && confirmation.review_revision.is_none())
        { return Err("stremio_preview_stale".into()); }
        if exclusions.iter().any(|item| !preview.row_ids.contains(item)) { return Err("stremio_invalid_request".into()); }
        let selected: Vec<_> = preview.candidates.iter().zip(&preview.row_ids)
            .filter(|(_, item)| exclusions.binary_search(item).is_err()).map(|(candidate, _)| candidate).collect();
        if selected.is_empty() && preview.selected_addons.is_empty() { return Err("stremio_invalid_request".into()); }
        if snapshot(db, profile)? != preview.snapshot {
            return Err("stremio_preview_stale".into());
        }
        backup::create(db, &worker.stremio_import)?;
        let tx = rusqlite::Transaction::new_unchecked(db, rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| STORAGE)?;
        // Recheck after backup/network and under SQLite's write lock, not only middleware.
        worker.request_lease().validate(&tx)?;
        kids::require_parent(&tx, &worker.identity())?;
        authorize(&tx, &worker, profile)?;
        if slot.expires <= util::now() {
            return Err("stremio_preview_expired".into());
        }
        if snapshot(&tx, profile)? != preview.snapshot {
            return Err("stremio_preview_stale".into());
        }
        let mut base = preview.summary.clone();
        base.favorites_to_add = 0;
        base.progress_to_add = 0;
        base.progress_to_update = 0;
        base.existing_preserved = 0;
        base.already_imported = 0;
        let summary = merge(
            &tx,
            profile,
            &preview.source,
            &selected.into_iter().cloned().collect::<Vec<_>>(),
            base,
            true,
        )?;
        let mut addons_added = 0;
        let mut addons_existing = 0;
        let vault = worker.secret_vault.as_deref();
        for addon in &preview.selected_addons {
            let vault = vault.ok_or("stremio_addon_unavailable")?;
            if addon.expected.is_some() {
                if crate::addon::credentials_v2::snapshot(&tx, vault, slot.account, &addon.url).map_err(|_| "stremio_addon_unavailable")? != addon.expected {
                    return Err("stremio_preview_stale".into());
                }
                addons_existing += 1;
                continue;
            }
            crate::addon::credentials_v2::store_checked_tx(&tx, vault, slot.account, &addon.url, &addon.manifest, &addon.expected)
                .map_err(|_| "stremio_addon_unavailable")?;
            if addon.expected.is_none() { addons_added += 1; }
        }
        tx.commit().map_err(|_| STORAGE)?;
        slot.preview = None;
        slot.completed = Some(summary.clone());
        slot.completed_exclusions = Some(exclusions.clone());
        slot.completed_addons = addons_added;
        slot.completed_existing = addons_existing;
        slot.completed_review_revision = confirmation.review_revision;
        Ok(Json(
            json!({"profile_id":profile,"summary":summary,"already_completed":false,"excluded_items":exclusions.len(),"addons_added":addons_added,"addons_existing":addons_existing}),
        ))
    })
    .await
}

fn snapshot(db: &Connection, profile: i64) -> Result<[u8; 32], Error> {
    use rusqlite::types::ValueRef;
    let mut hash = Sha256::new();
    for table in [
        "favorites",
        "progress",
        "queue_hidden",
        "stremio_import_receipts",
    ] {
        hash.update(table.as_bytes());
        let mut statement = db
            .prepare(&format!(
                "SELECT * FROM {table} WHERE profile_id=?1 ORDER BY {}",
                if table == "queue_hidden" {
                    "type,title_id"
                } else if table == "stremio_import_receipts" {
                    "source_account,type,id"
                } else {
                    "type,id"
                }
            ))
            .map_err(|_| STORAGE)?;
        let columns = statement.column_count();
        let mut rows = statement.query([profile]).map_err(|_| STORAGE)?;
        while let Some(row) = rows.next().map_err(|_| STORAGE)? {
            hash.update([255]);
            for n in 0..columns {
                match row.get_ref(n).map_err(|_| STORAGE)? {
                    ValueRef::Null => hash.update([0]),
                    ValueRef::Integer(v) => {
                        hash.update([1]);
                        hash.update(v.to_le_bytes());
                    }
                    ValueRef::Real(v) => {
                        hash.update([2]);
                        hash.update(v.to_bits().to_le_bytes());
                    }
                    ValueRef::Text(v) | ValueRef::Blob(v) => {
                        hash.update([3]);
                        hash.update((v.len() as u64).to_le_bytes());
                        hash.update(v);
                    }
                }
            }
        }
    }
    Ok(hash.finalize().into())
}
/// Project an import-owned continuation without changing either episode's playback row.
/// Ordinary playback invalidates the marker by advancing the original activity timestamp.
pub(crate) fn continuation_item(
    db: &Connection,
    profile: i64,
    current: &Value,
) -> Result<Option<Value>, ApiError> {
    if current["type"] != "series" {
        return Ok(None);
    }
    let raw: Option<String> = db
        .query_row(
            "SELECT context FROM progress WHERE profile_id=?1 AND type='series' AND id=?2",
            params![profile, current["id"].as_str()],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?;
    let Some(context) = raw.and_then(|raw| serde_json::from_str::<Value>(&raw).ok()) else {
        return Ok(None);
    };
    let marker = &context["stremio_continuation"];
    if context["stremio_import_watched"] != true
        || context["progress_corrected"] == true
        || context["watched_override"] == false
        || marker["activity_at"].as_i64().is_none()
        || marker["activity_at"] != current["updated_at"]
    {
        return Ok(None);
    }
    let Some(target_id) = marker["id"]
        .as_str()
        .filter(|id| !id.is_empty() && id.len() <= 512)
    else {
        return Ok(None);
    };
    let title = continuation::title_id(current);
    let target: Option<(String, Option<String>, f64, f64, String)> = db.query_row(
        "SELECT name,poster,position,duration,context FROM progress WHERE profile_id=?1 AND type='series' AND id=?2 AND title_id=?3",
        params![profile, target_id, title],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    ).optional().map_err(db_error)?;
    let Some((name, poster, position, duration, raw)) = target else {
        return Ok(None);
    };
    let Ok(context) = serde_json::from_str::<Value>(&raw) else {
        return Ok(None);
    };
    let coordinate = |v: &Value| Some((v["season"].as_u64()?, v["episode"].as_u64()?));
    let (Some(from), Some(to)) = (coordinate(current), coordinate(&context)) else {
        return Ok(None);
    };
    if context["progress_corrected"] == true
        || context["stremio_import_watched"] != true
        || (from.0 == 0) != (to.0 == 0)
        || to <= from
    {
        return Ok(None);
    }
    let mut item = json!({"id":target_id,"type":"series","name":name,"poster":poster,
        "position":position,"duration":duration,"updated_at":current["updated_at"],
        "queue_title_id":current["queue_title_id"]});
    if !library::watched(&item, &context) {
        return Ok(None);
    }
    library::add_watch_fields(&mut item, &context);
    item.as_object_mut()
        .unwrap()
        .extend(matching_context(&context)?.as_object().unwrap().clone());
    Ok(Some(item))
}

fn merge(
    db: &Connection,
    profile: i64,
    source: &str,
    candidates: &[mapper::Candidate],
    mut summary: Summary,
    write: bool,
) -> Result<Summary, Error> {
    for c in candidates {
        let (favorite_receipt, progress_receipt, completion_version): (bool,i64,i64) = db.query_row(
            "SELECT favorite,progress_timestamp,completion_version FROM stremio_import_receipts WHERE profile_id=?1 AND source_account=?2 AND type=?3 AND id=?4",
            params![profile,source,c.kind,c.id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))
            .optional().map_err(|_| STORAGE)?.unwrap_or((false,0,0));
        if c.favorite {
            let existing: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM favorites WHERE profile_id=?1 AND type=?2 AND id=?3)", params![profile,c.kind,c.id], |r| r.get(0)).map_err(|_| STORAGE)?;
            if favorite_receipt {
                summary.already_imported += 1;
            } else if existing {
                summary.existing_preserved += 1;
            } else if write {
                db.execute("INSERT INTO favorites(profile_id,type,id,name,poster) VALUES(?1,?2,?3,?4,NULL)",params![profile,c.kind,c.id,c.name]).map_err(|_| STORAGE)?;
                summary.favorites_added += 1;
            } else {
                summary.favorites_to_add += 1;
            }
        }
        if let Some(p) = &c.progress {
            let existing: Option<(i64,String,f64)> = db.query_row("SELECT updated_at,context,duration FROM progress WHERE profile_id=?1 AND type=?2 AND id=?3",params![profile,c.kind,c.id],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(|_| STORAGE)?;
            let completion = p.context["stremio_import_watched"] == true;
            let local = existing
                .as_ref()
                .and_then(|(_, raw, _)| serde_json::from_str::<Value>(raw).ok());
            let protected = existing.is_some()
                && local.as_ref().is_none_or(|v| {
                    !v.is_object()
                        || v["progress_corrected"] == true
                        || (v.get("watched_override").is_some()
                            && !(v["watched_override"] == true
                                && v["stremio_import_watched"] == true))
                });
            let add_fact = completion
                && completion_version < 1
                && existing.is_some()
                && !protected
                && local
                    .as_ref()
                    .is_some_and(|v| v["stremio_import_watched"] != true);
            // Version 2 repairs only the unchanged, receipt-owned imported resume.
            // A watched assertion alone is not proof that local playback belongs to import.
            let add_anchor = completion_version < 2
                && p.context["stremio_continuation"].is_object()
                && progress_receipt == p.timestamp
                && existing
                    .as_ref()
                    .is_some_and(|(time, _, _)| *time == p.timestamp)
                && !protected
                && local.as_ref().is_some_and(|v| {
                    (v["stremio_import_watched"] == true || add_fact)
                        && v["stremio_continuation"] != p.context["stremio_continuation"]
                });
            let newer = existing
                .as_ref()
                .is_some_and(|(time, _, _)| *time >= p.timestamp);
            let replayed =
                p.timestamp <= progress_receipt && !(completion && completion_version < 1);
            if protected || (replayed && !add_fact && !add_anchor) {
                if replayed {
                    summary.already_imported += 1;
                } else {
                    summary.existing_preserved += 1;
                }
            } else if newer && !add_fact && !add_anchor {
                summary.existing_preserved += 1;
            } else if write {
                if let Some((_, _, local_duration)) = &existing {
                    let mut context = local.unwrap();
                    if completion {
                        context["stremio_import_watched"] = json!(true);
                    }
                    if add_fact || add_anchor {
                        if add_anchor {
                            context["stremio_continuation"] =
                                p.context["stremio_continuation"].clone();
                        }
                        db.execute("UPDATE progress SET context=?4 WHERE profile_id=?1 AND type=?2 AND id=?3",params![profile,c.kind,c.id,context.to_string()]).map_err(|_| STORAGE)?;
                    } else {
                        context
                            .as_object_mut()
                            .unwrap()
                            .remove("stremio_continuation");
                        if p.context["stremio_continuation"].is_object() {
                            context["stremio_continuation"] =
                                p.context["stremio_continuation"].clone();
                        }
                        let mut position = p.position;
                        let mut duration = p.duration;
                        if p.context["watched_override"] == true
                            && duration == 0.0
                            && local_duration.is_finite()
                            && *local_duration > 0.0
                        {
                            duration = *local_duration;
                            position = duration;
                        }
                        if p.context["watched_override"] == true {
                            context["watched_override"] = json!(true);
                        } else {
                            context.as_object_mut().unwrap().remove("watched_override");
                        }
                        if p.context["stremio_completion_only"] == true {
                            context["stremio_completion_only"] = json!(true);
                            context["stremio_watch_date_unknown"] = json!(true);
                        } else {
                            context
                                .as_object_mut()
                                .unwrap()
                                .remove("stremio_completion_only");
                            context
                                .as_object_mut()
                                .unwrap()
                                .remove("stremio_watch_date_unknown");
                        }
                        db.execute("UPDATE progress SET position=?4,duration=?5,updated_at=?6,context=?7 WHERE profile_id=?1 AND type=?2 AND id=?3",params![profile,c.kind,c.id,position,duration,p.timestamp,context.to_string()]).map_err(|_| STORAGE)?;
                    }
                    summary.progress_updated += 1;
                } else {
                    db.execute("INSERT INTO progress(profile_id,type,id,name,poster,position,duration,updated_at,context,title_id) VALUES(?1,?2,?3,?4,NULL,?5,?6,?7,?8,?9)",params![profile,c.kind,c.id,c.name,p.position,p.duration,p.timestamp,p.context.to_string(),c.title]).map_err(|_| STORAGE)?;
                    summary.progress_added += 1;
                }
            } else if existing.is_some() {
                summary.progress_to_update += 1;
            } else {
                summary.progress_to_add += 1;
            }
        }
        if write {
            let completion_version = c.progress.as_ref().map_or(0, |p| {
                if p.context["stremio_continuation"].is_object() {
                    2
                } else {
                    i64::from(p.context["stremio_import_watched"] == true)
                }
            });
            db.execute("INSERT INTO stremio_import_receipts(profile_id,source_account,type,id,favorite,progress_timestamp,completion_version) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(profile_id,source_account,type,id) DO UPDATE SET favorite=MAX(favorite,excluded.favorite),progress_timestamp=MAX(progress_timestamp,excluded.progress_timestamp),completion_version=MAX(completion_version,excluded.completion_version)",params![profile,source,c.kind,c.id,c.favorite,c.progress.as_ref().map(|p| p.timestamp).unwrap_or(0),completion_version]).map_err(|_| STORAGE)?;
        }
    }
    Ok(summary)
}
