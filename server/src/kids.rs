//! Household policy: server-observed identities, bounded parent authority, and fail-closed browsing.
use super::*;
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use std::sync::OnceLock;

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
fn forbidden() -> ApiError {
    ApiError(
        StatusCode::FORBIDDEN,
        "This title is unavailable in this kids profile".into(),
    )
}
fn parent_required() -> ApiError {
    ApiError(StatusCode::FORBIDDEN, "Parent PIN required".into())
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
fn manager(db: &Connection, p: &auth::Principal, profile: Option<i64>) -> Result<i64, ApiError> {
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
fn pin_text(value: &Value, key: &str) -> Result<String, ApiError> {
    let pin = value[key].as_str().ok_or("PIN required")?;
    if !(4..=8).contains(&pin.len()) || !pin.bytes().all(|b| b.is_ascii_digit()) {
        return Err("PIN must contain 4 to 8 digits".into());
    }
    Ok(pin.into())
}
async fn pin_cpu<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    static GATE: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    let permit = tokio::time::timeout(
        Duration::from_secs(2),
        GATE.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(2)))
            .clone()
            .acquire_owned(),
    )
    .await
    .map_err(|_| {
        ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "Try the parent PIN again shortly".into(),
        )
    })?
    .map_err(|_| "PIN service unavailable")?;
    blocking(move || {
        let _permit = permit;
        work()
    })
    .await
}
async fn verify_pin(app: &App, pin: String) -> Result<String, ApiError> {
    let p = app.identity();
    let account = p.account_id().ok_or("Account required")?;
    let snapshot = {
        let db = app.db.lock().unwrap();
        app.request_lease().validate(&db)?;
        let row: Option<(String, i64, i64)> = db
            .query_row(
                "SELECT pin_hash,failures,blocked_until FROM parent_controls WHERE account_id=?1",
                [account],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(db_error)?;
        let (hash, failures, blocked) = row.ok_or_else(parent_required)?;
        if blocked > util::now() {
            return Err(ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many PIN attempts. Try again in five minutes".into(),
            ));
        }
        let attempts = if blocked > 0 { 1 } else { failures + 1 };
        db.execute(
            "UPDATE parent_controls SET failures=?1,blocked_until=?2 WHERE account_id=?3",
            params![
                attempts,
                if attempts >= 5 { util::now() + 300 } else { 0 },
                account
            ],
        )
        .map_err(db_error)?;
        hash
    };
    let hash = snapshot.clone();
    let valid = pin_cpu(move || {
        Ok(PasswordHash::new(&hash).ok().is_some_and(|h| {
            Argon2::default()
                .verify_password(pin.as_bytes(), &h)
                .is_ok()
        }))
    })
    .await?;
    let db = app.db.lock().unwrap();
    app.request_lease().validate(&db)?;
    let current: Option<String> = db
        .query_row(
            "SELECT pin_hash FROM parent_controls WHERE account_id=?1",
            [account],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?;
    if !valid || current.as_deref() != Some(snapshot.as_str()) {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Incorrect parent PIN".into(),
        ));
    }
    db.execute(
        "UPDATE parent_controls SET failures=0,blocked_until=0 WHERE account_id=?1",
        [account],
    )
    .map_err(db_error)?;
    Ok(snapshot)
}
pub(crate) async fn unlock(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    axum::Json(value): axum::Json<Value>,
) -> ApiResult {
    let app = app.with_lease(lease);
    let verified = verify_pin(&app, pin_text(&value, "pin")?).await?;
    let db = app.db.lock().unwrap();
    app.request_lease().validate(&db)?;
    let current: Option<String> = db
        .query_row(
            "SELECT pin_hash FROM parent_controls WHERE account_id=?1",
            [app.identity().account_id()],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?;
    if current.as_deref() != Some(verified.as_str()) {
        return Err(parent_required());
    }
    db.execute("INSERT INTO parent_grants(session_id,expires) VALUES(?1,?2) ON CONFLICT(session_id) DO UPDATE SET expires=excluded.expires",params![app.identity().session_id(),util::now()+120]).map_err(db_error)?;
    Ok(axum::Json(json!({"unlocked":true})))
}
pub(crate) async fn set_pin(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    axum::Json(value): axum::Json<Value>,
) -> ApiResult {
    let app = app.with_lease(lease);
    let (account, existing) = {
        let db = app.db.lock().unwrap();
        app.request_lease().validate(&db)?;
        auth::require_household_manager(&app.identity())?;
        let account = app.identity().account_id().ok_or("Account required")?;
        let existing: Option<String> = db
            .query_row(
                "SELECT pin_hash FROM parent_controls WHERE account_id=?1",
                [account],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?;
        if existing.is_none() {
            require_parent(&db, &app.identity())?;
        }
        (account, existing)
    };
    if existing.is_some() {
        verify_pin(&app, pin_text(&value, "current_pin")?).await?;
    }
    let removing = value["pin"].as_str() == Some("");
    let next = if removing {
        None
    } else {
        let pin = pin_text(&value, "pin")?;
        Some(
            pin_cpu(move || {
                Argon2::default()
                    .hash_password(pin.as_bytes(), &SaltString::generate(&mut OsRng))
                    .map(|v| v.to_string())
                    .map_err(|_| ApiError::from("PIN could not be saved"))
            })
            .await?,
        )
    };
    let mut db = app.db.lock().unwrap();
    let tx = db.transaction().map_err(db_error)?;
    app.request_lease().validate(&tx)?;
    let current: Option<String> = tx
        .query_row(
            "SELECT pin_hash FROM parent_controls WHERE account_id=?1",
            [account],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?;
    if current != existing {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "PIN changed. Try again".into(),
        ));
    }
    if let Some(hash) = next {
        tx.execute("INSERT INTO parent_controls(account_id,pin_hash) VALUES(?1,?2) ON CONFLICT(account_id) DO UPDATE SET pin_hash=excluded.pin_hash,failures=0,blocked_until=0",params![account,hash]).map_err(db_error)?;
    } else {
        let active:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM kids_profiles k JOIN profile_owners o ON o.profile_id=k.profile_id WHERE o.account_id=?1 AND k.enabled=1)",[account],|r|r.get(0)).map_err(db_error)?;
        if active {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "Disable kids profiles before removing the PIN".into(),
            ));
        }
        tx.execute("DELETE FROM parent_controls WHERE account_id=?1", [account])
            .map_err(db_error)?;
    }
    tx.execute("DELETE FROM parent_grants WHERE session_id IN(SELECT id FROM auth_sessions WHERE account_id=?1)",[account]).map_err(db_error)?;
    tx.commit().map_err(db_error)?;
    Ok(axum::Json(json!({"pin_configured":!removing})))
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

