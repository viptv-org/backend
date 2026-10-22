use super::*;
use viptv_engine::preferences::{PreferencesSnapshot, ProfilePreferences};
pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS engine_profile_preferences(profile_id INTEGER PRIMARY KEY REFERENCES profiles(id) ON DELETE CASCADE,account_id INTEGER NOT NULL,revision INTEGER NOT NULL,value TEXT NOT NULL)")
}
pub(crate) async fn get(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
) -> ApiResult {
    let app = app.with_lease(lease.clone());
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_profile(&db, profile)?;
    lease.validate(&db)?;
    let snapshot = read(&db, lease.principal.account_id().unwrap(), profile)?;
    Ok(axum::Json(
        serde_json::to_value(snapshot).map_err(|_| ApiError::from("Preferences unavailable"))?,
    ))
}
fn read(db: &Connection, account: i64, profile: i64) -> Result<PreferencesSnapshot, ApiError> {
    let row:Option<(u64,String)>=db.query_row("SELECT revision,value FROM engine_profile_preferences WHERE account_id=?1 AND profile_id=?2",params![account,profile],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(db_error)?;
    match row {
        Some((revision, value)) => Ok(PreferencesSnapshot {
            revision,
            preferences: serde_json::from_str(&value)
                .map_err(|_| ApiError::from("Preferences unavailable"))?,
        }),
        None => Ok(PreferencesSnapshot {
            revision: 0,
            preferences: ProfilePreferences::default(),
        }),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Update {
    revision: u64,
    /// Parsed against the stored row so an on/off-only client keeps `foreign`.
    preferences: Value,
}
pub(crate) async fn update(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    axum::Json(update): axum::Json<Update>,
) -> ApiResult {
    let app = app.with_lease(lease.clone());
    let account = lease.principal.account_id().ok_or_else(auth::forbidden)?;
    let mut db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_profile(&db, profile)?;
    lease.validate(&db)?;
    let tx = db.transaction().map_err(db_error)?;
    let previous = read(&tx, account, profile)?;
    if previous.revision != update.revision {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Profile preferences changed".into(),
        ));
    }
    // Shape errors (unknown fields, unknown enum values) keep the JSON
    // extractor's 422; well-formed values that fail validation are 400.
    let preferences = ProfilePreferences::from_update(update.preferences, &previous.preferences)
        .map_err(|_| {
            ApiError(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid profile preferences".into(),
            )
        })?;
    preferences
        .validate()
        .map_err(|_| ApiError::from("Invalid profile preferences"))?;
    let revision = update
        .revision
        .checked_add(1)
        .ok_or("Preference revision unavailable")?;
    let changed=tx.execute("INSERT INTO engine_profile_preferences VALUES(?1,?2,?3,?4) ON CONFLICT(profile_id) DO UPDATE SET revision=excluded.revision,value=excluded.value WHERE engine_profile_preferences.account_id=?2 AND engine_profile_preferences.revision=?5",params![profile,account,revision,serde_json::to_string(&preferences).map_err(|_|ApiError::from("Invalid preferences"))?,update.revision]).map_err(db_error)?;
    if changed != 1 {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Profile preferences changed".into(),
        ));
    }
    tx.commit().map_err(db_error)?;
    Ok(axum::Json(
        json!({"revision":revision,"preferences":preferences}),
    ))
}
