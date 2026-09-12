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
    pub quality: String,
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
            quality: "auto".into(),
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
        .map(|v| serde_json::from_str(&v))
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
        || !["auto", "1080p", "720p", "480p"].contains(&p.quality.as_str())
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
        for (key,value) in object { merged[key]=value.clone(); }
        let prefs:Preferences=serde_json::from_value(merged).map_err(|_| "Invalid playback preferences")?;
        validate(&prefs)?;
        tx.execute("INSERT INTO playback_preferences(profile_id,value) VALUES(?1,?2) ON CONFLICT(profile_id) DO UPDATE SET value=excluded.value",params![profile,json!(prefs).to_string()]).map_err(db_error)?;
        tx.execute("INSERT INTO viewing_settings(profile_id,autoplay) VALUES(?1,?2) ON CONFLICT(profile_id) DO UPDATE SET autoplay=excluded.autoplay",params![profile,prefs.autoplay]).map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        Ok(axum::Json(json!(prefs)))
    }).await
}
impl Preferences {
    pub(crate) fn cap(
        &self,
        caps: Option<playback::Capabilities>,
    ) -> Option<playback::Capabilities> {
        let height = match self.quality.as_str() {
            "1080p" => 1080,
            "720p" => 720,
            "480p" => 480,
            _ => return caps,
        };
        let mut caps = caps.unwrap_or_default();
        caps.max_height = caps.max_height.min(height);
        caps.max_width = caps.max_width.min(height * 16 / 9);
        Some(caps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quality_ceiling_never_increases_the_device_capability() {
        let mut prefs = Preferences {
            quality: "480p".into(),
            ..Default::default()
        };
        let caps = prefs
            .cap(Some(playback::Capabilities {
                max_width: 1920,
                max_height: 1080,
                ..Default::default()
            }))
            .unwrap();
        assert_eq!((caps.max_width, caps.max_height), (853, 480));
        prefs.quality = "1080p".into();
        let caps = prefs
            .cap(Some(playback::Capabilities {
                max_width: 640,
                max_height: 360,
                ..Default::default()
            }))
            .unwrap();
        assert_eq!((caps.max_width, caps.max_height), (640, 360));
        prefs.quality = "auto".into();
        assert!(prefs.cap(None).is_none());
    }
}