fn known_age(value: &str) -> Option<i64> {
    match value.trim().to_uppercase().as_str() {
        "G" | "TV-G" | "TV-Y" => Some(0),
        "TV-Y7" | "TV-Y7-FV" => Some(7),
        "PG" | "TV-PG" => Some(10),
        "PG-13" => Some(13),
        "TV-14" => Some(14),
        "R" => Some(17),
        "NC-17" | "TV-MA" => Some(18),
        _ => None,
    }
}
fn assessment(meta: &Value) -> (Option<i64>, bool) {
    let mut ages = HashSet::new();
    let mut unknown = false;
    for key in ["certification", "contentRating", "ageRating", "mpaaRating"] {
        if let Some(v) = meta.get(key) {
            if let Some(age) = v.as_str().and_then(known_age) {
                ages.insert(age);
            } else {
                unknown = true;
            }
        }
    }
    if meta["genres"].as_array().is_some_and(|v| {
        v.iter().any(|v| {
            v.as_str().is_some_and(|v| {
                matches!(v.to_lowercase().as_str(), "adult" | "porn" | "pornography")
            })
        })
    }) {
        ages.insert(18);
    }
    let age = ages.iter().max().copied();
    (age, unknown || ages.len() > 1)
}
/// Only call with metadata returned by an account's configured server-side addon fetch.
// Store only presentation and policy fields; addon extension blobs never enter the policy cache.
fn compact_metadata(meta: &Value) -> Value {
    let mut out = json!({});
    for key in [
        "id",
        "type",
        "name",
        "poster",
        "background",
        "logo",
        "description",
        "releaseInfo",
        "year",
        "runtime",
        "genres",
        "cast",
        "director",
        "imdbRating",
        "contentRating",
        "certification",
        "ageRating",
        "mpaaRating",
    ] {
        if let Some(value) = meta.get(key) {
            out[key] = match value {
                Value::String(text) => json!(text.chars().take(4096).collect::<String>()),
                Value::Array(items) => Value::Array(
                    items
                        .iter()
                        .take(32)
                        .filter(|v| v.is_string())
                        .map(|v| {
                            json!(v
                                .as_str()
                                .unwrap_or("")
                                .chars()
                                .take(256)
                                .collect::<String>())
                        })
                        .collect(),
                ),
                Value::Number(_) | Value::Bool(_) => value.clone(),
                _ => Value::Null,
            };
        }
    }
    if let Some(videos) = meta["videos"].as_array() {
        out["videos"] = Value::Array(
            videos
                .iter()
                .take(2000)
                .map(|video| {
                    let mut item = json!({});
                    for key in [
                        "id",
                        "season",
                        "episode",
                        "title",
                        "released",
                        "thumbnail",
                        "contentRating",
                        "certification",
                        "ageRating",
                        "mpaaRating",
                    ] {
                        if let Some(value) = video.get(key) {
                            item[key] = match value {
                                Value::String(text) => json!(text
                                    .chars()
                                    .take(if key == "thumbnail" { 1024 } else { 512 })
                                    .collect::<String>()),
                                Value::Number(_) | Value::Bool(_) => value.clone(),
                                _ => Value::Null,
                            };
                        }
                    }
                    item
                })
                .collect(),
        );
    }
    out
}
pub(crate) fn observe(
    db: &Connection,
    account: i64,
    kind: &str,
    meta: &Value,
    keep_unknown: bool,
) -> Result<(), ApiError> {
    if !["movie", "series"].contains(&kind) {
        return Ok(());
    }
    let Some(id) = meta["id"]
        .as_str()
        .filter(|v| !v.is_empty() && v.len() <= 512)
    else {
        return Ok(());
    };
    if !meta["name"].is_string() {
        return Ok(());
    }
    let (age, conflict) = assessment(meta);
    if age.is_none() && !keep_unknown && !conflict {
        return Ok(());
    }
    let mut metadata = compact_metadata(meta);
    let previous:Option<(Option<i64>,bool,String)>=db.query_row("SELECT age,conflict,metadata FROM kids_media WHERE account_id=?1 AND kind=?2 AND id=?3",params![account,kind,id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(db_error)?;
    if let Some((old_age, old_conflict, old_text)) = previous {
        let old: Value = serde_json::from_str(&old_text).unwrap_or(Value::Null);
        if metadata.get("videos").is_none() && old["videos"].is_array() {
            metadata["videos"] = old["videos"].clone();
        }
        let old_ids = old["videos"].as_array().map(|v| {
            v.iter()
                .filter_map(|v| v["id"].as_str())
                .collect::<HashSet<_>>()
        });
        let new_ids = metadata["videos"].as_array().map(|v| {
            v.iter()
                .filter_map(|v| v["id"].as_str())
                .collect::<HashSet<_>>()
        });
        if old_age != age || (!old_conflict && conflict) || old_ids != new_ids {
            db.execute("UPDATE kids_profiles SET revision=revision+1 WHERE enabled=1 AND profile_id IN(SELECT profile_id FROM profile_owners WHERE account_id=?1)",[account]).map_err(db_error)?;
        }
    }
    let encoded = metadata.to_string();

    db.execute("INSERT INTO kids_media(account_id,kind,id,parent_id,metadata,age,conflict,updated_at) VALUES(?1,?2,?3,?3,?4,?5,?6,?7) ON CONFLICT(account_id,kind,id) DO UPDATE SET metadata=excluded.metadata,age=COALESCE(MAX(kids_media.age,excluded.age),excluded.age,kids_media.age),conflict=MAX(kids_media.conflict,excluded.conflict,CASE WHEN kids_media.age IS NOT NULL AND excluded.age IS NOT NULL AND kids_media.age<>excluded.age THEN 1 ELSE 0 END),updated_at=excluded.updated_at",params![account,kind,id,encoded,age,conflict,util::now()]).map_err(db_error)?;
    let mut episode_policy_changed = false;
    if kind == "series" {
        // Exact video IDs are learned here, never from a playback/progress request's series_id.
        if let Some(videos) = meta["videos"]
            .as_array()
            .or_else(|| metadata["videos"].as_array())
        {
            for video in videos.iter().take(2000) {
                let Some(episode) = video["id"]
                    .as_str()
                    .filter(|v| !v.is_empty() && v.len() <= 512 && *v != id)
                else {
                    continue;
                };
                let quarantined=db.execute("INSERT OR IGNORE INTO kids_ambiguous(account_id,kind,id) SELECT account_id,kind,id FROM kids_media WHERE account_id=?1 AND kind='series' AND id=?2 AND parent_id<>?3",params![account,episode,id]).map_err(db_error)?;
                episode_policy_changed |= quarantined > 0;
                let (episode_age, episode_conflict) = assessment(video);
                let episode_age = match (age, episode_age) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    (a, b) => a.or(b),
                };
                let previous_episode:Option<(Option<i64>,bool)>=db.query_row("SELECT age,conflict FROM kids_media WHERE account_id=?1 AND kind='series' AND id=?2",params![account,episode],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(db_error)?;
                if previous_episode.is_some_and(|(prior_age, prior_conflict)| {
                    prior_age != episode_age || (!prior_conflict && episode_conflict)
                }) {
                    episode_policy_changed = true;
                }
                let compact = compact_metadata(&json!({"videos":[video]}));
                let mut item = compact["videos"][0].clone();
                item["name"] = metadata["name"].clone();
                item["poster"] = metadata["poster"].clone();
                item["series_id"] = json!(id);
                item["type"] = json!("series");
                db.execute("INSERT INTO kids_media(account_id,kind,id,parent_id,metadata,age,conflict,updated_at) VALUES(?1,'series',?2,?3,?4,?5,?6,?7) ON CONFLICT(account_id,kind,id) DO UPDATE SET metadata=excluded.metadata,age=excluded.age,conflict=MAX(kids_media.conflict,excluded.conflict,CASE WHEN kids_media.parent_id<>excluded.parent_id THEN 1 ELSE 0 END),updated_at=excluded.updated_at",params![account,episode,id,item.to_string(),episode_age,episode_conflict,util::now()]).map_err(db_error)?;
            }
        }
    }
    if episode_policy_changed {
        db.execute("UPDATE kids_profiles SET revision=revision+1 WHERE enabled=1 AND profile_id IN(SELECT profile_id FROM profile_owners WHERE account_id=?1)",[account]).map_err(db_error)?;
    }
    Ok(())
}
fn prune_observations(db: &Connection, account: i64) -> Result<(), ApiError> {
    // Bound once per response, rather than sorting the cache for every catalog item.
    db.execute("DELETE FROM kids_media WHERE account_id=?1 AND rowid NOT IN(SELECT rowid FROM kids_media WHERE account_id=?1 ORDER BY updated_at DESC,rowid DESC LIMIT 10000) AND NOT EXISTS(SELECT 1 FROM kids_approvals a JOIN profile_owners o ON o.profile_id=a.profile_id WHERE o.account_id=?1 AND a.kind=kids_media.kind AND a.id=kids_media.parent_id)",[account]).map_err(db_error)?;
    Ok(())
}
type MediaRecord = (Value, String, Option<i64>, bool);
fn media(
    db: &Connection,
    p: &auth::Principal,
    kind: &str,
    id: &str,
) -> Result<Option<MediaRecord>, ApiError> {
    db.query_row("SELECT metadata,parent_id,age,conflict FROM kids_media WHERE account_id=?1 AND kind=?2 AND id=?3",params![p.account_id(),kind,id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<i64>>(2)?,r.get::<_,bool>(3)?))).optional().map_err(db_error)?.map(|(text,parent,age,conflict)|serde_json::from_str(&text).map(|meta|(meta,parent,age,conflict)).map_err(|_|ApiError::from("Title metadata unavailable"))).transpose()
}
pub(crate) fn allow_item(
    db: &Connection,
    p: &auth::Principal,
    kind: &str,
    id: &str,
) -> Result<bool, ApiError> {
    let auth::Principal::Account { profile_id, .. } = p;
    // Share the collection predicate so episode cards and direct source requests agree.
    // This reads only policy fields instead of reparsing a full series for every episode.
    let allowed = sql_allowed("?1", "?2", "?3");
    db.query_row(
        &format!("SELECT {allowed}"),
        params![kind, id, profile_id],
        |r| r.get(0),
    )
    .map_err(db_error)
}

pub(crate) fn require_item(
    db: &Connection,
    p: &auth::Principal,
    kind: &str,
    id: &str,
) -> Result<(), ApiError> {
    if allow_item(db, p, kind, id)? {
        Ok(())
    } else {
        Err(forbidden())
    }
}
pub(crate) fn filter_items(
    db: &Connection,
    p: &auth::Principal,
    values: Vec<Value>,
) -> Result<Vec<Value>, ApiError> {
    if !restricted(db, p)? {
        return Ok(values);
    }
    let mut safe = Vec::new();
    for mut value in values {
        let kind = value["type"].as_str().unwrap_or("");
        let id = value["id"].as_str().unwrap_or("");
        if allow_item(db, p, kind, id)? {
            if let Some((metadata, _, _, _)) = media(db, p, kind, id)? {
                for key in ["name", "poster", "background", "series_id"] {
                    if !metadata[key].is_null() {
                        value[key] = metadata[key].clone();
                    }
                }
            }
            safe.push(value);
        }
    }
    Ok(safe)
}
fn live_allowed(db: &Connection, id: &str) -> Result<bool, ApiError> {
    if !id.starts_with("family:") {
        return Ok(false);
    }
    let metadata: Option<String> = db
        .query_row("SELECT data FROM family_channels WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .optional()
        .map_err(db_error)?;
    let value: Value = metadata
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    Ok(value["enabled"] == true
        && value["category"]
            .as_str()
            .is_some_and(|s| s.eq_ignore_ascii_case("kids"))
        && value["country"] == "US"
        && value["language"] == "en")
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

fn query(req: &Request) -> HashMap<String, String> {
    url::form_urlencoded::parse(req.uri().query().unwrap_or("").as_bytes())
        .into_owned()
        .collect()
}
fn decode_path(value: &str) -> Result<String, ApiError> {
    let bytes = value.as_bytes();
    let mut output = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err("Invalid path".into());
            }
            let n = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|v| u8::from_str_radix(v, 16).ok())
                .ok_or("Invalid path")?;
            output.push(n);
            i += 3;
        } else {
            output.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(output).map_err(|_| "Invalid path".into())
}
/// A restricted request never falls through to arbitrary addon catalogs or raw provider live listings.
enum PolicyDecision {
    Pass,
    Respond(Value),
    Sanitize,
}
fn decide_request(
    app: &App,
    path: &str,
    method: &axum::http::Method,
    q: &HashMap<String, String>,
) -> Result<PolicyDecision, ApiError> {
    let parts: Vec<_> = path.trim_matches('/').split('/').collect();
    let restricted_now = {
        let db = app.db.lock().unwrap();
        app.request_lease().validate(&db)?;
        restricted(&db, &app.identity())?
    };
    if !restricted_now {
        return Ok(PolicyDecision::Pass);
    }
    {
        let db = app.db.lock().unwrap();
        app.request_lease().validate(&db)?;
        let p = app.identity();
        match parts.as_slice() {
            ["parent", ..] => return Ok(PolicyDecision::Pass),
            ["profiles"] if method == axum::http::Method::GET => return Ok(PolicyDecision::Pass),
            ["profiles", _, "kids" | "approvals"] => {
                require_parent(&db, &p)?;
                return Ok(PolicyDecision::Pass);
            }
            ["profiles", _, "preferences"] => return Ok(PolicyDecision::Pass),
            ["profiles", _, "favorites" | "progress" | "continue", ..] => {}
            ["catalogs"] => {
                return Ok(PolicyDecision::Respond(
                    json!([{"addon_id":0,"id":"kids-movies","type":"movie","name":"Family movies","supports_search":true,"supports_skip":true,"extra":[],"genres":[]},{"addon_id":0,"id":"kids-series","type":"series","name":"Family series","supports_search":true,"supports_skip":true,"extra":[],"genres":[]}]),
                ))
            }
            ["discover"] => {
                let kind = q.get("type").map(String::as_str).unwrap_or("movie");
                let search = q
                    .get("search")
                    .map(|v| v.to_lowercase())
                    .unwrap_or_default();
                let offset = q
                    .get("skip")
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(0)
                    .min(10000);
                let auth::Principal::Account { profile_id, .. } = p;
                let allowed = sql_allowed("catalog.kind", "catalog.id", "?3");
                let predicate = format!("catalog.account_id=?1 AND catalog.kind=?2 AND catalog.id=catalog.parent_id AND instr(lower(json_extract(catalog.metadata,'$.name')),?4)>0 AND {allowed}");
                let total: usize = db
                    .query_row(
                        &format!("SELECT count(*) FROM kids_media catalog WHERE {predicate}"),
                        params![p.account_id(), kind, profile_id, search],
                        |r| r.get(0),
                    )
                    .map_err(db_error)?;
                let mut stmt = db.prepare(&format!("SELECT json_remove(metadata,'$.videos','$.links','$.recommendations') FROM kids_media catalog WHERE {predicate} ORDER BY updated_at DESC,id LIMIT 80 OFFSET ?5")).map_err(db_error)?;
                let values = stmt
                    .query_map(
                        params![p.account_id(), kind, profile_id, search, offset],
                        |r| r.get::<_, String>(0),
                    )
                    .map_err(db_error)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(db_error)?
                    .into_iter()
                    .filter_map(|s| serde_json::from_str::<Value>(&s).ok())
                    .map(|mut v| {
                        v["type"] = json!(kind);
                        v
                    })
                    .collect::<Vec<_>>();
                return Ok(PolicyDecision::Respond(
                    json!({"metas":values,"has_more":offset+80<total,"next_skip":offset+80,"total":total}),
                ));
            }
            ["meta", kind, id] => {
                let id = decode_path(id)?;
                require_item(&db, &p, kind, &id)?;
                let (mut meta, _, _, _) = media(&db, &p, kind, &id)?.ok_or_else(forbidden)?;
                if let Some(videos) = meta["videos"].as_array_mut() {
                    let mut eligible = Vec::new();
                    for video in std::mem::take(videos) {
                        if let Some(episode) = video["id"].as_str() {
                            if allow_item(&db, &p, "series", episode)? {
                                eligible.push(video);
                            }
                        }
                    }
                    *videos = eligible;
                }
                return Ok(PolicyDecision::Respond(json!({"meta":meta})));
            }
            ["live"] => {
                let auth::Principal::Account { profile_id, .. } = p;
                let mut result = live_catalog::browse(
                    &db,
                    Some("Kids"),
                    q.get("search").map(String::as_str),
                    q.get("collection").map(String::as_str),
                    profile_id,
                    0,
                    500,
                )?;
                let rows = result["channels"]
                    .as_array_mut()
                    .map(std::mem::take)
                    .unwrap_or_default();
                let rows = rows
                    .into_iter()
                    .filter(|v| {
                        v["id"]
                            .as_str()
                            .is_some_and(|id| live_allowed(&db, id).unwrap_or(false))
                    })
                    .collect::<Vec<_>>();
                let total = rows.len();
                let offset = q
                    .get("offset")
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(0);
                let limit = q
                    .get("limit")
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(40)
                    .clamp(1, 100);
                return Ok(PolicyDecision::Respond(
                    json!({"channels":rows.into_iter().skip(offset).take(limit).collect::<Vec<_>>(),"total":total}),
                ));
            }
            ["live", "categories"] => {
                return Ok(PolicyDecision::Respond(
                    json!({"categories":[{"id":"category:Kids","name":"Kids"}],"total":1}),
                ))
            }
            ["guide", id] => {
                require_item(&db, &p, "live", &decode_path(id)?)?;
                return Ok(PolicyDecision::Pass);
            }
            ["streams"] | ["playback"] => {}
            ["streams", ..] | ["playback", ..] => return Ok(PolicyDecision::Pass),
            _ => {
                require_parent(&db, &p)?;
                return Ok(PolicyDecision::Pass);
            }
        }
        if method == axum::http::Method::GET {
            if parts.last() == Some(&"series") {
                require_item(
                    &db,
                    &p,
                    "series",
                    q.get("series_id").map(String::as_str).unwrap_or(""),
                )?;
            }
            return Ok(PolicyDecision::Pass);
        }
        if method == axum::http::Method::DELETE {
            return Ok(PolicyDecision::Pass);
        }
        if parts.last() == Some(&"settings") {
            return Ok(PolicyDecision::Pass);
        }
    }
    Ok(PolicyDecision::Sanitize)
}
fn sanitize_request(app: &App, path: &str, mut value: Value) -> Result<Value, ApiError> {
    {
        let db = app.db.lock().unwrap();
        app.request_lease().validate(&db)?;
        let p = app.identity();
        if path == "/playback" {
            if let Some(id) = value["channel_id"].as_str() {
                require_item(&db, &p, "live", id)?;
            }
            // stream_id must have been issued to this exact profile/revision; existing playback authorization checks it.
        } else {
            let kind = value["type"].as_str().unwrap_or("").to_owned();
            let id = value["id"].as_str().unwrap_or("").to_owned();
            require_item(&db, &p, &kind, &id)?;
            if kind != "live" {
                let (meta, parent, _, _) = media(&db, &p, &kind, &id)?.ok_or_else(forbidden)?;
                let object = value.as_object_mut().ok_or("Invalid item")?;
                for key in [
                    "series_id",
                    "season",
                    "episode",
                    "imdb_id",
                    "tmdb_id",
                    "aliases",
                    "year",
                    "name",
                    "poster",
                    "title",
                    "seriesName",
                    "title_id",
                ] {
                    object.remove(key);
                }
                for key in ["name", "poster", "season", "episode"] {
                    if !meta[key].is_null() {
                        value[key] = meta[key].clone();
                    }
                }
                if kind == "series" {
                    value["series_id"] = json!(parent);
                }
                // Never let an approved title's opaque request smuggle an unrelated provider ID.
                if path == "/streams" {
                    let mut safe = json!({"type":kind,"id":id,"name":meta["name"]});
                    for key in ["series_id", "season", "episode"] {
                        if !value[key].is_null() {
                            safe[key] = value[key].clone();
                        }
                    }
                    enrich_matching(&mut safe, &meta);
                    value = safe;
                }
            }
        }
    }
    Ok(value)
}
pub(crate) async fn before(app: &App, req: &mut Request) -> Result<Option<Value>, ApiError> {
    let path = req.uri().path().trim_start_matches("/api").to_owned();
    let method = req.method().clone();
    let q = query(req);
    let decision_app = app.clone();
    let decision_path = path.clone();
    match blocking(move || decide_request(&decision_app, &decision_path, &method, &q)).await? {
        PolicyDecision::Pass => return Ok(None),
        PolicyDecision::Respond(value) => return Ok(Some(value)),
        PolicyDecision::Sanitize => {}
    }
    let bytes = axum::body::to_bytes(
        std::mem::replace(req.body_mut(), axum::body::Body::empty()),
        65536,
    )
    .await
    .map_err(|_| ApiError::from("Request too large"))?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| "Invalid request")?;
    let app = app.clone();
    let value = blocking(move || sanitize_request(&app, &path, value)).await?;
    req.headers_mut().remove(header::CONTENT_LENGTH);
    *req.body_mut() = axum::body::Body::from(value.to_string());
    Ok(None)
}
pub(crate) async fn after(app: &App, path: &str, response: Response) -> Response {
    if !response.status().is_success() || path.contains("/events") {
        return response;
    }
    let sensitive = path.starts_with("/streams")
        || path.starts_with("/playback")
        || path.starts_with("/live")
        || path.starts_with("/guide")
        || path == "/discover"
        || path.starts_with("/meta/")
        || (path.starts_with("/profiles/")
            && (path.contains("/favorites")
                || path.contains("/progress")
                || path.contains("/continue")));
    if !sensitive {
        return response;
    }
    let restricted_now = {
        let db = app.db.lock().unwrap();
        if let Err(error) = app.request_lease().validate(&db) {
            return error.into_response();
        }
        restricted(&db, &app.identity()).unwrap_or(true)
    };
    let observe_response = path == "/discover" || path.starts_with("/meta/");
    let personal = path.starts_with("/profiles/")
        && (path.contains("/favorites")
            || path.contains("/progress")
            || path.contains("/continue"));
    if !observe_response && !(restricted_now && personal) {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let bytes = match axum::body::to_bytes(body, 8 * 1024 * 1024).await {
        Ok(v) => v,
        Err(_) => return ApiError::from("Response too large").into_response(),
    };
    let mut value: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return ApiError::from("Invalid response").into_response(),
    };
    let worker_app = app.clone();
    let worker_path = path.to_owned();
    let result = blocking(move || -> Result<Value, ApiError> {
        let app = worker_app;
        let path = worker_path;
        let mut guard = app.db.lock().unwrap();
        let db = guard.transaction().map_err(db_error)?;
        app.request_lease().validate(&db)?;
        let p = app.identity();
        if !restricted_now && observe_response {
            if let Some(items) = value["metas"].as_array() {
                for item in items.iter().take(500) {
                    observe(
                        &db,
                        p.account_id().unwrap_or(0),
                        item["type"].as_str().unwrap_or("movie"),
                        item,
                        false,
                    )?;
                }
            }
            if value["meta"].is_object() {
                let kind = path.trim_matches('/').split('/').nth(1).unwrap_or("movie");
                observe(
                    &db,
                    p.account_id().unwrap_or(0),
                    kind,
                    &value["meta"],
                    false,
                )?;
            }
        }
        if !restricted_now && observe_response {
            prune_observations(&db, p.account_id().unwrap_or(0))?;
        }
        if restricted_now && personal {
            filter_response_items(&db, &p, &mut value)?;
        }
        db.commit().map_err(db_error)?;
        Ok(value)
    })
    .await;
    let value = match result {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    parts.headers.remove(header::CONTENT_LENGTH);
    parts.headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    Response::from_parts(parts, axum::body::Body::from(value.to_string()))
}
fn filter_response_items(
    db: &Connection,
    p: &auth::Principal,
    value: &mut Value,
) -> Result<(), ApiError> {
    if let Some(items) = value.as_array_mut() {
        *items = filter_items(db, p, std::mem::take(items))?;
        return Ok(());
    }
    if let Some(items) = value["items"].as_array_mut() {
        *items = filter_items(db, p, std::mem::take(items))?;
    }
    if value["item"].is_object() {
        let mut safe = filter_items(db, p, vec![value["item"].clone()])?;
        if let Some(item) = safe.pop() {
            value["item"] = item;
        } else {
            *value = json!({"status":"unavailable"});
        }
    }
    Ok(())
}

/// SQL counterpart used before pagination; arguments are compile-time column/parameter names.
pub(crate) fn sql_allowed(kind: &str, id: &str, profile: &str) -> String {
    format!("(NOT EXISTS(SELECT 1 FROM kids_profiles k0 WHERE k0.profile_id={profile} AND k0.enabled=1) OR EXISTS(SELECT 1 FROM kids_media m JOIN profile_owners o ON o.account_id=m.account_id JOIN kids_profiles k ON k.profile_id=o.profile_id JOIN kids_media root ON root.account_id=m.account_id AND root.kind=m.kind AND root.id=m.parent_id WHERE o.profile_id={profile} AND m.kind={kind} AND m.id={id} AND NOT EXISTS(SELECT 1 FROM kids_ambiguous bad WHERE bad.account_id=m.account_id AND bad.kind=m.kind AND bad.id=m.id) AND COALESCE(m.age,0)<18 AND COALESCE(root.age,0)<18 AND (m.id=m.parent_id OR EXISTS(SELECT 1 FROM json_each(root.metadata,'$.videos') video WHERE json_extract(video.value,'$.id')=m.id)) AND (EXISTS(SELECT 1 FROM kids_approvals a WHERE a.profile_id={profile} AND a.kind=m.kind AND a.id=m.parent_id) OR (m.conflict=0 AND root.conflict=0 AND root.age<=k.max_age AND m.age<=k.max_age))) OR ({kind}='live' AND EXISTS(SELECT 1 FROM family_channels f WHERE f.id={id} AND f.id LIKE 'family:%' AND json_extract(f.data,'$.enabled')=1 AND lower(json_extract(f.data,'$.category'))='kids' AND json_extract(f.data,'$.country')='US' AND json_extract(f.data,'$.language')='en')))")
}

#[derive(Deserialize)]
pub(crate) struct ParentSearch {
    #[serde(rename = "type")]
    kind: String,
    search: String,
}
pub(crate) async fn search(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Query(query): Query<ParentSearch>,
) -> ApiResult {
    let app = app.with_lease(lease);
    if !["movie", "series"].contains(&query.kind.as_str())
        || !(2..=128).contains(&query.search.trim().len())
    {
        return Err("Search for a movie or series using 2 to 128 characters".into());
    }
    {
        let db = app.db.lock().unwrap();
        app.request_lease().validate(&db)?;
        manager(&db, &app.identity(), None)?;
    }
    let result = tokio::time::timeout(
        Duration::from_secs(15),
        app.addons.discover_with_options(addon::DiscoverOptions {
            kind: query.kind.clone(),
            catalog: None,
            addon: None,
            skip: 0,
            search: Some(query.search.trim().into()),
            genre: None,
            extras: HashMap::new(),
        }),
    )
    .await
    .map_err(|_| "Title search timed out")??;
    blocking(move || {
        let mut guard = app.db.lock().unwrap();
        let db = guard.transaction().map_err(db_error)?;
        app.request_lease().validate(&db)?;
        let account = manager(&db, &app.identity(), None)?;
        let mut items = Vec::new();
        for meta in result["metas"].as_array().into_iter().flatten().take(40) {
            if !meta["id"].is_string()
                || !meta["name"].is_string()
                || assessment(meta).0.is_some_and(|age| age >= 18)
            {
                continue;
            }
            observe(&db, account, &query.kind, meta, false)?;
            items.push(
            json!({"id":meta["id"],"type":query.kind,"name":meta["name"],"poster":meta["poster"]}),
        );
        }
        prune_observations(&db, account)?;
        db.commit().map_err(db_error)?;
        Ok(axum::Json(json!({"items":items})))
    })
    .await
}
