//! One persistent, profile-scoped playback policy shared by phone, TV and server.
use super::*;
use serde::Serialize;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Preferences {
    pub audio_language: String,
    pub subtitle_language: String,
    pub subtitles_enabled: bool,
    pub subtitle_size: String,
    pub subtitle_style: String,
    pub autoplay: bool,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            audio_language: "en".into(),
            subtitle_language: "en".into(),
            subtitles_enabled: false,
            subtitle_size: "normal".into(),
            subtitle_style: "system".into(),
            autoplay: true,
        }
    }
}
pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS playback_preferences(profile_id INTEGER PRIMARY KEY REFERENCES profiles(id) ON DELETE CASCADE,value TEXT NOT NULL);")
}
pub(crate) fn load(db: &Connection, profile: i64) -> Result<Preferences, ApiError> {
    let value: Option<String> = db
        .query_row(
            "SELECT value FROM playback_preferences WHERE profile_id=?1",
            [profile],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?;
    let mut prefs: Preferences = value
        .map(|v| {
            let mut value: Value = serde_json::from_str(&v)?;
            if let Some(object) = value.as_object_mut() {
                object.remove("quality");
            }
            serde_json::from_value(value)
        })
        .transpose()
        .map_err(|_| "Invalid saved playback preferences")?
        .unwrap_or_default();
    prefs.autoplay = db
        .query_row(
            "SELECT autoplay FROM viewing_settings WHERE profile_id=?1",
            [profile],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?
        .unwrap_or(true);
    Ok(prefs)
}
fn validate(p: &Preferences) -> Result<(), ApiError> {
    for language in [&p.audio_language, &p.subtitle_language] {
        if ![
            "en", "es", "fr", "de", "it", "pt", "ja", "ko", "zh", "hi", "ar",
        ]
        .contains(&language.as_str())
        {
            return Err("Unsupported language preference".into());
        }
    }
    if !["small", "normal", "large"].contains(&p.subtitle_size.as_str())
        || !["system", "shadow", "opaque"].contains(&p.subtitle_style.as_str())
    {
        return Err("Invalid playback preference".into());
    }
    Ok(())
}
pub(crate) async fn get(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
) -> ApiResult {
    let app = app.with_lease(lease);
    blocking(move || {
        let db = app.db.lock().unwrap();
        app.require_profile(&db, profile)?;
        Ok(axum::Json(json!(load(&db, profile)?)))
    })
    .await
}
pub(crate) async fn put(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    axum::Json(value): axum::Json<Value>,
) -> ApiResult {
    let app = app.with_lease(lease);
    blocking(move || {
        let mut db=app.db.lock().unwrap();let tx=db.transaction().map_err(db_error)?;app.require_profile(&tx,profile)?;
        let mut merged=json!(load(&tx,profile)?);
        let object=value.as_object().ok_or("Invalid playback preferences")?;
        if object.is_empty() { return Err("Empty playback preferences".into()); }
        if object.keys().all(|key|key=="quality"){return Err(ApiError::from("client_update_required"));}
        for (key,value) in object { if key!="quality" {merged[key]=value.clone();} }
        let prefs:Preferences=serde_json::from_value(merged).map_err(|_| "Invalid playback preferences")?;
        validate(&prefs)?;
        let previous:Option<String>=tx.query_row("SELECT value FROM playback_preferences WHERE profile_id=?1",[profile],|row|row.get(0)).optional().map_err(db_error)?;
        let archived=previous.map(|value|serde_json::from_str::<Value>(&value)).transpose().map_err(|_|"Invalid saved playback preferences")?.and_then(|value|value.get("quality").cloned());
        let mut stored=json!(prefs);
        if let Some(quality)=archived {stored["quality"]=quality;}
        tx.execute("INSERT INTO playback_preferences(profile_id,value) VALUES(?1,?2) ON CONFLICT(profile_id) DO UPDATE SET value=excluded.value",params![profile,stored.to_string()]).map_err(db_error)?;
        tx.execute("INSERT INTO viewing_settings(profile_id,autoplay) VALUES(?1,?2) ON CONFLICT(profile_id) DO UPDATE SET autoplay=excluded.autoplay",params![profile,prefs.autoplay]).map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        Ok(axum::Json(json!(prefs)))
    }).await
}
