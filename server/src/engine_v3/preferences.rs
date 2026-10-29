use super::*;
use viptv_engine::preferences::{DisplayPreferences, PreferencesSnapshot, ProfilePreferences};
pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS engine_profile_preferences(profile_id INTEGER PRIMARY KEY REFERENCES profiles(id) ON DELETE CASCADE,account_id INTEGER NOT NULL,revision INTEGER NOT NULL,value TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS engine_profile_display_preferences(profile_id INTEGER PRIMARY KEY REFERENCES profiles(id) ON DELETE CASCADE,account_id INTEGER NOT NULL,value TEXT NOT NULL)")
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
    let snapshot = PreferencesSnapshot {
        display: read_display(&db, lease.principal.account_id().unwrap(), profile)?,
        ..snapshot
    };
    let mut value=serde_json::to_value(snapshot).map_err(|_|ApiError::from("Preferences unavailable"))?;
    super::viewing_presentation::preference_controls(&mut value);
    Ok(axum::Json(value))
}
pub(crate) fn read(db: &Connection, account: i64, profile: i64) -> Result<PreferencesSnapshot, ApiError> {
    let row:Option<(u64,String)>=db.query_row("SELECT revision,value FROM engine_profile_preferences WHERE account_id=?1 AND profile_id=?2",params![account,profile],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(db_error)?;
    match row {
        Some((revision, value)) => Ok(PreferencesSnapshot {
            revision,
            preferences: serde_json::from_str(&value)
                .map_err(|_| ApiError::from("Preferences unavailable"))?,
            display: DisplayPreferences::default(),
        }),
        None => Ok(PreferencesSnapshot {
            revision: 0,
            preferences: inherited(db, profile)?,
            display: DisplayPreferences::default(),
        }),
    }
}
pub(crate) fn read_display(
    db: &Connection,
    account: i64,
    profile: i64,
) -> Result<DisplayPreferences, ApiError> {
    let value: Option<String> = db
        .query_row(
            "SELECT value FROM engine_profile_display_preferences WHERE account_id=?1 AND profile_id=?2",
            params![account, profile],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?;
    value
        .map(|v| serde_json::from_str(&v).map_err(|_| ApiError::from("Invalid saved display preferences")))
        .transpose()
        .map(Option::unwrap_or_default)
}
// The snapshot and playback admission must resolve the same effective values.
fn inherited(db: &Connection, profile: i64) -> Result<ProfilePreferences, ApiError> {
    let mut result = ProfilePreferences::default();
    let saved: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM playback_preferences WHERE profile_id=?1)",
        [profile], |row| row.get(0),
    ).map_err(db_error)?;
    if saved {
        let legacy = crate::preferences::load(db, profile)?;
        result.preferred_audio_language = Some(legacy.audio_language);
        result.preferred_subtitle_language = Some(legacy.subtitle_language);
        result.subtitle_mode = match legacy.subtitle_mode.as_str() {
            "always" => viptv_engine::preferences::SubtitleMode::Always,
            "never" => viptv_engine::preferences::SubtitleMode::Never,
            "foreign" => viptv_engine::preferences::SubtitleMode::Foreign,
            _ => return Err(ApiError::from("Invalid saved subtitle mode")),
        };
        result.subtitles_enabled = result.subtitle_mode != viptv_engine::preferences::SubtitleMode::Never;
    }
    Ok(result)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Update {
    revision: u64,
    preferences: Option<Value>,
    display: Option<DisplayPreferences>,
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
    if update.preferences.is_none() && update.display.is_none() {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid profile preferences".into(),
        ));
    }
    let previous = read(&tx, account, profile)?;
    let previous_display = read_display(&tx, account, profile)?;
    if previous.revision != update.revision {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Profile preferences changed".into(),
        ));
    }
    // Shape errors keep the extractor's 422; values that fail validation are 400.
    let preferences = match update.preferences {
        Some(value) => ProfilePreferences::from_update(value, &previous.preferences)
            .map_err(|_| {
                ApiError(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "Invalid profile preferences".into(),
                )
            })?,
        None => previous.preferences.clone(),
    };
    preferences
        .validate()
        .map_err(|_| ApiError::from("Invalid profile preferences"))?;
    let display = match update.display {
        Some(display) => {
            let display = display.normalized();
            display
                .validate()
                .map_err(|_| ApiError::from("Invalid display preferences"))?;
            display
        }
        None => previous_display,
    };
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
    tx.execute("INSERT INTO engine_profile_display_preferences VALUES(?1,?2,?3) ON CONFLICT(profile_id) DO UPDATE SET value=excluded.value WHERE engine_profile_display_preferences.account_id=?2",params![profile,account,serde_json::to_string(&display).map_err(|_|ApiError::from("Invalid display preferences"))?]).map_err(db_error)?;
    tx.commit().map_err(db_error)?;
    let mut value=json!({"revision":revision,"preferences":preferences,"display":display});
    super::viewing_presentation::preference_controls(&mut value);
    Ok(axum::Json(value))
}
