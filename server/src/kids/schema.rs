use super::*;

pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS kids_profiles(profile_id INTEGER PRIMARY KEY REFERENCES profiles(id) ON DELETE CASCADE,enabled INTEGER NOT NULL DEFAULT 0,max_age INTEGER NOT NULL DEFAULT 12,revision INTEGER NOT NULL DEFAULT 0);
    CREATE TABLE IF NOT EXISTS parent_controls(account_id INTEGER PRIMARY KEY REFERENCES auth_accounts(id) ON DELETE CASCADE,pin_hash TEXT NOT NULL,failures INTEGER NOT NULL DEFAULT 0,blocked_until INTEGER NOT NULL DEFAULT 0);
    CREATE TABLE IF NOT EXISTS parent_grants(session_id TEXT PRIMARY KEY REFERENCES auth_sessions(id) ON DELETE CASCADE,expires INTEGER NOT NULL);
    CREATE TABLE IF NOT EXISTS kids_media(account_id INTEGER NOT NULL REFERENCES auth_accounts(id) ON DELETE CASCADE,kind TEXT NOT NULL,id TEXT NOT NULL,parent_id TEXT NOT NULL,metadata TEXT NOT NULL,age INTEGER,conflict INTEGER NOT NULL DEFAULT 0,updated_at INTEGER NOT NULL,PRIMARY KEY(account_id,kind,id));
    CREATE TABLE IF NOT EXISTS kids_ambiguous(account_id INTEGER NOT NULL REFERENCES auth_accounts(id) ON DELETE CASCADE,kind TEXT NOT NULL,id TEXT NOT NULL,PRIMARY KEY(account_id,kind,id));
    CREATE TABLE IF NOT EXISTS kids_approvals(profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,kind TEXT NOT NULL,id TEXT NOT NULL,PRIMARY KEY(profile_id,kind,id));")?;
    let family_exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='family_channels')",
        [],
        |r| r.get(0),
    )?;
    if family_exists {
        db.execute_batch("CREATE TRIGGER IF NOT EXISTS kids_family_policy_update AFTER UPDATE ON family_channels WHEN json_extract(OLD.data,'$.enabled') IS NOT json_extract(NEW.data,'$.enabled') OR json_extract(OLD.data,'$.category') IS NOT json_extract(NEW.data,'$.category') OR json_extract(OLD.data,'$.country') IS NOT json_extract(NEW.data,'$.country') OR json_extract(OLD.data,'$.language') IS NOT json_extract(NEW.data,'$.language') BEGIN UPDATE kids_profiles SET revision=revision+1 WHERE enabled=1; END; CREATE TRIGGER IF NOT EXISTS kids_family_policy_delete AFTER DELETE ON family_channels BEGIN UPDATE kids_profiles SET revision=revision+1 WHERE enabled=1; END;")?;
    }
    Ok(())
}
pub(super) fn forbidden() -> ApiError {
    ApiError(
        StatusCode::FORBIDDEN,
        "This title is unavailable in this kids profile".into(),
    )
}
pub(super) fn parent_required() -> ApiError {
    ApiError(StatusCode::FORBIDDEN, MSG_PARENT_REQUIRED.into())
}
// One stored-PIN lookup; callers bind either a known account id or a nullable one.
pub(super) fn stored_pin(
    db: &Connection,
    account: impl rusqlite::ToSql,
) -> Result<Option<String>, ApiError> {
    db.query_row(
        "SELECT pin_hash FROM parent_controls WHERE account_id=?1",
        [account],
        |r| r.get(0),
    )
    .optional()
    .map_err(db_error)
}
pub(crate) fn revision(db: &Connection, p: &auth::Principal) -> Result<i64, ApiError> {
    let auth::Principal::Account { profile_id, .. } = p;
    db.query_row(
        "SELECT revision FROM kids_profiles WHERE profile_id=?1",
        [profile_id],
        |r| r.get(0),
    )
    .optional()
    .map_err(db_error)
    .map(|v| v.unwrap_or(0))
}
pub(crate) fn restricted(db: &Connection, p: &auth::Principal) -> Result<bool, ApiError> {
    let auth::Principal::Account { profile_id, .. } = p;
    db.query_row(
        "SELECT enabled FROM kids_profiles WHERE profile_id=?1",
        [profile_id],
        |r| r.get(0),
    )
    .optional()
    .map_err(db_error)
    .map(|v| v.unwrap_or(false))
}
pub(crate) fn unlocked(db: &Connection, p: &auth::Principal) -> Result<bool, ApiError> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM parent_grants WHERE session_id=?1 AND expires>?2)",
        params![p.session_id(), util::now()],
        |r| r.get(0),
    )
    .map_err(db_error)
}
pub(crate) fn require_parent(db: &Connection, p: &auth::Principal) -> Result<(), ApiError> {
    if restricted(db, p)? && !unlocked(db, p)? {
        return Err(parent_required());
    }
    Ok(())
}
pub(crate) fn switch_profile(
    db: &Connection,
    p: &auth::Principal,
    target: i64,
) -> Result<(), ApiError> {
    let auth::Principal::Account { profile_id, .. } = p;
    if *profile_id != Some(target) {
        require_parent(db, p)?;
    }
    db.execute(
        "DELETE FROM parent_grants WHERE session_id=?1",
        [p.session_id()],
    )
    .map_err(db_error)?;
    Ok(())
}
pub(super) fn manager(
    db: &Connection,
    p: &auth::Principal,
    profile: Option<i64>,
) -> Result<i64, ApiError> {
    auth::require_household_manager(p)?;
    require_parent(db, p)?;
    let account = p.account_id().ok_or("Account required")?;
    if let Some(profile) = profile {
        let owns: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM profile_owners WHERE profile_id=?1 AND account_id=?2)",
                params![profile, account],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        if !owns {
            return Err(ApiError(
                StatusCode::FORBIDDEN,
                "Profile access denied".into(),
            ));
        }
    }
    Ok(account)
}
pub(crate) fn profile_fields(db: &Connection, id: i64) -> Result<Value, ApiError> {
    db.query_row(
        "SELECT enabled,max_age FROM kids_profiles WHERE profile_id=?1",
        [id],
        |r| Ok(json!({"enabled":r.get::<_,bool>(0)?,"max_age":r.get::<_,i64>(1)?})),
    )
    .optional()
    .map_err(db_error)
    .map(|v| v.unwrap_or(json!({"enabled":false,"max_age":12})))
}
pub(crate) async fn status(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
) -> ApiResult {
    let app = app.with_lease(lease);
    blocking(move||{let db=app.db.lock().unwrap();app.request_lease().validate(&db)?;let p=app.identity();let configured:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM parent_controls WHERE account_id=?1)",[p.account_id()],|r|r.get(0)).map_err(db_error)?;Ok(axum::Json(json!({"pin_configured":configured,"unlocked":unlocked(&db,&p)?,"restricted":restricted(&db,&p)?})))}).await
}

pub(crate) async fn get_policy(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<i64>,
) -> ApiResult {
    let app = app.with_lease(lease);
    blocking(move || {
        let db = app.db.lock().unwrap();
        app.request_lease().validate(&db)?;
        manager(&db, &app.identity(), Some(id))?;
        Ok(axum::Json(profile_fields(&db, id)?))
    })
    .await
}
pub(crate) async fn set_policy(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<i64>,
    axum::Json(value): axum::Json<Value>,
) -> ApiResult {
    let app = app.with_lease(lease);
    blocking(move||{let mut db=app.db.lock().unwrap();let tx=db.transaction().map_err(db_error)?;app.request_lease().validate(&tx)?;let account=manager(&tx,&app.identity(),Some(id))?;
        let enabled=value["enabled"].as_bool().ok_or("Invalid kids setting")?;let age=value["max_age"].as_i64().filter(|v|(0..=17).contains(v)).ok_or("Age ceiling must be between 0 and 17")?;
        if enabled {let configured:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM parent_controls WHERE account_id=?1)",[account],|r|r.get(0)).map_err(db_error)?;if !configured{return Err(ApiError(StatusCode::CONFLICT,"Set a parent PIN before enabling a kids profile".into()));}}
        tx.execute("INSERT INTO kids_profiles(profile_id,enabled,max_age,revision) VALUES(?1,?2,?3,1) ON CONFLICT(profile_id) DO UPDATE SET enabled=excluded.enabled,max_age=excluded.max_age,revision=kids_profiles.revision+1",params![id,enabled,age]).map_err(db_error)?;
        tx.execute("DELETE FROM parent_grants WHERE session_id IN(SELECT id FROM auth_sessions WHERE profile_id=?1)",[id]).map_err(db_error)?;
        tx.commit().map_err(db_error)?;Ok(axum::Json(json!({"enabled":enabled,"max_age":age})))
    }).await
}

#[derive(Deserialize, Default)]
pub(crate) struct ApprovalPage {
    #[serde(default)]
    offset: usize,
}
pub(crate) async fn approvals(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<i64>,
    Query(page): Query<ApprovalPage>,
) -> ApiResult {
    let app = app.with_lease(lease);
    blocking(move||{let db=app.db.lock().unwrap();app.request_lease().validate(&db)?;let account=manager(&db,&app.identity(),Some(id))?;
        let mut stmt=db.prepare("SELECT a.id,a.kind,json_object('name',json_extract(m.metadata,'$.name'),'poster',json_extract(m.metadata,'$.poster')) FROM kids_approvals a LEFT JOIN kids_media m ON m.account_id=?1 AND m.kind=a.kind AND m.id=a.id WHERE a.profile_id=?2 ORDER BY a.kind,a.id LIMIT 500 OFFSET ?3").map_err(db_error)?;
        let rows=stmt.query_map(params![account,id,page.offset.min(i64::MAX as usize)],|r|{let metadata:Option<String>=r.get(2)?;let meta:Value=metadata.and_then(|s|serde_json::from_str(&s).ok()).unwrap_or(Value::Null);Ok(json!({"id":r.get::<_,String>(0)?,"type":r.get::<_,String>(1)?,"name":meta["name"],"poster":meta["poster"]}))}).map_err(db_error)?.collect::<Result<Vec<_>,_>>().map_err(db_error)?;Ok(axum::Json(json!(rows)))
    }).await
}
pub(crate) async fn approve(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    axum::Json(value): axum::Json<Value>,
) -> ApiResult {
    let app = app.with_lease(lease);
    let kind = media_type(&value)?.to_owned();
    if kind == "live" {
        return Err("Live kids access uses the curated Family Kids lineup".into());
    }
    let id = text(&value, "id", 512)?.to_owned();
    let approved = value["approved"]
        .as_bool()
        .ok_or("Approval choice required")?;
    {
        let db = app.db.lock().unwrap();
        app.request_lease().validate(&db)?;
        manager(&db, &app.identity(), Some(profile))?;
    }
    let meta = if approved {
        let response = tokio::time::timeout(Duration::from_secs(15), app.addons.meta(&kind, &id))
            .await
            .map_err(|_| "Metadata timed out")??;
        if response["meta"]["id"] != id {
            return Err("Metadata identity did not match the requested title".into());
        }
        Some(response["meta"].clone())
    } else {
        None
    };
    blocking(move||{let mut db=app.db.lock().unwrap();let tx=db.transaction().map_err(db_error)?;app.request_lease().validate(&tx)?;let account=manager(&tx,&app.identity(),Some(profile))?;
        if let Some(meta)=meta {observe(&tx,account,&kind,&meta,true)?;let (_,_,age,_)=media(&tx,&app.identity(),&kind,&id)?.ok_or("Metadata unavailable")?;if age.is_some_and(|age|age>=18){return Err("Adult titles cannot be approved for kids profiles".into());}
            let count:i64=tx.query_row("SELECT count(*) FROM kids_approvals WHERE profile_id=?1",[profile],|r|r.get(0)).map_err(db_error)?;let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM kids_approvals WHERE profile_id=?1 AND kind=?2 AND id=?3)",params![profile,kind,id],|r|r.get(0)).map_err(db_error)?;if count>=500 && !exists{return Err("Up to 500 approved titles are supported. Revoke a title before adding another".into());}
            tx.execute("INSERT OR IGNORE INTO kids_approvals(profile_id,kind,id) VALUES(?1,?2,?3)",params![profile,kind,id]).map_err(db_error)?;
        }else{tx.execute("DELETE FROM kids_approvals WHERE profile_id=?1 AND kind=?2 AND id=?3",params![profile,kind,id]).map_err(db_error)?;}
        tx.execute("INSERT INTO kids_profiles(profile_id,revision) VALUES(?1,1) ON CONFLICT(profile_id) DO UPDATE SET revision=revision+1",[profile]).map_err(db_error)?;
        prune_observations(&tx,account)?;
        tx.commit().map_err(db_error)?;Ok(axum::Json(json!({"approved":approved})))
    }).await
}
