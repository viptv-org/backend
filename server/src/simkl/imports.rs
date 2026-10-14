//! Profile-scoped, read-only remote import into the existing library and history.
use super::*;
mod aliases;
mod history;
pub(crate) use aliases::remote as remote_identity;
mod membership;
mod next;
mod original;
mod paused;
mod progress;
pub(in crate::simkl) mod schema;
mod worker;
pub(crate) use membership::{enrich, merge};
pub(super) use schema::init;
#[derive(Clone, Default)]
pub(crate) struct Status {
    pub running: bool,
    pub checked_at_millis: Option<u64>,
    pub library_revision: Option<u64>,
    pub added: usize,
    pub history: usize,
    pub preserved: usize,
    pub unresolved: usize,
    pub error: bool,
}
impl Status {
    pub(crate) fn json(&self) -> Value {
        use viptv_engine::{capabilities::Availability, library::LibraryImportStatus};
        serde_json::to_value(LibraryImportStatus {
            provider_id: "simkl".into(),
            running: self.running,
            checked_at_millis: self.checked_at_millis,
            library_revision: self.library_revision,
            added_titles: self.added as u64,
            imported_history: self.history as u64,
            preserved_local: self.preserved as u64,
            unresolved: self.unresolved as u64,
            availability: if self.error {
                Availability::TemporarilyUnavailable
            } else {
                Availability::Available
            },
            refresh_after_millis: self.running.then_some(4000),
        })
        .unwrap_or(Value::Null)
    }
}
pub(super) type Jobs = std::sync::Mutex<HashMap<String, Status>>;
fn key(app: &App, access: &store::Credential) -> String {
    format!(
        "{}:{}:{:?}",
        app.identity().key(),
        access.configuration_revision,
        access.revision
    )
}
pub(crate) fn schedule(app: &App, lease: &ResourceLease, profile: i64) -> Option<Status> {
    let access = store::credential(app, profile, Capability::Authorization).ok()?;
    access.token.as_ref()?;
    let key = key(app, &access);
    let now = util::now_millis().max(0) as u64;
    let mut jobs = app.simkl.imports.lock().ok()?;
    if let Some(status) = jobs.get(&key).filter(|s| {
        s.running
            || s.checked_at_millis
                .is_some_and(|last| now.saturating_sub(last) < 60000)
    }) {
        return Some(status.clone());
    }
    if jobs.len() >= 32 && !jobs.contains_key(&key) {
        jobs.retain(|_, s| s.running);
        if jobs.len() >= 32 {
            return None;
        }
    }
    let status = Status {
        running: true,
        ..Default::default()
    };
    jobs.insert(key.clone(), status.clone());
    drop(jobs);
    let app = app.clone();
    let lease = lease.clone();
    tokio::spawn(async move {
        let result = tokio::time::timeout(
            Duration::from_secs(90),
            worker::run(&app, &lease, profile, &access),
        )
        .await;
        let mut status = match result {
            Ok(Ok(mut status)) => {
                status.running = false;
                status.checked_at_millis = Some(util::now_millis().max(0) as u64);
                status
            }
            _ => Status {
                checked_at_millis: Some(util::now_millis().max(0) as u64),
                error: true,
                ..Default::default()
            },
        };
        if let Ok(db) = app.db.lock() {
            status.library_revision = db
                .query_row(
                    "SELECT revision FROM engine_library_revisions WHERE profile_id=?1",
                    [profile],
                    |r| r.get::<_, u64>(0),
                )
                .optional()
                .ok()
                .flatten()
                .map(|revision| revision.saturating_add(access.configuration_revision));
        }
        if let Ok(mut jobs) = app.simkl.imports.lock() {
            jobs.insert(key, status);
        }
    });
    Some(status)
}
pub(crate) async fn read(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
) -> ApiResult {
    let app = app.with_lease(lease.clone());
    let access = store::credential(&app, profile, Capability::Authorization)?;
    if access.token.is_none() {
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "Link Simkl for this profile".into(),
        ));
    }
    let status =
        schedule(&app, &lease, profile).ok_or_else(|| ApiError::from("Import unavailable"))?;
    Ok(axum::Json(status.json()))
}

#[cfg(test)]
mod tests;

pub(crate) fn publish_membership(
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    entries: &[viptv_engine::library::Entry],
) -> Result<(), ApiError> {
    let access = store::credential(app, profile, Capability::Authorization)?;
    let mut db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    schema::validate_db(&db, app, profile, &access, lease)?;
    let tx = db.transaction().map_err(db_error)?;
    membership::apply(
        &tx,
        profile,
        access.revision.unwrap_or(0),
        entries,
        util::now_millis().max(0) as u64,
    )?;
    tx.commit().map_err(db_error)?;
    Ok(())
}
