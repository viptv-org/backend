//! Typed server profiles. Parent account PINs never become per-profile locks.
use super::*;
use viptv_engine::capabilities::Availability;
/// Legacy ids from KMP builds before the avatar catalog. They stay accepted and
/// projected as `avatar_id` while the account record holds no catalog choice;
/// the catalog (`avatar_style` + 1-based `avatar_choice`) lives on the account.
const AVATARS: [&str; 4] = ["vynxc", "zayne", "initial-pink", "initial-blue"];
pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS engine_profile_presentation(profile_id INTEGER PRIMARY KEY REFERENCES profiles(id) ON DELETE CASCADE,avatar_id TEXT);")
}
/// A profile's avatar as the account record holds it.
pub(super) struct Avatar {
    /// The legacy id, hidden once the account record holds a catalog choice
    /// so the catalog takes precedence on every client.
    pub(super) id: Option<String>,
    /// The account's catalog world (`critters` until one is chosen).
    pub(super) style: String,
    /// The 1-based catalog position, `None` while no catalog avatar is chosen.
    pub(super) choice: Option<u16>,
}
pub(super) fn avatar_record(db: &Connection, profile: i64) -> Result<Option<Avatar>, ApiError> {
    db.query_row(
        "SELECT e.avatar_id,p.avatar_style,p.avatar_seed FROM profiles p LEFT JOIN engine_profile_presentation e ON e.profile_id=p.id WHERE p.id=?1",
        [profile],
        |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
    )
    .optional()
    .map_err(db_error)
    .map(|row| {
        row.map(|(id, style, seed)| {
            let choice = auth::avatar_choice(&style, &seed).and_then(|c| u16::try_from(c).ok());
            Avatar { id: id.filter(|_| choice.is_none()), style, choice }
        })
    })
}
/// The legacy `avatar_id` the engine profile card shows.
pub(super) fn avatar(db: &Connection, profile: i64) -> Result<Option<String>, ApiError> {
    Ok(avatar_record(db, profile)?.and_then(|avatar| avatar.id))
}
fn state(availability: Availability, message: Option<&str>) -> Value {
    json!({"availability":availability,"message":message})
}
fn owns(db: &Connection, app: &App, id: i64) -> Result<(), ApiError> {
    app.request_lease().validate(db)?;
    let account = app.identity().account_id().ok_or_else(auth::unauthorized)?;
    let owns: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM profile_owners WHERE account_id=?1 AND profile_id=?2)",
            params![account, id],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    if !owns {
        return Err(auth::forbidden());
    }
    Ok(())
}
fn project(db: &Connection, value: &Value) -> Result<Value, ApiError> {
    let id = value["id"]
        .as_str()
        .and_then(|id| id.parse::<i64>().ok())
        .ok_or("Invalid profile identity")?;
    Ok(
        json!({"id":value["id"],"name":value["name"],"avatar_id":avatar(db,id)?,"avatar_style":value["avatar_style"],"avatar_choice":value["avatar_choice"],"avatar_url":value["avatar_url"],"locked":false,"selection_requires_parent":false,"lock_kind":null,"is_primary":value["is_primary"],"can_delete":value["is_primary"]!=true,"kids":value["kids"],"max_age":value["max_age"],"setup_complete":value["setup_complete"]}),
    )
}
/// A validated engine profile write.
struct Payload {
    /// Legacy id change: `None` leaves it, `Some(None)` clears it.
    legacy: Option<Option<String>>,
    /// `avatar_style`/`avatar_choice`, forwarded verbatim to the account layer,
    /// which owns their validation (style in `AVATAR_STYLES`, choice within the
    /// world's size).
    catalog: serde_json::Map<String, Value>,
}
impl Payload {
    fn touches_avatar(&self) -> bool {
        self.legacy.is_some() || !self.catalog.is_empty()
    }
}
fn payload(value: &Value, create: bool) -> Result<Payload, ApiError> {
    let value = value.as_object().ok_or("Invalid profile")?;
    if value.is_empty()
        || value.keys().any(|key| {
            !matches!(
                key.as_str(),
                "name" | "avatar_id" | "avatar_style" | "avatar_choice"
            )
        })
        || (create && !value.contains_key("name"))
    {
        return Err("Invalid profile fields".into());
    }
    if let Some(name) = value.get("name") {
        if !name.as_str().is_some_and(|s| {
            !s.trim().is_empty() && s.len() <= 80 && !s.chars().any(char::is_control)
        }) {
            return Err("Invalid profile name".into());
        }
    }
    let catalog: serde_json::Map<_, _> = value
        .iter()
        .filter(|(key, _)| matches!(key.as_str(), "avatar_style" | "avatar_choice"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let legacy = match value.get("avatar_id") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::String(id)) if id.is_empty() => Some(None),
        Some(Value::String(id)) if AVATARS.contains(&id.as_str()) => Some(Some(id.clone())),
        _ => return Err("Unsupported profile avatar".into()),
    };
    if !catalog.is_empty() && matches!(legacy, Some(Some(_))) {
        return Err("Choose either avatar_id or avatar_style/avatar_choice".into());
    }
    Ok(Payload { legacy, catalog })
}
/// Persists the legacy half of a write; a catalog write retires the legacy id.
fn save_avatar(db: &Connection, id: i64, payload: &Payload) -> Result<(), ApiError> {
    let legacy = match (&payload.legacy, payload.catalog.is_empty()) {
        (_, false) => None,
        (Some(legacy), true) => legacy.as_deref(),
        (None, true) => return Ok(()),
    };
    db.execute("INSERT INTO engine_profile_presentation(profile_id,avatar_id) VALUES(?1,?2) ON CONFLICT(profile_id) DO UPDATE SET avatar_id=excluded.avatar_id",params![id,legacy]).map_err(db_error)?;
    Ok(())
}
pub(crate) async fn list(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
) -> ApiResult {
    let app = app.with_lease(lease);
    blocking(move||{
        let db=app.db.lock().map_err(|_|ApiError::from("Database unavailable"))?;app.request_lease().validate(&db)?;
        let principal=app.identity();let account=principal.account_id().ok_or_else(auth::unauthorized)?;
        let restricted=kids::restricted(&db,&principal)?;let unlocked=kids::unlocked(&db,&principal)?;
        let configured:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM parent_controls WHERE account_id=?1)",[account],|r|r.get(0)).map_err(db_error)?;
        let available=if restricted&&!unlocked{state(Availability::NeedsAuthorization,Some("Unlock the account parent PIN to manage profiles"))}else{state(Availability::Available,None)};
        let mut cards=auth::list_profiles(&db,account)?.iter().map(|p|project(&db,p)).collect::<Result<Vec<_>,_>>()?;
        let auth::Principal::Account {profile_id,..}=principal;
        if restricted&&!unlocked {for card in &mut cards {if card["id"].as_str().and_then(|s|s.parse::<i64>().ok())!=profile_id {card["locked"]=json!(true);card["selection_requires_parent"]=json!(true);card["lock_kind"]=json!("parent");}}}
        Ok(axum::Json(json!({"profiles":cards,"avatar_ids":AVATARS,"capabilities":{"create":available,"rename":available,"delete":available,"avatar":available,"profile_pin":state(Availability::Unsupported,Some("This server uses account parent PINs; per-profile PINs are unavailable")),"parent_unlock":state(if configured{Availability::Available}else{Availability::NeedsConfiguration},None)},"parent":{"pin_configured":configured,"unlocked":unlocked,"restricted":restricted}})))
    }).await
}
pub(crate) async fn create(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    axum::Json(value): axum::Json<Value>,
) -> ApiResult {
    let app = app.with_lease(lease);
    blocking(move || {
        let payload = payload(&value, true)?;
        let mut db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        let tx = db.transaction().map_err(db_error)?;
        app.request_lease().validate(&tx)?;
        kids::require_parent(&tx, &app.identity())?;
        let account = app.identity().account_id().ok_or_else(auth::unauthorized)?;
        let mut account_fields = payload.catalog.clone();
        account_fields.insert("name".into(), value["name"].clone());
        let profile = auth::create_profile(&tx, account, &Value::Object(account_fields))?;
        let id = profile["id"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or("Invalid profile identity")?;
        save_avatar(&tx, id, &payload)?;
        let profile = project(&tx, &profile)?;
        tx.commit().map_err(db_error)?;
        Ok(axum::Json(profile))
    })
    .await
}
pub(crate) async fn update(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<i64>,
    axum::Json(value): axum::Json<Value>,
) -> ApiResult {
    let app = app.with_lease(lease);
    blocking(move || {
        let payload = payload(&value, false)?;
        let mut db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        let tx = db.transaction().map_err(db_error)?;
        owns(&tx, &app, id)?;
        kids::require_parent(&tx, &app.identity())?;
        let account = app.identity().account_id().ok_or_else(auth::unauthorized)?;
        let current = auth::list_profiles(&tx, account)?
            .into_iter()
            .find(|p| p["id"].as_str().and_then(|s| s.parse::<i64>().ok()) == Some(id))
            .ok_or_else(auth::forbidden)?;
        let mut account_fields = payload.catalog.clone();
        account_fields.insert(
            "name".into(),
            value.get("name").unwrap_or(&current["name"]).clone(),
        );
        // Naming a fresh imported profile and picking any avatar completes setup.
        if current["setup_complete"] == false
            && value.get("name").is_some()
            && payload.touches_avatar()
        {
            account_fields
                .entry("avatar_style")
                .or_insert_with(|| current["avatar_style"].clone());
            account_fields.insert("setup_complete".into(), json!(true));
        }
        let profile = auth::update_profile(&tx, account, id, &Value::Object(account_fields))?;
        save_avatar(&tx, id, &payload)?;
        let profile = project(&tx, &profile)?;
        tx.commit().map_err(db_error)?;
        Ok(axum::Json(profile))
    })
    .await
}
pub(crate) async fn pin_status(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<i64>,
) -> ApiResult {
    let app = app.with_lease(lease);
    blocking(move || {
        let db = app.db.lock().unwrap();
        owns(&db, &app, id)?;
        Ok(axum::Json(state(
            Availability::Unsupported,
            Some("This server uses account parent PINs; per-profile PINs are unavailable"),
        )))
    })
    .await
}
pub(crate) async fn unsupported_pin(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<i64>,
) -> ApiResult {
    let app = app.with_lease(lease);
    blocking(move || {
        let db = app.db.lock().unwrap();
        owns(&db, &app, id)?;
        Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Per-profile PINs are unavailable on this server".into(),
        ))
    })
    .await
}
