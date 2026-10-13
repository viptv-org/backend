//! Typed server profiles. Parent account PINs never become per-profile locks.
use super::*;
use viptv_engine::capabilities::Availability;
const AVATARS: [&str; 4] = ["vynxc", "zayne", "initial-pink", "initial-blue"];
pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS engine_profile_presentation(profile_id INTEGER PRIMARY KEY REFERENCES profiles(id) ON DELETE CASCADE,avatar_id TEXT);")
}
pub(super) fn avatar(db: &Connection, profile: i64) -> Result<Option<String>, ApiError> {
    db.query_row(
        "SELECT avatar_id FROM engine_profile_presentation WHERE profile_id=?1",
        [profile],
        |r| r.get(0),
    )
    .optional()
    .map_err(db_error)
    .map(Option::flatten)
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
        json!({"id":value["id"],"name":value["name"],"avatar_id":avatar(db,id)?,"locked":false,"selection_requires_parent":false,"lock_kind":null,"is_primary":value["is_primary"],"can_delete":value["is_primary"]!=true,"kids":value["kids"],"setup_complete":value["setup_complete"]}),
    )
}
fn payload(value: &Value, create: bool) -> Result<Option<Option<String>>, ApiError> {
    let value = value.as_object().ok_or("Invalid profile")?;
    if value.is_empty()
        || value.len() > 2
        || value
            .keys()
            .any(|key| !matches!(key.as_str(), "name" | "avatar_id"))
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
    match value.get("avatar_id") {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(id)) if id.is_empty() => Ok(Some(None)),
        Some(Value::String(id)) if AVATARS.contains(&id.as_str()) => Ok(Some(Some(id.clone()))),
        _ => Err("Unsupported profile avatar".into()),
    }
}
fn save_avatar(db: &Connection, id: i64, avatar: Option<Option<String>>) -> Result<(), ApiError> {
    if let Some(avatar) = avatar {
        db.execute("INSERT INTO engine_profile_presentation(profile_id,avatar_id) VALUES(?1,?2) ON CONFLICT(profile_id) DO UPDATE SET avatar_id=excluded.avatar_id",params![id,avatar]).map_err(db_error)?;
    }
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
        let avatar = payload(&value, true)?;
        let mut db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        let tx = db.transaction().map_err(db_error)?;
        app.request_lease().validate(&tx)?;
        kids::require_parent(&tx, &app.identity())?;
        let account = app.identity().account_id().ok_or_else(auth::unauthorized)?;
        let profile = auth::create_profile(&tx, account, &json!({"name":value["name"]}))?;
        let id = profile["id"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or("Invalid profile identity")?;
        save_avatar(&tx, id, avatar)?;
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
        let avatar = payload(&value, false)?;
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
        let mut legacy = json!({"name":value.get("name").unwrap_or(&current["name"])});
        if current["setup_complete"] == false && value.get("name").is_some() && avatar.is_some() {
            legacy["avatar_style"] = current["avatar_style"].clone();
            legacy["setup_complete"] = json!(true);
        }
        let profile = auth::update_profile(&tx, account, id, &legacy)?;
        save_avatar(&tx, id, avatar)?;
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
