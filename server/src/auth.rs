//! Account authentication, browser sessions and explicit device authorization.
//! All bearer credentials are random; only SHA-256 digests are persisted.
use crate::{ApiError, App};
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{
    body::Body,
    extract::{OriginalUri, Query, Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
    Extension, Json, Router,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    sync::LazyLock,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const ACCESS: i64 = 1800;
const REFRESH: i64 = 30 * 86400;
// Device grants persist until explicit revocation; short-lived access tokens still rotate.
const DEVICE_EXPIRY: i64 = 253_402_300_799;
const MAX_PROFILES: i64 = 12;
const MAX_SESSIONS_PER_ACCOUNT: i64 = 32;
const MAX_MEMBER_SESSIONS: i64 = 8_000;
const MAX_SESSIONS: i64 = 10_000;
const MAX_AUTH_BUCKETS: i64 = 10_000;
const MAX_PENDING_PAIRINGS: i64 = 192;
const MAX_PAIRINGS: i64 = 256;
const MAX_APPROVED_PAIRINGS_PER_ACCOUNT: i64 = 8;
const MAX_REFRESH_TOMBSTONES_PER_FAMILY: i64 = 64;
const MAX_REFRESH_TOMBSTONES_PER_ACCOUNT: i64 = 2_048;
const MAX_REFRESH_TOMBSTONES: i64 = 20_000;
const AVATAR_STYLES: &[&str] = &[
    "critters",
    "pixel-art",
    "pixel-art-neutral",
    "moods",
    "thumbs",
    "lorelei",
    "notionists",
    "pixelbot",
    "voxel-bot",
    "sprouts",
    "planets",
    "clay",
    "disney",
    "princesses",
    "animal-friends",
    "villains",
];
#[derive(Clone, Debug)]
pub enum Principal {
    Account {
        account_id: i64,
        role: String,
        profile_id: Option<i64>,
        session_id: Option<String>,
    },
}
impl Principal {
    pub fn key(&self) -> String {
        match self {
            Self::Account {
                account_id,
                profile_id,
                session_id,
                ..
            } => format!(
                "account:{account_id}:profile:{profile_id:?}:family:{}",
                session_id.as_deref().unwrap_or("unbound")
            ),
        }
    }
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::Account { session_id, .. } => session_id.as_deref(),
        }
    }
    /// Revalidate the captured credential family and exact selected profile for media/resources.
    pub fn validate_scope(&self, db: &Connection) -> Result<(), ApiError> {
        let Self::Account {
            account_id,
            role,
            profile_id,
            session_id,
        } = self;
        let sid = session_id.as_deref().ok_or_else(unauthorized)?;
        let valid:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM auth_sessions s JOIN auth_accounts a ON a.id=s.account_id WHERE s.id=?1 AND s.account_id=?2 AND s.profile_id IS ?3 AND s.refresh_expires>?4 AND a.disabled=0 AND ((?5='device' AND s.kind='device') OR (?5=a.role AND s.kind='browser')))",params![sid,account_id,profile_id,now(),role],|r|r.get(0)).map_err(crate::db_error)?;
        if !valid {
            return Err(unauthorized());
        }
        if let Some(profile) = profile_id {
            self.require_profile(db, *profile)?;
        }
        Ok(())
    }
    pub fn can_profile(&self, db: &Connection, profile: i64) -> Result<bool, ApiError> {
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM profile_owners o JOIN profiles p ON p.id=o.profile_id WHERE o.account_id=?1 AND o.profile_id=?2 AND p.presentation_complete=1)",
            params![self.account_id(), profile],
            |r| r.get(0),
        )
        .map_err(crate::db_error)
    }
    pub fn account_id(&self) -> Option<i64> {
        match self {
            Self::Account { account_id, .. } => Some(*account_id),
        }
    }
    pub fn is_owner(&self) -> bool {
        matches!(self, Self::Account {role,..} if role == "owner")
    }
    pub fn require_owner(&self) -> Result<(), ApiError> {
        if self.is_owner() {
            Ok(())
        } else {
            Err(forbidden())
        }
    }
    pub fn require_profile(&self, db: &Connection, profile: i64) -> Result<(), ApiError> {
        let ok = self.can_profile(db, profile)?;
        if ok {
            Ok(())
        } else {
            Err(forbidden())
        }
    }
}
pub fn init(db: &Connection) -> rusqlite::Result<()> {
    required_origin().map_err(|_| rusqlite::Error::InvalidParameterName("VIPTV_AUTH_ORIGIN must be an HTTPS origin without credentials, path, query or fragment".into()))?;
    let tx = db.unchecked_transaction()?;
    init_schema(&tx)?;
    tx.execute(
        "UPDATE auth_sessions SET refresh_expires=?1 WHERE kind='device' AND refresh_expires>?2",
        [DEVICE_EXPIRY, now()],
    )?;
    crate::kids::init(&tx)?;
    tx.commit()
}

fn init_schema(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS schema_migrations(version INTEGER PRIMARY KEY,applied_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS auth_accounts(id INTEGER PRIMARY KEY,username TEXT NOT NULL UNIQUE,name TEXT NOT NULL DEFAULT '',password_hash TEXT NOT NULL,role TEXT NOT NULL CHECK(role IN ('owner','member')),recovery_hash TEXT NOT NULL,disabled INTEGER NOT NULL DEFAULT 0,created_at INTEGER NOT NULL);
CREATE UNIQUE INDEX IF NOT EXISTS auth_one_owner ON auth_accounts(role) WHERE role='owner';
CREATE TABLE IF NOT EXISTS auth_profiles(account_id INTEGER NOT NULL REFERENCES auth_accounts(id) ON DELETE CASCADE,profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,PRIMARY KEY(account_id,profile_id));
CREATE TABLE IF NOT EXISTS auth_sessions(id TEXT PRIMARY KEY,account_id INTEGER NOT NULL REFERENCES auth_accounts(id) ON DELETE CASCADE,access_hash TEXT NOT NULL UNIQUE,refresh_hash TEXT NOT NULL UNIQUE,csrf_hash TEXT NOT NULL,profile_id INTEGER REFERENCES profiles(id) ON DELETE SET NULL,kind TEXT NOT NULL,device_name TEXT NOT NULL,access_expires INTEGER NOT NULL,refresh_expires INTEGER NOT NULL,created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS auth_csrf_tokens(session_id TEXT NOT NULL REFERENCES auth_sessions(id) ON DELETE CASCADE,csrf_hash TEXT NOT NULL,expires INTEGER NOT NULL,created_at INTEGER NOT NULL,PRIMARY KEY(session_id,csrf_hash));
CREATE TABLE IF NOT EXISTS auth_pairings(code_hash TEXT PRIMARY KEY,device_hash TEXT NOT NULL UNIQUE,device_name TEXT NOT NULL,account_id INTEGER REFERENCES auth_accounts(id) ON DELETE CASCADE,profile_id INTEGER REFERENCES profiles(id) ON DELETE CASCADE,expires INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS auth_pairing_profiles(code_hash TEXT NOT NULL REFERENCES auth_pairings(code_hash) ON DELETE CASCADE,profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,PRIMARY KEY(code_hash,profile_id));
CREATE TABLE IF NOT EXISTS auth_device_profiles(session_id TEXT NOT NULL REFERENCES auth_sessions(id) ON DELETE CASCADE,profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,PRIMARY KEY(session_id,profile_id));
CREATE TABLE IF NOT EXISTS auth_refresh_used(hash TEXT PRIMARY KEY,family TEXT NOT NULL,account_id INTEGER,csrf_hash TEXT NOT NULL,kind TEXT NOT NULL,expires INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS auth_onboarding(account_id INTEGER PRIMARY KEY REFERENCES auth_accounts(id) ON DELETE CASCADE,state TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS auth_limits(bucket TEXT PRIMARY KEY,count INTEGER NOT NULL,expires INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS auth_events(id INTEGER PRIMARY KEY,kind TEXT NOT NULL,account_id INTEGER,created_at INTEGER NOT NULL);")?;
    let refresh_columns = db
        .prepare("PRAGMA table_info(auth_refresh_used)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if !refresh_columns.iter().any(|column| column == "account_id") {
        db.execute_batch("ALTER TABLE auth_refresh_used ADD COLUMN account_id INTEGER;")?;
    }
    db.execute_batch("CREATE INDEX IF NOT EXISTS auth_refresh_used_family_expiry ON auth_refresh_used(family,expires);CREATE INDEX IF NOT EXISTS auth_refresh_used_account_expiry ON auth_refresh_used(account_id,expires);")?;
    let has_name = db
        .prepare("PRAGMA table_info(auth_accounts)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|column| column == "name");
    if !has_name {
        db.execute(
            "ALTER TABLE auth_accounts ADD COLUMN name TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }
    db.execute(
        "INSERT OR IGNORE INTO schema_migrations(version,applied_at) VALUES(1,?1)",
        [now()],
    )?;
    db.execute(
        "INSERT OR IGNORE INTO schema_migrations(version,applied_at) VALUES(2,?1)",
        [now()],
    )?;
    db.execute(
        "INSERT OR IGNORE INTO schema_migrations(version,applied_at) VALUES(3,?1)",
        [now()],
    )?;
    let profile_columns = db
        .prepare("PRAGMA table_info(profiles)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    for (column, declaration) in [
        ("avatar_style", "TEXT NOT NULL DEFAULT 'critters'"),
        ("avatar_seed", "TEXT NOT NULL DEFAULT ''"),
        ("presentation_complete", "INTEGER NOT NULL DEFAULT 0"),
        ("created_at", "INTEGER NOT NULL DEFAULT 0"),
        ("updated_at", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        if !profile_columns.iter().any(|existing| existing == column) {
            db.execute_batch(&format!(
                "ALTER TABLE profiles ADD COLUMN {column} {declaration};"
            ))?;
        }
    }
    db.execute_batch("CREATE TABLE IF NOT EXISTS profile_owners(profile_id INTEGER PRIMARY KEY REFERENCES profiles(id) ON DELETE CASCADE,account_id INTEGER NOT NULL REFERENCES auth_accounts(id) ON DELETE CASCADE,created_at INTEGER NOT NULL);")?;
    let ambiguous_profile: Option<i64> = db
        .query_row(
            "SELECT profile_id FROM auth_profiles GROUP BY profile_id HAVING count(DISTINCT account_id)>1 ORDER BY profile_id LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(profile_id) = ambiguous_profile {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "profile {profile_id} has ambiguous legacy account ownership"
        )));
    }
    let inconsistent_profile: Option<i64> = db
        .query_row(
            "SELECT profile_id FROM (
               SELECT ap.profile_id FROM auth_profiles ap
               LEFT JOIN profiles p ON p.id=ap.profile_id
               LEFT JOIN auth_accounts a ON a.id=ap.account_id
               WHERE p.id IS NULL OR a.id IS NULL
               UNION ALL
               SELECT po.profile_id FROM profile_owners po
               LEFT JOIN profiles p ON p.id=po.profile_id
               LEFT JOIN auth_accounts a ON a.id=po.account_id
               WHERE p.id IS NULL OR a.id IS NULL
                  OR EXISTS(SELECT 1 FROM auth_profiles ap WHERE ap.profile_id=po.profile_id AND ap.account_id<>po.account_id)
             ) ORDER BY profile_id LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(profile_id) = inconsistent_profile {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "profile {profile_id} has inconsistent account ownership"
        )));
    }
    db.execute(
        "INSERT OR IGNORE INTO profile_owners(profile_id,account_id,created_at)
         SELECT p.id,
           COALESCE(
             (SELECT ap.account_id FROM auth_profiles ap JOIN auth_accounts a ON a.id=ap.account_id WHERE ap.profile_id=p.id ORDER BY CASE a.role WHEN 'owner' THEN 0 ELSE 1 END,a.id LIMIT 1),
             (SELECT id FROM auth_accounts WHERE role='owner' AND disabled=0 ORDER BY id LIMIT 1)
           ),?1
         FROM profiles p
         WHERE COALESCE(
             (SELECT ap.account_id FROM auth_profiles ap JOIN auth_accounts a ON a.id=ap.account_id WHERE ap.profile_id=p.id ORDER BY CASE a.role WHEN 'owner' THEN 0 ELSE 1 END,a.id LIMIT 1),
             (SELECT id FROM auth_accounts WHERE role='owner' AND disabled=0 ORDER BY id LIMIT 1)
           ) IS NOT NULL",
        [now()],
    )?;
    db.execute(
        "UPDATE profiles SET avatar_seed=lower(hex(randomblob(16))) WHERE avatar_seed=''",
        [],
    )?;
    db.execute(
        "UPDATE profiles SET created_at=?1 WHERE created_at=0",
        [now()],
    )?;
    db.execute(
        "UPDATE profiles SET updated_at=created_at WHERE updated_at=0",
        [],
    )?;
    db.execute(
        "INSERT OR IGNORE INTO schema_migrations(version,applied_at) VALUES(4,?1)",
        [now()],
    )?;
    Ok(())
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
fn token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}
fn hash(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}
fn avatar_style(value: Option<&str>) -> Result<&str, ApiError> {
    let style = value.unwrap_or("critters");
    if AVATAR_STYLES.contains(&style) {
        Ok(style)
    } else {
        Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Unsupported avatar style".into(),
        ))
    }
}
fn character_avatars() -> &'static Value {
    static CATALOG: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str(include_str!("../assets/character-avatars.json"))
            .expect("bundled character catalog")
    })
}
fn avatar_url(style: &str, seed: &str) -> String {
    if let Some(items) = character_avatars().get(style).and_then(Value::as_array) {
        let choice = seed
            .strip_prefix(&format!("viptv-{style}-"))
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(1);
        return items.get(choice.saturating_sub(1)).unwrap_or(&items[0])["url"]
            .as_str()
            .unwrap_or_default()
            .to_string();
    }
    format!("https://api.dicebear.com/10.x/{style}/png?seed={seed}&size=256")
}
fn selected_avatar_seed(value: &Value, style: &str, fallback: String) -> Result<String, ApiError> {
    if let Some(items) = character_avatars().get(style).and_then(Value::as_array) {
        let choice = value
            .get("avatar_choice")
            .and_then(Value::as_u64)
            .or_else(|| {
                fallback
                    .strip_prefix(&format!("viptv-{style}-"))
                    .and_then(|v| v.parse().ok())
            })
            .unwrap_or(1);
        if choice == 0 || choice as usize > items.len() {
            return Err("Invalid avatar_choice".into());
        }
        return Ok(format!("viptv-{style}-{choice}"));
    }
    Ok(value
        .get("avatar_choice")
        .and_then(Value::as_u64)
        .map_or(fallback, |choice| format!("viptv-{style}-{choice}")))
}
fn profile_json(id: i64, name: &str, style: &str, seed: &str, complete: bool) -> Value {
    json!({
        "id":id.to_string(),
        "name":name,
        "avatar_style":style,
        "avatar_choice":seed.strip_prefix(&format!("viptv-{style}-")).and_then(|id| id.parse::<u64>().ok()).filter(|id| (1..=48).contains(id)),
        "avatar_url":avatar_url(style,seed),
        "setup_complete":complete
    })
}
fn validate_profile_payload(value: &Value, update: bool) -> Result<(), ApiError> {
    let object = value.as_object().ok_or("Invalid profile")?;
    if object.keys().any(|key| {
        !matches!(
            key.as_str(),
            "name" | "avatar_style" | "avatar_choice" | "setup_complete"
        )
    }) {
        return Err("Unknown profile field".into());
    }
    if update && object.is_empty() {
        return Err("Empty profile update".into());
    }
    if !update && !object.contains_key("name") {
        return Err("Missing name".into());
    }
    if !update && object.contains_key("setup_complete") {
        return Err("setup_complete is only valid when updating an imported profile".into());
    }
    if let Some(name) = object.get("name") {
        let name = name.as_str().ok_or("Invalid name")?;
        if name.trim().is_empty() || name.len() > 80 {
            return Err("Invalid name".into());
        }
    }
    if let Some(style) = object.get("avatar_style") {
        avatar_style(Some(style.as_str().ok_or("Invalid avatar_style")?))?;
    }
    if let Some(choice) = object.get("avatar_choice") {
        if !choice.as_u64().is_some_and(|id| (1..=48).contains(&id)) {
            return Err("Invalid avatar_choice".into());
        }
    }
    if let Some(complete) = object.get("setup_complete") {
        if complete != &Value::Bool(true) {
            return Err("Invalid setup_complete".into());
        }
    }
    Ok(())
}
pub(crate) fn require_household_manager(principal: &Principal) -> Result<(), ApiError> {
    if matches!(principal, Principal::Account {role,..} if role == "device") {
        return Err(forbidden());
    }
    Ok(())
}
pub(crate) fn primary_profile(db: &Connection, account: i64) -> Result<Option<i64>, ApiError> {
    db.query_row(
        "SELECT min(profile_id) FROM profile_owners WHERE account_id=?1",
        [account],
        |r| r.get(0),
    )
    .map_err(crate::db_error)
}
fn with_primary(db: &Connection, account: i64, mut value: Value) -> Result<Value, ApiError> {
    value["is_primary"] = json!(
        value["id"].as_str().and_then(|v| v.parse::<i64>().ok()) == primary_profile(db, account)?
    );
    if let Some(id) = value["id"].as_str().and_then(|v| v.parse::<i64>().ok()) {
        let policy = crate::kids::profile_fields(db, id)?;
        value["kids"] = policy["enabled"].clone();
        value["max_age"] = policy["max_age"].clone();
    }
    Ok(value)
}
pub(crate) fn list_profiles(db: &Connection, account_id: i64) -> Result<Vec<Value>, ApiError> {
    let mut statement = db
        .prepare(
            "SELECT p.id,p.name,p.avatar_style,p.avatar_seed,p.presentation_complete
         FROM profiles p JOIN profile_owners o ON o.profile_id=p.id
         WHERE o.account_id=?1 ORDER BY p.id",
        )
        .map_err(crate::db_error)?;
    let rows = statement
        .query_map([account_id], |row| {
            let id = row.get::<_, i64>(0)?;
            let name = row.get::<_, String>(1)?;
            let style = row.get::<_, String>(2)?;
            let seed = row.get::<_, String>(3)?;
            let complete = row.get::<_, bool>(4)?;
            Ok(profile_json(id, &name, &style, &seed, complete))
        })
        .map_err(crate::db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(crate::db_error)?;
    rows.into_iter()
        .map(|value| with_primary(db, account_id, value))
        .collect()
}
pub(crate) fn create_profile(
    db: &Connection,
    account_id: i64,
    value: &Value,
) -> Result<Value, ApiError> {
    validate_profile_payload(value, false)?;
    let name = crate::text(value, "name", 80)?;
    let style = avatar_style(value.get("avatar_style").and_then(Value::as_str))?;
    let count: i64 = db
        .query_row(
            "SELECT count(*) FROM profile_owners WHERE account_id=?1",
            [account_id],
            |row| row.get(0),
        )
        .map_err(crate::db_error)?;
    if count >= MAX_PROFILES {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Profile limit reached".into(),
        ));
    }
    let seed = selected_avatar_seed(value, style, Uuid::new_v4().simple().to_string())?;
    let timestamp = now();
    db.execute("INSERT INTO profiles(name,avatar_style,avatar_seed,presentation_complete,created_at,updated_at) VALUES(?1,?2,?3,1,?4,?4)",params![name,style,seed,timestamp]).map_err(crate::db_error)?;
    let profile_id = db.last_insert_rowid();
    db.execute(
        "INSERT INTO profile_owners(profile_id,account_id,created_at) VALUES(?1,?2,?3)",
        params![profile_id, account_id, timestamp],
    )
    .map_err(crate::db_error)?;
    with_primary(
        db,
        account_id,
        profile_json(profile_id, name, style, &seed, true),
    )
}
pub(crate) fn update_profile(
    db: &Connection,
    account_id: i64,
    profile_id: i64,
    value: &Value,
) -> Result<Value, ApiError> {
    validate_profile_payload(value, true)?;
    let current=db.query_row("SELECT p.name,p.avatar_style,p.avatar_seed,p.presentation_complete FROM profiles p JOIN profile_owners o ON o.profile_id=p.id WHERE p.id=?1 AND o.account_id=?2",params![profile_id,account_id],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,bool>(3)?))).optional().map_err(crate::db_error)?.ok_or_else(forbidden)?;
    if !current.3
        && (!value.get("name").is_some_and(Value::is_string)
            || !value.get("avatar_style").is_some_and(Value::is_string)
            || value.get("setup_complete") != Some(&Value::Bool(true)))
    {
        return Err(
            "Incomplete profile setup requires name, avatar_style, and setup_complete=true".into(),
        );
    }
    let name = match value.get("name") {
        Some(_) => crate::text(value, "name", 80)?.to_owned(),
        None => current.0,
    };
    let style = match value.get("avatar_style") {
        Some(style) => avatar_style(Some(style.as_str().ok_or("Invalid avatar_style")?))?,
        None => avatar_style(Some(&current.1))?,
    };
    let complete = current.3 || value.get("setup_complete") == Some(&Value::Bool(true));
    let seed = selected_avatar_seed(value, style, current.2)?;
    db.execute("UPDATE profiles SET name=?1,avatar_style=?2,presentation_complete=?3,updated_at=?4,avatar_seed=?6 WHERE id=?5",params![name,style,complete,now(),profile_id,seed]).map_err(crate::db_error)?;
    with_primary(
        db,
        account_id,
        profile_json(profile_id, &name, style, &seed, complete),
    )
}
pub(crate) fn unauthorized() -> ApiError {
    ApiError(StatusCode::UNAUTHORIZED, "Unauthorized".into())
}
fn forbidden() -> ApiError {
    ApiError(StatusCode::FORBIDDEN, "Forbidden".into())
}
fn auth_error(error: ApiError) -> Response {
    let code = if error.1 == "Parent PIN required" {
        "parent_required"
    } else if error.1 == "Refresh token reuse detected" {
        "refresh_reuse"
    } else if error.1 == "authorization_pending" {
        "authorization_pending"
    } else {
        match error.0 {
            StatusCode::UNAUTHORIZED => "unauthorized",
            StatusCode::FORBIDDEN => "forbidden",
            StatusCode::CONFLICT => "conflict",
            StatusCode::TOO_MANY_REQUESTS => "rate_limited",
            StatusCode::BAD_REQUEST => "invalid_request",
            _ => "auth_error",
        }
    };
    let mut r = (error.0, Json(json!({"error":error.1,"error_code":code}))).into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}
fn constant_eq(a: &str, b: &str) -> bool {
    let mut d = a.len() ^ b.len();
    for (i, c) in b.bytes().enumerate() {
        d |= (a.as_bytes().get(i).copied().unwrap_or(0) ^ c) as usize;
    }
    d == 0
}
fn bearer(h: &HeaderMap) -> Option<&str> {
    h.get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}
fn cookie<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|v| v.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}
fn parse_origin(value: &str) -> Result<url::Url, ApiError> {
    let u = url::Url::parse(value).map_err(|_| forbidden())?;
    if u.scheme() != "https"
        || u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.path() != "/"
        || u.query().is_some()
        || u.fragment().is_some()
    {
        return Err(forbidden());
    }
    Ok(u)
}
fn configured_origin() -> Result<Option<url::Url>, ApiError> {
    match std::env::var("VIPTV_AUTH_ORIGIN") {
        Ok(value) if value.trim().is_empty() => Ok(None),
        Ok(value) => parse_origin(&value).map(Some),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(_) => Err(forbidden()),
    }
}
fn required_origin() -> Result<Option<url::Url>, ApiError> {
    let configured = configured_origin()?;
    // Production/release builds fail at startup and on QR generation without a
    // pinned HTTPS origin. Debug builds retain Host fallback only for isolated
    // in-memory integration tests and explicit local development.
    if configured.is_none() && !cfg!(debug_assertions) {
        return Err(ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "VIPTV_AUTH_ORIGIN is required".into(),
        ));
    }
    Ok(configured)
}
fn origin(h: &HeaderMap) -> Result<(), ApiError> {
    check_origin(h, required_origin()?.as_ref())
}
fn check_origin(h: &HeaderMap, configured: Option<&url::Url>) -> Result<(), ApiError> {
    if h.get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == "cross-site")
    {
        return Err(forbidden());
    }
    if let Some(o) = h.get(header::ORIGIN) {
        let u = parse_origin(o.to_str().map_err(|_| forbidden())?)?;
        let expected = if let Some(configured) = configured {
            configured.clone()
        } else {
            let host = h
                .get(header::HOST)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(forbidden)?;
            parse_origin(&format!("https://{host}"))?
        };
        if u.origin() != expected.origin() {
            return Err(forbidden());
        }
    }
    Ok(())
}
fn csrf_for_session(
    db: &Connection,
    h: &HeaderMap,
    session_id: &str,
    primary: &str,
) -> Result<(), ApiError> {
    if !h.contains_key(header::ORIGIN) {
        return Err(forbidden());
    }
    origin(h)?;
    let supplied = h
        .get("x-csrf-token")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(forbidden)?;
    let supplied_hash = hash(supplied);
    if constant_eq(&supplied_hash, primary) {
        return Ok(());
    }
    let valid: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM auth_csrf_tokens WHERE session_id=?1 AND csrf_hash=?2 AND expires>?3)",
        params![session_id,supplied_hash,now()],
        |row| row.get(0),
    ).map_err(crate::db_error)?;
    if valid {
        Ok(())
    } else {
        Err(forbidden())
    }
}
fn issue_csrf(db: &Connection, session_id: &str) -> Result<String, ApiError> {
    db.execute("DELETE FROM auth_csrf_tokens WHERE expires<=?1", [now()])
        .map_err(crate::db_error)?;
    let raw = token();
    db.execute(
        "INSERT INTO auth_csrf_tokens(session_id,csrf_hash,expires,created_at) VALUES(?1,?2,?3,?4)",
        params![session_id, hash(&raw), now() + REFRESH, now()],
    )
    .map_err(crate::db_error)?;
    db.execute(
        "DELETE FROM auth_csrf_tokens WHERE rowid IN(SELECT rowid FROM auth_csrf_tokens WHERE session_id=?1 ORDER BY created_at DESC,rowid DESC LIMIT -1 OFFSET 16)",
        [session_id],
    ).map_err(crate::db_error)?;
    Ok(raw)
}
fn public(path: &str, method: &Method) -> bool {
    let canonical = if path.starts_with("/device/") {
        format!("/auth{path}")
    } else {
        path.to_owned()
    };
    let path = canonical.as_str();
    (method == Method::GET && matches!(path, "/auth/status" | "/auth/device/qr"))
        || (method == Method::POST
            && matches!(
                path,
                "/auth/register"
                    | "/auth/login"
                    | "/auth/refresh"
                    | "/auth/recover"
                    | "/auth/device/code"
                    | "/auth/device/token"
                    | "/auth/device/refresh"
            ))
}
pub async fn authenticate(State(app): State<App>, mut req: Request, next: Next) -> Response {
    let path = req
        .uri()
        .path()
        .strip_prefix("/api")
        .unwrap_or(req.uri().path());
    if public(path, req.method()) {
        return next.run(req).await;
    }
    let headers = req.headers().clone();
    let method = req.method().clone();
    let result = tokio::task::spawn_blocking(move || {
        let from_cookie = bearer(&headers).is_none();
        let t = bearer(&headers)
            .or_else(|| cookie(&headers, "viptv_session"))
            .ok_or_else(unauthorized)?;
        let db = app.db.lock().map_err(|_| unauthorized())?;
        let row=db.query_row("SELECT a.id,a.role,s.profile_id,s.csrf_hash,s.kind,s.id FROM auth_sessions s JOIN auth_accounts a ON a.id=s.account_id WHERE s.access_hash=?1 AND s.access_expires>?2 AND a.disabled=0",params![hash(t),now()],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<i64>>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?))).optional().map_err(crate::db_error)?.ok_or_else(unauthorized)?;
        if from_cookie && row.4 != "browser" {
            return Err(unauthorized());
        }
        if from_cookie && !matches!(method, Method::GET | Method::HEAD | Method::OPTIONS) {
            csrf_for_session(&db, &headers, &row.5, &row.3)?;
        }
        Ok(Principal::Account {
            account_id: row.0,
            role: if row.4 == "device" {
                "device".into()
            } else {
                row.1
            },
            profile_id: row.2,
            session_id:Some(row.5),
        })
    }).await.unwrap_or_else(|_|Err(unauthorized()));
    match result {
        Ok(p) => {
            req.extensions_mut().insert(p);
            next.run(req).await
        }
        Err(e) => auth_error(e),
    }
}
/// Use this variant when mounting outside an already authenticated API router.
pub fn router_with_auth(app: App) -> Router<App> {
    router().route_layer(axum::middleware::from_fn_with_state(app, authenticate))
}
pub fn router() -> Router<App> {
    let mut r = Router::new()
        .route("/auth/status", get(status))
        .route("/auth/device/qr", get(device_qr));
    for p in [
        "register",
        "login",
        "refresh",
        "recover",
        "logout",
        "profile",
        "device/code",
        "device/lookup",
        "device/approve",
        "device/token",
        "device/refresh",
        "device/revoke",
    ] {
        r = r.route(&format!("/auth/{p}"), post(action));
    }
    for p in ["me", "accounts", "devices", "events"] {
        r = r.route(&format!("/auth/{p}"), get(info));
    }
    for p in ["code", "token", "refresh", "lookup", "approve", "deny"] {
        r = r.route(&format!("/device/{p}"), post(action));
    }
    r.route("/auth/sessions", get(info).delete(remove))
        .route("/devices", get(info))
        .route("/devices/:id", axum::routing::delete(remove))
        .route("/admin/devices", get(info))
        .route("/admin/devices/:id", axum::routing::delete(remove))
        .route("/accounts", get(info))
        .route("/onboarding", get(info).patch(action))
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
        .layer(axum::middleware::from_fn(no_store))
}
async fn no_store(req: Request, next: Next) -> Response {
    let mut r = next.run(req).await;
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

async fn remove(
    state: State<App>,
    uri: OriginalUri,
    p: Option<Extension<Principal>>,
    h: HeaderMap,
) -> Response {
    action(state, uri, p, h, Json(json!({}))).await
}
async fn status(State(app): State<App>, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    crate::blocking(move || {
        let db = app.db.lock().map_err(|_| unauthorized())?;
        let access_valid = cookie(&headers, "viptv_session").is_some_and(|access| {
            db.query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_sessions s JOIN auth_accounts a ON a.id=s.account_id WHERE s.access_hash=?1 AND s.access_expires>?2 AND s.kind='browser' AND a.disabled=0)",
                params![hash(access), now()],
                |r| r.get::<_, bool>(0),
            ).unwrap_or(false)
        });
        let refresh_valid = cookie(&headers, "viptv_refresh").is_some_and(|refresh| {
            db.query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_sessions s JOIN auth_accounts a ON a.id=s.account_id WHERE s.refresh_hash=?1 AND s.refresh_expires>?2 AND s.kind='browser' AND a.disabled=0)",
                params![hash(refresh), now()],
                |r| r.get::<_, bool>(0),
            ).unwrap_or(false)
        });
        let authenticated = access_valid || refresh_valid;
        let session_id = if access_valid {
            cookie(&headers, "viptv_session").and_then(|access| db.query_row(
                "SELECT id FROM auth_sessions WHERE access_hash=?1 AND kind='browser'",
                [hash(access)], |row| row.get::<_,String>(0),
            ).optional().ok().flatten())
        } else if refresh_valid {
            cookie(&headers, "viptv_refresh").and_then(|refresh| db.query_row(
                "SELECT id FROM auth_sessions WHERE refresh_hash=?1 AND kind='browser'",
                [hash(refresh)], |row| row.get::<_,String>(0),
            ).optional().ok().flatten())
        } else { None };
        let csrf_token = if let Some(session_id) = session_id {
            Some(issue_csrf(&db, &session_id)?)
        } else { None };
        Ok(Json(json!({
            "registration_enabled": true,
            "authenticated": authenticated,
            "csrf_token": csrf_token
        })))
    })
    .await
}
#[derive(Deserialize)]
struct QrQuery {
    code: String,
}
async fn device_qr(
    State(app): State<App>,
    headers: HeaderMap,
    Query(query): Query<QrQuery>,
) -> Result<Response, ApiError> {
    crate::blocking(move || {
        let code = query.code.trim().to_uppercase();
        if code.len() != 10 || !code.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            return Err(ApiError(
                StatusCode::BAD_REQUEST,
                "Invalid device code".into(),
            ));
        }
        let db = app.db.lock().map_err(|_| unauthorized())?;
        let exists: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_pairings WHERE code_hash=?1 AND expires>?2)",
                params![hash(&code), now()],
                |row| row.get(0),
            )
            .map_err(crate::db_error)?;
        if !exists {
            return Err(ApiError(
                StatusCode::NOT_FOUND,
                "Invalid or expired pairing code".into(),
            ));
        }
        let origin = required_origin()?
            .map(|value| value.to_string().trim_end_matches('/').to_owned())
            .or_else(|| {
                headers
                    .get(header::HOST)
                    .and_then(|value| value.to_str().ok())
                    .map(|host| format!("https://{host}"))
            })
            .ok_or_else(forbidden)?;
        let target = format!("{origin}/device?code={code}");
        let qr = qrcode::QrCode::new(target.as_bytes()).map_err(|_| {
            ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "QR generation failed".into(),
            )
        })?;
        let quiet = 4usize;
        let scale = 8usize;
        let modules = qr.width();
        let side = (modules + quiet * 2) * scale;
        let colors = qr.to_colors();
        let mut pixels = vec![255u8; side * side];
        for y in 0..modules {
            for x in 0..modules {
                if colors[y * modules + x] == qrcode::Color::Dark {
                    let start_y = (y + quiet) * scale;
                    let start_x = (x + quiet) * scale;
                    for py in start_y..start_y + scale {
                        pixels[py * side + start_x..py * side + start_x + scale].fill(0);
                    }
                }
            }
        }
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, side as u32, side as u32);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().map_err(|_| {
                ApiError(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "QR generation failed".into(),
                )
            })?;
            writer.write_image_data(&pixels).map_err(|_| {
                ApiError(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "QR generation failed".into(),
                )
            })?;
        }
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "image/png")
            .header(header::CACHE_CONTROL, "no-store")
            .body(Body::from(png))
            .map_err(|_| {
                ApiError(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "QR response failed".into(),
                )
            })
    })
    .await
}
fn field<'a>(v: &'a Value, k: &str, max: usize) -> Result<&'a str, ApiError> {
    crate::text(v, k, max)
}
fn identifier(v: &Value, key: &str) -> Result<i64, ApiError> {
    v.get(key)
        .and_then(|value| value.as_i64().or_else(|| value.as_str()?.parse().ok()))
        .ok_or_else(|| ApiError::from(format!("Invalid {key}")))
}
fn password(v: &Value) -> Result<String, ApiError> {
    let p = field(v, "password", 256)?;
    if p.len() < 12 {
        return Err("Password must contain at least 12 characters".into());
    }
    Argon2::default()
        .hash_password(p.as_bytes(), &SaltString::generate(&mut OsRng))
        .map(|v| v.to_string())
        .map_err(|_| {
            ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Password hashing failed".into(),
            )
        })
}
pub fn create_owner_offline(
    db: &mut Connection,
    username_value: &str,
    name_value: &str,
    password_value: &str,
) -> Result<String, ApiError> {
    let payload = json!({"username":username_value,"name":name_value,"password":password_value});
    let username = username(&payload)?;
    let name = field(&payload, "name", 80)?.trim();
    if name.is_empty() {
        return Err("Invalid name".into());
    }
    let password_hash = password(&payload)?;
    let recovery = token();
    let tx = db.transaction().map_err(crate::db_error)?;
    let exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM auth_accounts WHERE role='owner')",
            [],
            |row| row.get(0),
        )
        .map_err(crate::db_error)?;
    if exists {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Owner already exists".into(),
        ));
    }
    tx.execute("INSERT INTO auth_accounts(username,name,password_hash,role,recovery_hash,created_at) VALUES(?1,?2,?3,'owner',?4,?5)",params![username,name,password_hash,hash(&recovery),now()]).map_err(crate::db_error)?;
    let account_id = tx.last_insert_rowid();
    tx.execute("INSERT OR IGNORE INTO profile_owners(profile_id,account_id,created_at) SELECT id,?1,?2 FROM profiles WHERE id NOT IN(SELECT profile_id FROM profile_owners)",params![account_id,now()]).map_err(crate::db_error)?;
    event(&tx, "owner_created_offline", Some(account_id))?;
    tx.commit().map_err(crate::db_error)?;
    Ok(recovery)
}
fn username(v: &Value) -> Result<String, ApiError> {
    let s = field(v, "username", 80)?.trim().to_ascii_lowercase();
    if !s
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || b"._-@".contains(&c))
    {
        return Err("Invalid username".into());
    }
    Ok(s)
}
// Accepts either device-flow spelling and normalizes it before hashing.
fn pairing_code(v: &Value) -> Result<String, ApiError> {
    Ok(v.get("user_code")
        .or_else(|| v.get("code"))
        .and_then(Value::as_str)
        .ok_or("Missing user_code")?
        .trim()
        .to_uppercase())
}
fn event(db: &Connection, kind: &str, id: Option<i64>) -> Result<(), ApiError> {
    db.execute(
        "INSERT INTO auth_events(kind,account_id,created_at) VALUES(?1,?2,?3)",
        params![kind, id, now()],
    )
    .map_err(crate::db_error)?;
    db.execute("DELETE FROM auth_events WHERE id NOT IN (SELECT id FROM auth_events ORDER BY id DESC LIMIT 1000)",[]).map_err(crate::db_error)?;
    Ok(())
}
fn rate(db: &Connection, bucket: &str, limit: i64) -> Result<(), ApiError> {
    db.execute("DELETE FROM auth_limits WHERE expires<=?1", [now()])
        .map_err(crate::db_error)?;
    let known: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM auth_limits WHERE bucket=?1)",
            [bucket],
            |row| row.get(0),
        )
        .map_err(crate::db_error)?;
    if !known {
        let count: i64 = db
            .query_row("SELECT count(*) FROM auth_limits", [], |row| row.get(0))
            .map_err(crate::db_error)?;
        if count >= MAX_AUTH_BUCKETS {
            db.execute(
                "DELETE FROM auth_limits WHERE bucket=(SELECT bucket FROM auth_limits WHERE bucket NOT LIKE 'auth:global:%' ORDER BY expires,bucket LIMIT 1)",
                [],
            )
            .map_err(crate::db_error)?;
        }
    }
    db.execute("INSERT INTO auth_limits(bucket,count,expires) VALUES(?1,1,?2) ON CONFLICT(bucket) DO UPDATE SET count=count+1",params![bucket,now()+300]).map_err(crate::db_error)?;
    let count: i64 = db
        .query_row(
            "SELECT count FROM auth_limits WHERE bucket=?1",
            [bucket],
            |r| r.get(0),
        )
        .map_err(crate::db_error)?;
    // Buckets are fixed endpoint names, never attacker-provided identifiers.
    if count > limit {
        event(db, "rate_limited", None)?;
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "Try again later".into(),
        ));
    }
    Ok(())
}
fn session(
    db: &Connection,
    id: i64,
    profile: Option<i64>,
    kind: &str,
    name: &str,
) -> Result<(Value, Option<(String, String)>), ApiError> {
    let (a, r, c) = (token(), token(), token());
    let sid = Uuid::new_v4().to_string();
    db.execute(
        "DELETE FROM auth_sessions WHERE refresh_expires<=?1",
        [now()],
    )
    .map_err(crate::db_error)?;
    let role: String = db
        .query_row(
            "SELECT role FROM auth_accounts WHERE id=?1 AND disabled=0",
            [id],
            |row| row.get(0),
        )
        .map_err(crate::db_error)?;
    let account_count: i64 = db
        .query_row(
            "SELECT count(*) FROM auth_sessions WHERE account_id=?1",
            [id],
            |row| row.get(0),
        )
        .map_err(crate::db_error)?;
    if account_count >= MAX_SESSIONS_PER_ACCOUNT {
        db.execute(
            "DELETE FROM auth_sessions WHERE id IN(SELECT id FROM auth_sessions WHERE account_id=?1 AND kind='browser' ORDER BY created_at,id LIMIT ?2)",
            params![id, account_count - MAX_SESSIONS_PER_ACCOUNT + 1],
        )
        .map_err(crate::db_error)?;
    }
    if role != "owner" {
        let member_count: i64 = db
            .query_row(
                "SELECT count(*) FROM auth_sessions s JOIN auth_accounts a ON a.id=s.account_id WHERE a.role='member'",
                [],
                |row| row.get(0),
            )
            .map_err(crate::db_error)?;
        if member_count >= MAX_MEMBER_SESSIONS {
            db.execute(
                "DELETE FROM auth_sessions WHERE id IN(SELECT s.id FROM auth_sessions s JOIN auth_accounts a ON a.id=s.account_id WHERE a.role='member' AND s.kind='browser' ORDER BY s.created_at,s.id LIMIT ?1)",
                [member_count - MAX_MEMBER_SESSIONS + 1],
            )
            .map_err(crate::db_error)?;
        }
    }
    let total: i64 = db
        .query_row("SELECT count(*) FROM auth_sessions", [], |row| row.get(0))
        .map_err(crate::db_error)?;
    if total >= MAX_SESSIONS {
        db.execute(
            "DELETE FROM auth_sessions WHERE id IN(SELECT s.id FROM auth_sessions s JOIN auth_accounts a ON a.id=s.account_id WHERE a.role='member' AND s.kind='browser' ORDER BY s.created_at,s.id LIMIT ?1)",
            [total - MAX_SESSIONS + 1],
        )
        .map_err(crate::db_error)?;
    }
    let capacity: bool = db.query_row(
        "SELECT (SELECT count(*) FROM auth_sessions WHERE account_id=?1) < ?2 AND (SELECT count(*) FROM auth_sessions) < ?3",
        params![id,MAX_SESSIONS_PER_ACCOUNT,MAX_SESSIONS], |r| r.get(0),
    ).map_err(crate::db_error)?;
    if !capacity {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "Session limit reached. Revoke an unused device first.".into(),
        ));
    }
    db.execute(
        "INSERT INTO auth_sessions(id,account_id,access_hash,refresh_hash,csrf_hash,profile_id,kind,device_name,access_expires,refresh_expires,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            sid,
            id,
            hash(&a),
            hash(&r),
            hash(&c),
            profile,
            kind,
            name,
            now() + ACCESS,
            if kind == "device" { DEVICE_EXPIRY } else { now() + REFRESH },
            now()
        ],
    )
    .map_err(crate::db_error)?;
    let mut v = json!({"session_id":sid,"account_id":id,"profile_id":profile,"csrf_token":c,"expires_in":ACCESS});
    if kind == "device" {
        v["access_token"] = json!(a);
        v["refresh_token"] = json!(r);
        Ok((v, None))
    } else {
        Ok((v, Some((a, r))))
    }
}
fn response(v: Value, cookies: Option<(String, String)>) -> Response {
    let mut r = Json(v).into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Some((a, b)) = cookies {
        for (name, value, age) in [("viptv_session", a, ACCESS), ("viptv_refresh", b, REFRESH)] {
            let age = if value.is_empty() { 0 } else { age };
            r.headers_mut().append(
                header::SET_COOKIE,
                HeaderValue::from_str(&format!(
                    "{name}={value}; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age={age}"
                ))
                .unwrap(),
            );
        }
    }
    r
}
fn canonical(path: &str, v: &mut Value) -> Result<String, ApiError> {
    let parts: Vec<_> = path.trim_matches('/').split('/').collect();
    Ok(match parts.as_slice() {
        ["device", op] => format!("/auth/device/{op}"),
        ["accounts"] => "/auth/accounts".into(),
        ["devices"] => "/auth/devices-own".into(),
        ["devices", id] => {
            v["session_id"] = json!(id);
            "/auth/device/revoke-own".into()
        }
        ["admin", "devices", id] => {
            v["session_id"] = json!(id);
            "/auth/admin/revoke".into()
        }
        ["admin", "devices"] => "/auth/admin/devices".into(),
        ["onboarding"] => "/auth/setup".into(),
        _ => path.into(),
    })
}
// Bound expensive credential work to the host's full hardware parallelism: this
// prevents an auth flood from oversubscribing CPUs without imposing an artificial quota.
static AUTH_CPU: LazyLock<tokio::sync::Semaphore> = LazyLock::new(|| {
    tokio::sync::Semaphore::new(std::thread::available_parallelism().map_or(1, |value| value.get()))
});
async fn acquire_auth_cpu(
    expensive: bool,
    semaphore: &tokio::sync::Semaphore,
    wait: std::time::Duration,
) -> Result<Option<tokio::sync::SemaphorePermit<'_>>, ApiError> {
    if !expensive {
        return Ok(None);
    }
    match tokio::time::timeout(wait, semaphore.acquire()).await {
        Ok(Ok(permit)) => Ok(Some(permit)),
        Ok(Err(_)) | Err(_) => Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "Authentication busy".into(),
        )),
    }
}
struct PreparedAuth {
    new_hash: Option<String>,
    login_hash: Option<String>,
    login_valid: bool,
}
fn login_snapshot(db: &Connection, path: &str, v: &Value) -> Result<Option<String>, ApiError> {
    if path != "/auth/login" {
        return Ok(None);
    }
    db.query_row(
        "SELECT password_hash FROM auth_accounts WHERE username=?1 AND disabled=0",
        [username(v)?],
        |r| r.get(0),
    )
    .optional()
    .map_err(crate::db_error)
}
fn prepare_auth(
    path: &str,
    v: &Value,
    login_hash: Option<String>,
) -> Result<PreparedAuth, ApiError> {
    let needs_hash = matches!(path, "/auth/register" | "/auth/recover");
    let new_hash = if needs_hash { Some(password(v)?) } else { None };
    let login_valid = if path == "/auth/login" {
        // A syntactically valid fixed Argon2id PHC forces the same expensive verifier
        // path for unknown/disabled accounts without creating a usable credential.
        const DUMMY:&str="$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHQxMjM0NTY3OA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let phc = login_hash.as_deref().unwrap_or(DUMMY);
        let candidate = field(v, "password", 256)?;
        let valid = PasswordHash::new(phc).ok().is_some_and(|p| {
            Argon2::default()
                .verify_password(candidate.as_bytes(), &p)
                .is_ok()
        });
        login_hash.is_some() && valid
    } else {
        false
    };
    Ok(PreparedAuth {
        new_hash,
        login_hash,
        login_valid,
    })
}
fn endpoint_global_limit(path: &str) -> i64 {
    match path {
        "/auth/register" => 100,
        "/auth/login" => 600,
        "/auth/recover" => 100,
        "/auth/device/code" => 120,
        "/auth/device/token" | "/auth/device/lookup" => 600,
        "/auth/refresh" | "/auth/device/refresh" => 1_200,
        _ => 600,
    }
}

async fn action(
    State(app): State<App>,
    OriginalUri(uri): OriginalUri,
    p: Option<Extension<Principal>>,
    h: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    let raw_path = uri
        .path()
        .strip_prefix("/api")
        .unwrap_or(uri.path())
        .to_owned();
    let mut v = v;
    let path = match canonical(&raw_path, &mut v) {
        Ok(path) => path,
        Err(error) => return auth_error(error),
    };
    if matches!(
        path.as_str(),
        "/auth/register" | "/auth/login" | "/auth/recover"
    ) && !h.contains_key(header::ORIGIN)
    {
        return auth_error(forbidden());
    }
    if let Err(error) = origin(&h) {
        return auth_error(error);
    }
    let global_app = app.clone();
    let global_bucket = format!("auth:global:{path}");
    let global_limit = endpoint_global_limit(&path);
    let global_result = tokio::task::spawn_blocking(move || {
        let db = global_app.db.lock().map_err(|_| unauthorized())?;
        rate(&db, &global_bucket, global_limit)
    })
    .await;
    match global_result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return auth_error(error),
        Err(_) => {
            return auth_error(ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Authentication admission worker failed".into(),
            ))
        }
    }
    let expensive = matches!(
        path.as_str(),
        "/auth/register" | "/auth/login" | "/auth/recover"
    );
    let permit =
        match acquire_auth_cpu(expensive, &AUTH_CPU, std::time::Duration::from_secs(2)).await {
            Ok(permit) => permit,
            Err(error) => return auth_error(error),
        };
    let result = tokio::task::spawn_blocking(move || {
        let snapshot = {
            let db = app.db.lock().map_err(|_| unauthorized())?;
            let subject = if matches!(
                path.as_str(),
                "/auth/login" | "/auth/recover" | "/auth/register"
            ) {
                username(&v).unwrap_or_else(|_| "invalid-user".into())
            } else if path.contains("device/token") {
                v.get("device_code")
                    .or_else(|| v.get("device_token"))
                    .and_then(Value::as_str)
                    .unwrap_or("invalid-device")
                    .into()
            } else if path.contains("device/refresh") {
                v.get("refresh_token")
                    .and_then(Value::as_str)
                    .unwrap_or("invalid-refresh")
                    .into()
            } else if path.contains("device/code") {
                v.get("device_name")
                    .and_then(Value::as_str)
                    .unwrap_or("unnamed-device")
                    .into()
            } else if let Some(account_id) =
                p.as_ref().and_then(|principal| principal.0.account_id())
            {
                format!("account-{account_id}")
            } else {
                "anonymous".into()
            };
            let bucket = format!("auth:subject:{}:{}", path, hash(&subject));
            rate(
                &db,
                &bucket,
                if path == "/auth/register" {
                    5
                } else if path.contains("lookup") || path.contains("token") {
                    120
                } else {
                    30
                },
            )?;
            login_snapshot(&db, &path, &v)?
        };
        // No SQLite guard exists during any Argon2 work. Expensive actions alone
        // hold a bounded admission permit, and release it before final database work.
        let prepared = prepare_auth(&path, &v, snapshot)?;
        drop(permit);
        let mut db = app.db.lock().map_err(|_| unauthorized())?;
        let tx = db.transaction().map_err(crate::db_error)?;
        let result = dispatch_prepared(&tx, &path, p.map(|p| p.0), &h, &v, &prepared);
        match result {
            Ok(result) => {
                tx.commit().map_err(crate::db_error)?;
                Ok(result)
            }
            Err(e) => {
                if e.1 == "Refresh token reuse detected" {
                    tx.commit().map_err(crate::db_error)?;
                } else {
                    drop(tx);
                }
                event(&db, "auth_rejected", None)?;
                Err(e)
            }
        }
    })
    .await;
    match result {
        Ok(Ok((v, c))) => response(v, c),
        Ok(Err(e)) => auth_error(e),
        Err(_) => ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Authentication worker failed".into(),
        )
        .into_response(),
    }
}
#[cfg(test)]
fn dispatch(
    db: &Connection,
    path: &str,
    p: Option<Principal>,
    h: &HeaderMap,
    v: &Value,
    _fixture_token: &str,
) -> Result<(Value, Option<(String, String)>), ApiError> {
    let prepared = prepare_auth(path, v, login_snapshot(db, path, v)?)?;
    dispatch_prepared(db, path, p, h, v, &prepared)
}
fn dispatch_prepared(
    db: &Connection,
    path: &str,
    p: Option<Principal>,
    h: &HeaderMap,
    v: &Value,
    prepared: &PreparedAuth,
) -> Result<(Value, Option<(String, String)>), ApiError> {
    if matches!(p,Some(Principal::Account{ref role,..}) if role=="device")
        && !matches!(path, "/auth/profile" | "/auth/logout")
    {
        return Err(forbidden());
    }
    if let Some(ref principal) = p {
        if path != "/auth/profile" {
            crate::kids::require_parent(db, principal)?;
        }
    }
    match path {
        "/auth/setup" => {
            let principal = p.as_ref().ok_or_else(unauthorized)?;
            let id = principal.account_id().ok_or_else(forbidden)?;
            if !v.is_object() || v.to_string().len() > 4096 {
                return Err("Invalid onboarding state".into());
            }
            let mut state = db
                .query_row(
                    "SELECT state FROM auth_onboarding WHERE account_id=?1",
                    [id],
                    |r| r.get::<_, String>(0),
                )
                .optional()
                .map_err(crate::db_error)?
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .unwrap_or(json!({}));
            for (k, value) in v.as_object().unwrap() {
                if ![
                    "household_name",
                    "timezone",
                    "language",
                    "completed",
                    "step",
                    "profile_id",
                ]
                .contains(&k.as_str())
                {
                    return Err("Unknown onboarding field".into());
                }
                match k.as_str() {
                    "household_name"
                        if !value
                            .as_str()
                            .is_some_and(|text| !text.trim().is_empty() && text.len() <= 100) =>
                    {
                        return Err("Invalid household_name".into())
                    }
                    "timezone"
                        if !value
                            .as_str()
                            .is_some_and(|text| !text.trim().is_empty() && text.len() <= 100) =>
                    {
                        return Err("Invalid timezone".into())
                    }
                    "language"
                        if !value
                            .as_str()
                            .is_some_and(|text| !text.trim().is_empty() && text.len() <= 35) =>
                    {
                        return Err("Invalid language".into())
                    }
                    "completed" if !value.is_boolean() => return Err("Invalid completed".into()),
                    "step" if !value.as_str().is_some_and(|text| text.len() <= 80) => {
                        return Err("Invalid step".into())
                    }
                    "profile_id"
                        if value
                            .as_i64()
                            .or_else(|| value.as_str()?.parse::<i64>().ok())
                            .is_none() =>
                    {
                        return Err("Invalid profile_id".into())
                    }
                    _ => {}
                }
                state[k] = value.clone();
            }
            db.execute("INSERT INTO auth_onboarding VALUES(?1,?2) ON CONFLICT(account_id) DO UPDATE SET state=excluded.state",params![id,state.to_string()]).map_err(crate::db_error)?;
            Ok((state, None))
        }
        "/auth/sessions" => {
            let principal = p.as_ref().ok_or_else(unauthorized)?;
            let id = principal.account_id().ok_or_else(forbidden)?;
            db.execute(
                "DELETE FROM auth_sessions WHERE account_id=?1 AND kind='browser'",
                [id],
            )
            .map_err(crate::db_error)?;
            event(db, "sessions_revoked", Some(id))?;
            Ok((json!({"ok":true}), Some((String::new(), String::new()))))
        }
        "/auth/device/deny" => {
            let principal = p.as_ref().ok_or_else(unauthorized)?;
            let code = pairing_code(v)?;
            db.execute(
                "DELETE FROM auth_pairings WHERE code_hash=?1",
                [hash(&code)],
            )
            .map_err(crate::db_error)?;
            event(db, "device_denied", principal.account_id())?;
            Ok((json!({"denied":true}), None))
        }
        "/auth/admin/revoke" => {
            p.as_ref().ok_or_else(unauthorized)?.require_owner()?;
            dispatch_prepared(db, "/auth/device/revoke-admin", p, h, v, prepared)
        }
        "/auth/register" => {
            let username = username(v)?;
            let display_name = v
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(&username)
                .trim();
            if display_name.is_empty() || display_name.len() > 80 {
                return Err("Invalid name".into());
            }
            let password_hash = prepared.new_hash.as_ref().ok_or_else(unauthorized)?;
            let recovery = token();
            let inserted = db.execute(
                "INSERT OR IGNORE INTO auth_accounts(username,name,password_hash,role,recovery_hash,created_at) VALUES(?1,?2,?3,'member',?4,?5)",
                params![username,display_name,password_hash,hash(&recovery),now()],
            ).map_err(crate::db_error)?;
            if inserted != 1 {
                return Err(ApiError(
                    StatusCode::CONFLICT,
                    "Username unavailable".into(),
                ));
            }
            let id = db.last_insert_rowid();
            event(db, "account_registered", Some(id))?;
            let (mut out, cookies) = session(db, id, None, "browser", "")?;
            out["recovery_code"] = json!(recovery);
            out["recovery_codes"] = json!([out["recovery_code"].clone()]);
            out["profile_id"] = Value::Null;
            out["profiles"] = json!([]);
            Ok((out, cookies))
        }
        "/auth/login" | "/auth/recover" => {
            let name = username(v)?;
            let row=db.query_row("SELECT id,password_hash,recovery_hash FROM auth_accounts WHERE username=?1 AND disabled=0",[name],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).optional().map_err(crate::db_error)?;
            let recover = path.ends_with("recover");
            let valid = if let Some((_, pass, rc)) = &row {
                if recover {
                    constant_eq(&hash(field(v, "recovery_code", 256)?), rc)
                } else {
                    prepared.login_valid && prepared.login_hash.as_deref() == Some(pass.as_str())
                }
            } else {
                false
            };
            if !valid {
                return Err(unauthorized());
            }
            let id = row.unwrap().0;
            let recovery = if recover {
                let pass = prepared.new_hash.as_ref().ok_or_else(unauthorized)?;
                let rc = token();
                db.execute(
                    "UPDATE auth_accounts SET password_hash=?1,recovery_hash=?2 WHERE id=?3",
                    params![pass, hash(&rc), id],
                )
                .map_err(crate::db_error)?;
                db.execute("DELETE FROM auth_sessions WHERE account_id=?1", [id])
                    .map_err(crate::db_error)?;
                db.execute("DELETE FROM auth_pairings WHERE account_id=?1", [id])
                    .map_err(crate::db_error)?;
                Some(rc)
            } else {
                None
            };
            event(
                db,
                if recover {
                    "account_recovered"
                } else {
                    "login"
                },
                Some(id),
            )?;
            let (mut out, c) = session(db, id, None, "browser", "")?;
            if let Some(rc) = recovery {
                out["recovery_code"] = json!(rc);
                out["recovery_codes"] = json!([out["recovery_code"].clone()]);
            }
            Ok((out, c))
        }
        "/auth/refresh" | "/auth/device/refresh" => {
            let device = path.contains("device");
            let supplied = if device {
                field(v, "refresh_token", 256)?
            } else {
                cookie(h, "viptv_refresh").ok_or_else(unauthorized)?
            };
            let used=db.query_row("SELECT family,csrf_hash FROM auth_refresh_used WHERE hash=?1 AND kind=?2 AND expires>?3",params![hash(supplied),if device{"device"}else{"browser"},now()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional().map_err(crate::db_error)?;
            if let Some((family, c)) = used {
                if !device {
                    csrf_for_session(db, h, &family, &c)?;
                }
                db.execute("DELETE FROM auth_sessions WHERE id=?1", [family])
                    .map_err(crate::db_error)?;
                event(db, "refresh_replay", None)?;
                return Err(ApiError(
                    StatusCode::UNAUTHORIZED,
                    "Refresh token reuse detected".into(),
                ));
            }
            db.execute("DELETE FROM auth_refresh_used WHERE expires<=?1", [now()])
                .map_err(crate::db_error)?;
            let row=db.query_row("SELECT s.id,s.account_id,s.profile_id,s.csrf_hash,s.device_name FROM auth_sessions s JOIN auth_accounts a ON a.id=s.account_id WHERE s.refresh_hash=?1 AND s.refresh_expires>?2 AND s.kind=?3 AND a.disabled=0",params![hash(supplied),now(),if device{"device"}else{"browser"}],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,Option<i64>>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?))).optional().map_err(crate::db_error)?.ok_or_else(unauthorized)?;
            if !device {
                csrf_for_session(db, h, &row.0, &row.3)?;
            }
            db.execute(
                "INSERT INTO auth_refresh_used(hash,family,account_id,csrf_hash,kind,expires) VALUES(?1,?2,?3,?4,?5,?6)",
                params![
                    hash(supplied),
                    row.0,
                    row.1,
                    row.3,
                    if device { "device" } else { "browser" },
                    now() + REFRESH
                ],
            )
            .map_err(crate::db_error)?;
            db.execute(
                "DELETE FROM auth_refresh_used WHERE family=?1 AND rowid NOT IN(SELECT rowid FROM auth_refresh_used WHERE family=?1 ORDER BY expires DESC,rowid DESC LIMIT ?2)",
                params![row.0, MAX_REFRESH_TOMBSTONES_PER_FAMILY],
            )
            .map_err(crate::db_error)?;
            db.execute(
                "DELETE FROM auth_refresh_used WHERE account_id=?1 AND rowid NOT IN(SELECT rowid FROM auth_refresh_used WHERE account_id=?1 ORDER BY expires DESC,rowid DESC LIMIT ?2)",
                params![row.1, MAX_REFRESH_TOMBSTONES_PER_ACCOUNT],
            )
            .map_err(crate::db_error)?;
            let tombstones: i64 = db
                .query_row("SELECT count(*) FROM auth_refresh_used", [], |item| {
                    item.get(0)
                })
                .map_err(crate::db_error)?;
            if tombstones > MAX_REFRESH_TOMBSTONES {
                db.execute(
                    "DELETE FROM auth_refresh_used WHERE rowid IN(SELECT rowid FROM auth_refresh_used ORDER BY expires,rowid LIMIT ?1)",
                    [tombstones - MAX_REFRESH_TOMBSTONES],
                )
                .map_err(crate::db_error)?;
            }
            // Rotate credentials in place: keep the stable family, approved device
            // grants, and other browser tabs' CSRF tokens intact.
            let access = token();
            let refresh = token();
            db.execute(
                "UPDATE auth_sessions SET access_hash=?1,refresh_hash=?2,access_expires=?3,refresh_expires=?4 WHERE id=?5",
                params![hash(&access),hash(&refresh),now()+ACCESS,if device { DEVICE_EXPIRY } else { now()+REFRESH },row.0],
            ).map_err(crate::db_error)?;
            event(db, "session_rotated", Some(row.1))?;
            let mut out = json!({"session_id":row.0,"account_id":row.1,"profile_id":row.2,"expires_in":ACCESS});
            if device {
                out["access_token"] = json!(access);
                out["refresh_token"] = json!(refresh);
                Ok((out, None))
            } else {
                out["csrf_token"] = json!(issue_csrf(db, &row.0)?);
                Ok((out, Some((access, refresh))))
            }
        }
        "/auth/device/code" => {
            db.execute("DELETE FROM auth_pairings WHERE expires<=?1", [now()])
                .map_err(crate::db_error)?;
            let pending: i64 = db
                .query_row(
                    "SELECT count(*) FROM auth_pairings WHERE account_id IS NULL",
                    [],
                    |row| row.get(0),
                )
                .map_err(crate::db_error)?;
            if pending >= MAX_PENDING_PAIRINGS {
                db.execute(
                    "DELETE FROM auth_pairings WHERE code_hash IN(SELECT code_hash FROM auth_pairings WHERE account_id IS NULL ORDER BY expires,code_hash LIMIT ?1)",
                    [pending - MAX_PENDING_PAIRINGS + 1],
                )
                .map_err(crate::db_error)?;
            }
            let total: i64 = db
                .query_row("SELECT count(*) FROM auth_pairings", [], |row| row.get(0))
                .map_err(crate::db_error)?;
            if total >= MAX_PAIRINGS {
                db.execute(
                    "DELETE FROM auth_pairings WHERE code_hash IN(SELECT code_hash FROM auth_pairings ORDER BY account_id IS NOT NULL,expires,code_hash LIMIT ?1)",
                    [total - MAX_PAIRINGS + 1],
                )
                .map_err(crate::db_error)?;
            }
            let user_code = Uuid::new_v4().simple().to_string()[..10].to_uppercase();
            let device_code = token();
            let name = v["device_name"].as_str().unwrap_or("TV").trim();
            if name.is_empty() || name.len() > 100 {
                return Err("Invalid device name".into());
            }
            db.execute("INSERT INTO auth_pairings(code_hash,device_hash,device_name,expires) VALUES(?1,?2,?3,?4)",params![hash(&user_code),hash(&device_code),name,now()+600]).map_err(crate::db_error)?;
            let origin = required_origin()?
                .map(|origin| origin.to_string().trim_end_matches('/').to_owned())
                .or_else(|| {
                    h.get(header::HOST)
                        .and_then(|host| host.to_str().ok())
                        .map(|host| format!("https://{host}"))
                })
                .unwrap_or_else(|| "https://localhost".into());
            let verification_uri = format!("{origin}/device");
            let verification_uri_complete = format!("{verification_uri}?code={user_code}");
            let qr_uri = format!("{origin}/api/auth/device/qr?code={user_code}");
            Ok((
                json!({
                    "device_code": device_code,
                    "user_code": user_code,
                    "verification_uri": verification_uri,
                    "verification_uri_complete": verification_uri_complete,
                    "qr_uri": qr_uri,
                    "expires_in": 600,
                    "interval": 5
                }),
                None,
            ))
        }
        "/auth/device/lookup" => {
            let _principal = p.as_ref().ok_or_else(unauthorized)?;
            let code = pairing_code(v)?;
            let row = db.query_row(
                "SELECT device_name,expires,account_id IS NOT NULL FROM auth_pairings WHERE code_hash=?1 AND expires>?2",
                params![hash(&code), now()],
                |item| Ok((item.get::<_, String>(0)?, item.get::<_, i64>(1)?, item.get::<_, bool>(2)?)),
            ).optional().map_err(crate::db_error)?.ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, "Invalid or expired pairing code".into()))?;
            Ok((
                json!({"user_code":code,"device":{"device_name":row.0,"status":if row.2{"approved"}else{"pending"}},"expires_at":row.1.to_string()}),
                None,
            ))
        }
        "/auth/device/approve" => {
            let principal = p.as_ref().ok_or_else(unauthorized)?;
            if v.get("account_id").is_some() || v.get("profile_ids").is_some() {
                return Err("Pairing scope is determined by the signed-in account".into());
            }
            let id = principal.account_id().ok_or_else(unauthorized)?;
            let code = pairing_code(v)?;
            let code_hash = hash(&code);
            db.execute("DELETE FROM auth_pairings WHERE expires<=?1", [now()])
                .map_err(crate::db_error)?;
            let approved_count: i64 = db
                .query_row(
                    "SELECT count(*) FROM auth_pairings WHERE account_id=?1",
                    [id],
                    |row| row.get(0),
                )
                .map_err(crate::db_error)?;
            if approved_count >= MAX_APPROVED_PAIRINGS_PER_ACCOUNT {
                return Err(ApiError(
                    StatusCode::TOO_MANY_REQUESTS,
                    "Too many approved devices awaiting connection".into(),
                ));
            }
            let changed=db.execute("UPDATE auth_pairings SET account_id=?1,profile_id=NULL WHERE code_hash=?2 AND expires>?3 AND account_id IS NULL",params![id,code_hash,now()]).map_err(crate::db_error)?;
            if changed != 1 {
                return Err(ApiError(
                    StatusCode::BAD_REQUEST,
                    "Invalid or expired pairing code".into(),
                ));
            }
            event(db, "device_approved", Some(id))?;
            Ok((json!({"approved":true,"account_id":id.to_string()}), None))
        }
        "/auth/device/token" => {
            let device_code = v
                .get("device_code")
                .or_else(|| v.get("device_token"))
                .and_then(Value::as_str)
                .ok_or("Missing device_code")?;
            let device_hash = hash(device_code);
            let row=db.query_row("SELECT p.account_id,p.device_name FROM auth_pairings p JOIN auth_accounts a ON a.id=p.account_id WHERE p.device_hash=?1 AND p.expires>?2 AND a.disabled=0",params![device_hash,now()],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?))).optional().map_err(crate::db_error)?.ok_or_else(||ApiError(StatusCode::BAD_REQUEST,"authorization_pending".into()))?;
            let (output, cookies) = session(db, row.0, None, "device", &row.1)?;
            db.execute(
                "DELETE FROM auth_pairings WHERE device_hash=?1",
                [device_hash],
            )
            .map_err(crate::db_error)?;
            event(db, "device_paired", Some(row.0))?;
            Ok((output, cookies))
        }
        "/auth/logout" => {
            let t = bearer(h)
                .or_else(|| cookie(h, "viptv_session"))
                .ok_or_else(unauthorized)?;
            db.execute("DELETE FROM auth_sessions WHERE access_hash=?1", [hash(t)])
                .map_err(crate::db_error)?;
            Ok((json!({"ok":true}), Some((String::new(), String::new()))))
        }
        "/auth/profile" => {
            let principal = p.as_ref().ok_or_else(unauthorized)?;
            let profile = identifier(v, "profile_id")?;
            principal.require_profile(db, profile)?;
            crate::kids::switch_profile(db, principal, profile)?;
            let t = bearer(h)
                .or_else(|| cookie(h, "viptv_session"))
                .ok_or_else(unauthorized)?;
            db.execute(
                "UPDATE auth_sessions SET profile_id=?1 WHERE access_hash=?2",
                params![profile, hash(t)],
            )
            .map_err(crate::db_error)?;
            Ok((json!({"profile_id":profile.to_string()}), None))
        }
        "/auth/device/revoke" | "/auth/device/revoke-own" | "/auth/device/revoke-admin" => {
            let principal = p.as_ref().ok_or_else(unauthorized)?;
            let sid = field(v, "session_id", 100)?;
            let owner = db
                .query_row(
                    "SELECT account_id FROM auth_sessions WHERE id=?1 AND kind='device'",
                    [sid],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(crate::db_error)?
                .ok_or("Unknown device")?;
            if path == "/auth/device/revoke-admin" {
                principal.require_owner()?;
            } else if principal.account_id() != Some(owner) {
                return Err(forbidden());
            }
            db.execute(
                "DELETE FROM auth_sessions WHERE id=?1 AND kind='device'",
                [sid],
            )
            .map_err(crate::db_error)?;
            event(db, "session_revoked", Some(owner))?;
            Ok((json!({"ok":true}), None))
        }
        _ => Err(ApiError(
            StatusCode::NOT_FOUND,
            "Unknown authentication endpoint".into(),
        )),
    }
}
async fn info(
    State(app): State<App>,
    OriginalUri(uri): OriginalUri,
    Extension(p): Extension<Principal>,
) -> Result<Response, ApiError> {
    crate::blocking(move || {
    let db = app.db.lock().map_err(|_| unauthorized())?;
    let path = uri.path().strip_prefix("/api").unwrap_or(uri.path());
    let mut args = json!({});
    let canonical = canonical(path, &mut args)?;
    let path = canonical.as_str();
    if matches!(&p,Principal::Account{role,..} if role=="device") && path != "/auth/me" {
        return Err(forbidden());
    }
    if path != "/auth/me" {crate::kids::require_parent(&db,&p)?;}
    let v = match path {
        "/auth/setup" => {
            let id = p.account_id().ok_or_else(forbidden)?;
            db.query_row(
                "SELECT state FROM auth_onboarding WHERE account_id=?1",
                [id],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(crate::db_error)?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(json!({"step":"profile","completed":false}))
        }
        "/auth/me" => {
            let Principal::Account { account_id, role, profile_id, .. } = &p;
            let account = db.query_row(
                "SELECT username,name,role FROM auth_accounts WHERE id=?1",
                [account_id],
                |row| Ok(json!({"id":account_id.to_string(),"username":row.get::<_,String>(0)?,"name":row.get::<_,String>(1)?,"role":row.get::<_,String>(2)?})),
            ).map_err(crate::db_error)?;
            let profiles = list_profiles(&db, *account_id)?;
            let csrf_token = if role != "device" {
                p.session_id().map(|session_id| issue_csrf(&db, session_id)).transpose()?
            } else { None };
            let presentation_required = profiles.is_empty() || profiles.iter().any(|profile| profile["setup_complete"] != true);
            json!({
                "account_id":account_id.to_string(),
                "account":account,
                "user":account,
                "role":role,
                "profile_id":profile_id.map(|value| value.to_string()),
                "restricted":crate::kids::restricted(&db,&p)?,
                "profiles":profiles,
                "capabilities":{"create_profiles":true,"can_create_profile":true,"manage_profiles":true,"can_manage_profiles":true},
                "can_create_profile":true,
                "can_manage_profiles":true,
                "profile_setup_required":presentation_required,
                "csrf_token":csrf_token
            })
        }
        "/auth/accounts" => {
            p.require_owner()?;
            let mut s = db
                .prepare("SELECT id,username,name,role,disabled FROM auth_accounts ORDER BY id")
                .map_err(crate::db_error)?;
            let rows=s.query_map([],|r|Ok(json!({"id":r.get::<_,i64>(0)?.to_string(),"username":r.get::<_,String>(1)?,"name":r.get::<_,String>(2)?,"role":r.get::<_,String>(3)?,"disabled":r.get::<_,bool>(4)?,"enabled":!r.get::<_,bool>(4)?}))).map_err(crate::db_error)?.collect::<Result<Vec<_>,_>>().map_err(crate::db_error)?;
            json!(rows)
        }
        "/auth/devices" | "/auth/devices-own" | "/auth/admin/devices" | "/auth/sessions" => {
            let admin_devices = path == "/auth/admin/devices";
            if admin_devices {
                p.require_owner()?;
            }
            let kind = if path == "/auth/sessions" {
                "browser"
            } else {
                "device"
            };
            let mut statement=db.prepare("SELECT id,account_id,device_name,kind,profile_id,created_at FROM auth_sessions WHERE (?1 OR account_id=?2) AND refresh_expires>?3 AND kind=?4 ORDER BY created_at DESC LIMIT 1000").map_err(crate::db_error)?;
            let rows=statement.query_map(params![admin_devices,p.account_id(),now(),kind],|row|{
                let session_id=row.get::<_,String>(0)?;
                Ok(json!({"id":session_id,"session_id":session_id,"account_id":row.get::<_,i64>(1)?.to_string(),"device_name":row.get::<_,String>(2)?,"kind":row.get::<_,String>(3)?,"profile_id":row.get::<_,Option<i64>>(4)?.map(|value|value.to_string()),"created_at":row.get::<_,i64>(5)?.to_string()}))
            }).map_err(crate::db_error)?.collect::<Result<Vec<_>,_>>().map_err(crate::db_error)?;
            if path == "/auth/sessions" {
                json!({"sessions":rows})
            } else if admin_devices || path == "/auth/devices-own" {
                json!(rows)
            } else {
                json!({"devices":rows})
            }
        }
        "/auth/events" => {
            p.require_owner()?;
            let mut s = db
                .prepare(
                    "SELECT kind,account_id,created_at FROM auth_events ORDER BY id DESC LIMIT 100",
                )
                .map_err(crate::db_error)?;
            let rows=s.query_map([],|r|Ok(json!({"kind":r.get::<_,String>(0)?,"account_id":r.get::<_,Option<i64>>(1)?,"created_at":r.get::<_,i64>(2)?}))).map_err(crate::db_error)?.collect::<Result<Vec<_>,_>>().map_err(crate::db_error)?;
            json!({"events":rows})
        }
        _ => return Err(ApiError(StatusCode::NOT_FOUND, "Unknown endpoint".into())),
    };
    Ok(response(v, None))
    }).await
}

#[cfg(test)]
mod tests {
    use super::*;
    const SESSION_TOKEN: &str = "account-session-fixture-token";
    fn db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON;CREATE TABLE profiles(id INTEGER PRIMARY KEY,name TEXT NOT NULL);INSERT INTO profiles VALUES(1,'Default');CREATE TABLE favorites(profile_id INTEGER,id TEXT);INSERT INTO favorites VALUES(1,'kept');").unwrap();
        init(&db).unwrap();
        db
    }
    fn headers() -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_static("tv.example"));
        h.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://tv.example"),
        );
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {SESSION_TOKEN}")).unwrap(),
        );
        h
    }
    fn claim(db: &Connection) -> (Value, Option<(String, String)>) {
        let result = dispatch(
            db,
            "/auth/register",
            None,
            &headers(),
            &json!({"username":"owner","password":"a-long-owner-password"}),
            SESSION_TOKEN,
        )
        .unwrap();
        db.execute("UPDATE auth_accounts SET role='owner' WHERE id=1", [])
            .unwrap();
        if !db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM profiles WHERE id=1)",
                [],
                |row| row.get::<_, bool>(0),
            )
            .unwrap()
        {
            create_profile(db, 1, &json!({"name":"Owner","avatar_style":"critters"})).unwrap();
        }
        db.execute("UPDATE profiles SET presentation_complete=1 WHERE id=1", [])
            .unwrap();
        db.execute(
            "INSERT OR IGNORE INTO profile_owners(profile_id,account_id,created_at) VALUES(1,1,0)",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT OR IGNORE INTO auth_profiles(account_id,profile_id) VALUES(1,1)",
            [],
        )
        .unwrap();
        result
    }
    #[test]
    fn device_grants_persist_while_access_tokens_expire_and_browser_logins_rotate() {
        let db = db();
        claim(&db);
        let (grant, _) = session(&db, 1, Some(1), "device", "Living room").unwrap();
        let sid = grant["session_id"].as_str().unwrap();
        let (access, refresh): (i64, i64) = db
            .query_row(
                "SELECT access_expires,refresh_expires FROM auth_sessions WHERE id=?1",
                [sid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(access <= now() + ACCESS);
        assert_eq!(refresh, DEVICE_EXPIRY);
        for _ in 0..MAX_SESSIONS_PER_ACCOUNT + 2 {
            session(&db, 1, None, "browser", "").unwrap();
        }
        assert!(db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_sessions WHERE id=?1)",
                [sid],
                |r| r.get::<_, bool>(0)
            )
            .unwrap());
    }
    fn owner() -> Principal {
        Principal::Account {
            account_id: 1,
            role: "owner".into(),
            profile_id: Some(1),
            session_id: None,
        }
    }
    #[tokio::test]
    async fn http_middleware_rejects_unauthenticated_and_cookie_csrf() {
        use axum::body::Body;
        use tower::ServiceExt;
        let app = App::new(
            Connection::open_in_memory().unwrap(),
            reqwest::Client::new(),
            crate::playback::PlaybackManager::new(crate::playback::Config {
                ffmpeg: "missing".into(),
                ffprobe: "missing".into(),
                root: "unused-auth-test".into(),
                max_sessions: 1,
                ttl: std::time::Duration::from_secs(30),
            }),
        )
        .unwrap();
        let (data, cookies) = claim(&app.db.lock().unwrap());
        let access = cookies.unwrap().0;
        let router = router_with_auth(app.clone()).with_state(app.clone());
        for path in ["/auth/register", "/auth/login", "/auth/recover"] {
            let out = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(path)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(out.status(), StatusCode::FORBIDDEN, "missing Origin {path}");
        }
        for (path, method, cookie_value, csrf_value, expected) in [
            ("/auth/status", "GET", None, None, StatusCode::OK),
            ("/auth/me", "GET", None, None, StatusCode::UNAUTHORIZED),
            (
                "/auth/profile",
                "POST",
                Some(access.as_str()),
                None,
                StatusCode::FORBIDDEN,
            ),
            (
                "/auth/profile",
                "POST",
                Some(access.as_str()),
                Some("bad"),
                StatusCode::FORBIDDEN,
            ),
            (
                "/auth/profile",
                "POST",
                Some(access.as_str()),
                data["csrf_token"].as_str(),
                StatusCode::OK,
            ),
        ] {
            let mut b = Request::builder()
                .header(header::HOST, "tv.example")
                .header(header::ORIGIN, "https://tv.example")
                .uri(path)
                .method(method)
                .header(header::CONTENT_TYPE, "application/json");
            if let Some(c) = cookie_value {
                b = b.header(header::COOKIE, format!("viptv_session={c}"));
            }
            if let Some(c) = csrf_value {
                b = b.header("x-csrf-token", c);
            }
            let out = router
                .clone()
                .oneshot(b.body(Body::from("{\"profile_id\":1}")).unwrap())
                .await
                .unwrap();
            assert_eq!(out.status(), expected, "{method} {path}");
        }
        let device = session(&app.db.lock().unwrap(), 1, Some(1), "device", "TV")
            .unwrap()
            .0;
        for path in ["/accounts", "/admin/devices", "/auth/sessions"] {
            let out = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header(
                            header::AUTHORIZATION,
                            format!("Bearer {}", device["access_token"].as_str().unwrap()),
                        )
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(out.status(), StatusCode::FORBIDDEN, "device {path}");
        }
        for expected in [StatusCode::OK, StatusCode::UNAUTHORIZED] {
            let out = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/device/refresh")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(
                            json!({"refresh_token":device["refresh_token"]}).to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(out.status(), expected);
        }
        assert_eq!(
            app.db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM auth_sessions WHERE id=?1",
                    [device["session_id"].as_str().unwrap()],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        for path in [
            "/accounts",
            "/onboarding",
            "/auth/sessions",
            "/admin/devices",
        ] {
            let out = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header(header::COOKIE, format!("viptv_session={access}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(out.status(), StatusCode::OK, "owner {path}");
        }
        app.db
            .lock()
            .unwrap()
            .execute("UPDATE auth_sessions SET access_expires=0", [])
            .unwrap();
        let out = router
            .oneshot(
                Request::builder()
                    .uri("/auth/me")
                    .header(header::COOKIE, format!("viptv_session={access}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(out.status(), StatusCode::UNAUTHORIZED);
    }
    #[test]
    fn additive_schema_preserves_existing_rows_and_assigns_owner() {
        let db = db();
        claim(&db);
        init(&db).unwrap();
        assert_eq!(
            db.query_row("SELECT id FROM favorites", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "kept"
        );
        assert_eq!(
            db.query_row(
                "SELECT account_id FROM profile_owners WHERE profile_id=1",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("SELECT max(version) FROM schema_migrations", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            4
        );
    }
    #[test]
    fn offline_owner_bootstrap_is_single_use_and_claims_only_unowned_history() {
        let mut db = db();
        let recovery = create_owner_offline(
            &mut db,
            "administrator",
            "Administrator",
            "offline-owner-password",
        )
        .unwrap();
        assert!(recovery.len() >= 32);
        assert_eq!(
            db.query_row(
                "SELECT role FROM auth_accounts WHERE username='administrator'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            "owner"
        );
        assert_eq!(
            db.query_row(
                "SELECT account_id FROM profile_owners WHERE profile_id=1",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert!(
            create_owner_offline(&mut db, "second-admin", "Second", "second-owner-password")
                .is_err()
        );
        let member = dispatch(
            &db,
            "/auth/register",
            None,
            &headers(),
            &json!({"username":"public-member","password":"public-member-password"}),
            SESSION_TOKEN,
        )
        .unwrap()
        .0;
        assert_eq!(
            db.query_row(
                "SELECT role FROM auth_accounts WHERE id=?1",
                [identifier(&member, "account_id").unwrap()],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            "member"
        );
    }
    #[test]
    fn public_registration_is_zero_profile_and_argon2() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE profiles(id INTEGER PRIMARY KEY,name TEXT NOT NULL);CREATE TABLE favorites(profile_id INTEGER,id TEXT);").unwrap();
        init(&db).unwrap();
        let (out, cookies) = dispatch(
            &db,
            "/auth/register",
            None,
            &headers(),
            &json!({"username":"viewer","name":"Viewer","password":"a-long-viewer-password"}),
            SESSION_TOKEN,
        )
        .unwrap();
        assert!(cookies.is_some());
        assert!(out["profile_id"].is_null());
        assert!(list_profiles(&db, 1).unwrap().is_empty());
        let ph: String = db
            .query_row("SELECT password_hash FROM auth_accounts", [], |r| r.get(0))
            .unwrap();
        assert!(ph.starts_with("$argon2id$"));
    }
    #[test]
    fn secrets_hashed_and_refresh_rotates() {
        let db = db();
        let (v, c) = claim(&db);
        let (access, refresh) = c.unwrap();
        let stored: (String, String, String) = db
            .query_row(
                "SELECT access_hash,refresh_hash,csrf_hash FROM auth_sessions",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(stored.0, hash(&access));
        assert_eq!(stored.1, hash(&refresh));
        assert_eq!(stored.2, hash(v["csrf_token"].as_str().unwrap()));
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_static("tv.example"));
        h.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://tv.example"),
        );
        h.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("viptv_refresh={refresh}")).unwrap(),
        );
        assert!(dispatch(&db, "/auth/refresh", None, &h, &json!({}), SESSION_TOKEN).is_err());
        h.insert(
            "x-csrf-token",
            HeaderValue::from_str(v["csrf_token"].as_str().unwrap()).unwrap(),
        );
        let rotated = dispatch(&db, "/auth/refresh", None, &h, &json!({}), SESSION_TOKEN).unwrap();
        assert_ne!(rotated.1.unwrap().0, access);
        assert!(dispatch(&db, "/auth/refresh", None, &h, &json!({}), SESSION_TOKEN).is_err());
    }
    #[test]
    fn recovery_is_single_use_and_revokes_sessions() {
        let db = db();
        let (v, _) = claim(&db);
        let payload = json!({"username":"owner","recovery_code":v["recovery_code"],"password":"replacement-long-password"});
        let out = dispatch(
            &db,
            "/auth/recover",
            None,
            &HeaderMap::new(),
            &payload,
            SESSION_TOKEN,
        )
        .unwrap();
        assert_ne!(v["recovery_code"], out.0["recovery_code"]);
        assert!(dispatch(
            &db,
            "/auth/recover",
            None,
            &HeaderMap::new(),
            &payload,
            SESSION_TOKEN
        )
        .is_err());
        assert_eq!(
            db.query_row("SELECT count(*) FROM auth_sessions", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert!(dispatch(
            &db,
            "/auth/login",
            None,
            &HeaderMap::new(),
            &json!({"username":"owner","password":"a-long-owner-password"}),
            SESSION_TOKEN
        )
        .is_err());
        assert!(dispatch(
            &db,
            "/auth/login",
            None,
            &HeaderMap::new(),
            &json!({"username":"owner","password":"replacement-long-password"}),
            SESSION_TOKEN
        )
        .is_ok());
    }
    #[test]
    fn member_profile_and_owner_boundaries() {
        let db = db();
        claim(&db);
        let out = dispatch(
            &db,
            "/auth/register",
            None,
            &headers(),
            &json!({"username":"member","password":"a-long-member-password"}),
            SESSION_TOKEN,
        )
        .unwrap()
        .0;
        let account_id = identifier(&out, "account_id").unwrap();
        let profile = create_profile(
            &db,
            account_id,
            &json!({"name":"Member","avatar_style":"moods"}),
        )
        .unwrap();
        let p = Principal::Account {
            account_id,
            role: "member".into(),
            profile_id: None,
            session_id: None,
        };
        assert!(p.require_owner().is_err());
        assert!(p.require_profile(&db, 1).is_err());
        assert!(p
            .require_profile(&db, identifier(&profile, "id").unwrap())
            .is_ok());
        assert_eq!(
            db.query_row(
                "SELECT role FROM auth_accounts WHERE id=?1",
                [account_id],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            "member"
        );
    }
    #[test]
    fn device_pending_approval_consumption_and_revocation() {
        let db = db();
        claim(&db);
        let code = dispatch(
            &db,
            "/auth/device/code",
            None,
            &HeaderMap::new(),
            &json!({"device_name":"Living Room"}),
            SESSION_TOKEN,
        )
        .unwrap()
        .0;
        let payload = json!({"device_code":code["device_code"]});
        assert!(dispatch(
            &db,
            "/auth/device/token",
            None,
            &HeaderMap::new(),
            &payload,
            SESSION_TOKEN
        )
        .is_err());
        let lookup = dispatch(
            &db,
            "/auth/device/lookup",
            Some(owner()),
            &headers(),
            &json!({"user_code":code["user_code"]}),
            SESSION_TOKEN,
        )
        .unwrap()
        .0;
        assert_eq!(lookup["device"]["status"], "pending");
        dispatch(
            &db,
            "/auth/device/approve",
            Some(owner()),
            &headers(),
            &json!({"user_code":code["user_code"]}),
            SESSION_TOKEN,
        )
        .unwrap();
        let token = dispatch(
            &db,
            "/auth/device/token",
            None,
            &HeaderMap::new(),
            &payload,
            SESSION_TOKEN,
        )
        .unwrap()
        .0;
        assert!(token["access_token"].is_string());
        assert!(dispatch(
            &db,
            "/auth/device/token",
            None,
            &HeaderMap::new(),
            &payload,
            SESSION_TOKEN
        )
        .is_err());
        let rotated = dispatch(
            &db,
            "/auth/device/refresh",
            None,
            &HeaderMap::new(),
            &json!({"refresh_token":token["refresh_token"]}),
            SESSION_TOKEN,
        )
        .unwrap()
        .0;
        dispatch(
            &db,
            "/auth/device/revoke",
            Some(owner()),
            &headers(),
            &json!({"session_id":rotated["session_id"]}),
            SESSION_TOKEN,
        )
        .unwrap();
        assert!(dispatch(
            &db,
            "/auth/device/refresh",
            None,
            &HeaderMap::new(),
            &json!({"refresh_token":rotated["refresh_token"]}),
            SESSION_TOKEN
        )
        .is_err());
    }
    #[test]
    fn refresh_replay_revokes_only_its_family_and_devices_are_not_admins() {
        let db = db();
        claim(&db);
        let first = session(&db, 1, Some(1), "device", "TV").unwrap().0;
        let other = session(&db, 1, Some(1), "device", "Other").unwrap().0;
        let old = json!({"refresh_token":first["refresh_token"]});
        let next = dispatch(
            &db,
            "/auth/device/refresh",
            None,
            &HeaderMap::new(),
            &old,
            SESSION_TOKEN,
        )
        .unwrap()
        .0;
        assert_eq!(first["session_id"], next["session_id"]);
        assert_eq!(
            dispatch(
                &db,
                "/auth/device/refresh",
                None,
                &HeaderMap::new(),
                &old,
                SESSION_TOKEN
            )
            .unwrap_err()
            .1,
            "Refresh token reuse detected"
        );
        assert!(dispatch(
            &db,
            "/auth/device/refresh",
            None,
            &HeaderMap::new(),
            &json!({"refresh_token":next["refresh_token"]}),
            SESSION_TOKEN
        )
        .is_err());
        assert!(dispatch(
            &db,
            "/auth/device/refresh",
            None,
            &HeaderMap::new(),
            &json!({"refresh_token":other["refresh_token"]}),
            SESSION_TOKEN
        )
        .is_ok());
        let device = Principal::Account {
            account_id: 1,
            role: "device".into(),
            profile_id: Some(1),
            session_id: None,
        };
        assert!(!device.is_owner());
        assert!(device.require_owner().is_err());
        assert!(dispatch(
            &db,
            "/auth/accounts",
            Some(device),
            &headers(),
            &json!({"username":"evil","password":"long-password-for-evil"}),
            SESSION_TOKEN
        )
        .is_err());
    }
    #[test]
    fn configured_https_origin_is_pinned_without_forwarded_host_trust() {
        for bad in [
            "http://tv.example",
            "https://tv.example/path",
            "https://user:pass@tv.example",
            "https://tv.example?x=1",
            "https://tv.example#frag",
            "",
        ] {
            assert!(parse_origin(bad).is_err(), "{bad}");
        }
        let pin = parse_origin("https://tv.example:8443").unwrap();
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_static("internal:3000"));
        h.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://tv.example:8443"),
        );
        assert!(check_origin(&h, Some(&pin)).is_ok());
        assert!(check_origin(&h, None).is_err());
        h.insert(
            "x-forwarded-host",
            HeaderValue::from_static("tv.example:8443"),
        );
        assert!(check_origin(&h, None).is_err());
        h.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://tv.example"),
        );
        assert!(check_origin(&h, Some(&pin)).is_err());
    }
    #[test]
    fn origins_cookie_flags_and_public_allowlist() {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_static("tv.example"));
        h.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://tv.example"),
        );
        h.insert(header::HOST, HeaderValue::from_static("tv.example"));
        h.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://evil.example"),
        );
        assert!(origin(&h).is_err());
        h.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://tv.example"),
        );
        assert!(origin(&h).is_ok());
        h.insert("sec-fetch-site", HeaderValue::from_static("cross-site"));
        assert!(origin(&h).is_err());
        let r = response(json!({}), Some(("access".into(), "refresh".into())));
        for c in r.headers().get_all(header::SET_COOKIE) {
            let c = c.to_str().unwrap();
            assert!(c.contains("HttpOnly; Secure; SameSite=Strict"));
            assert!(!c.contains(".."));
        }
        assert!(public("/auth/login", &Method::POST));
        assert!(!public("/auth/accounts", &Method::POST));
        assert!(!public("/auth/device/approve", &Method::POST));
        assert!(!public("/auth/login/extra", &Method::POST));
    }
    #[test]
    fn immutable_device_scope_keys_and_persisted_revocation() {
        let db = db();
        claim(&db);
        db.execute("INSERT INTO profiles(id,name,avatar_seed,presentation_complete) VALUES(2,'Other','other-seed',1)", [])
            .unwrap();
        db.execute("INSERT INTO auth_profiles VALUES(1,2)", [])
            .unwrap();
        let first = session(&db, 1, Some(1), "device", "TV").unwrap().0;
        let second = session(&db, 1, Some(1), "device", "TV2").unwrap().0;
        let scope = |v: &Value, profile| Principal::Account {
            account_id: 1,
            role: "device".into(),
            profile_id: Some(profile),
            session_id: Some(v["session_id"].as_str().unwrap().into()),
        };
        let p = scope(&first, 1);
        assert!(p.validate_scope(&db).is_ok());
        assert_ne!(p.key(), scope(&second, 1).key());
        assert_ne!(p.key(), scope(&first, 2).key());
        assert!(scope(&first, 2).validate_scope(&db).is_err());
        assert!(p.require_profile(&db, 2).is_err());
        let rotated = dispatch(
            &db,
            "/auth/device/refresh",
            None,
            &HeaderMap::new(),
            &json!({"refresh_token":first["refresh_token"]}),
            SESSION_TOKEN,
        )
        .unwrap()
        .0;
        assert_eq!(first["session_id"], rotated["session_id"]);
        assert!(p.validate_scope(&db).is_ok());
        db.execute(
            "DELETE FROM profile_owners WHERE account_id=1 AND profile_id=1",
            [],
        )
        .unwrap();
        assert!(p.validate_scope(&db).is_err());
        db.execute(
            "INSERT INTO profile_owners(profile_id,account_id,created_at) VALUES(1,1,0)",
            [],
        )
        .unwrap();
        db.execute(
            "DELETE FROM auth_sessions WHERE id=?1",
            [p.session_id().unwrap()],
        )
        .unwrap();
        assert!(p.validate_scope(&db).is_err());
        assert!(scope(&second, 1).validate_scope(&db).is_ok());
    }
    #[test]
    fn owner_management_does_not_bypass_profile_grants() {
        let db = db();
        claim(&db);
        db.execute("DELETE FROM profile_owners WHERE account_id=1", [])
            .unwrap();
        assert!(owner().is_owner());
        assert!(!owner().can_profile(&db, 1).unwrap());
        assert!(owner().require_profile(&db, 1).is_err());
    }
    #[test]
    fn password_snapshot_revalidation_and_last_owner_protection() {
        let db = db();
        claim(&db);
        let payload = json!({"username":"owner","password":"a-long-owner-password"});
        let prepared = prepare_auth(
            "/auth/login",
            &payload,
            login_snapshot(&db, "/auth/login", &payload).unwrap(),
        )
        .unwrap();
        assert!(prepared.login_valid);
        db.execute(
            "UPDATE auth_accounts SET password_hash='changed-concurrently' WHERE id=1",
            [],
        )
        .unwrap();
        assert!(
            dispatch_prepared(&db, "/auth/login", None, &headers(), &payload, &prepared).is_err()
        );
        let unknown = prepare_auth(
            "/auth/login",
            &json!({"username":"unknown","password":"unknown-password"}),
            None,
        )
        .unwrap();
        assert!(!unknown.login_valid);
        for payload in [
            json!({"account_id":1,"role":"member"}),
            json!({"account_id":1,"disabled":true}),
        ] {
            assert!(dispatch(
                &db,
                "/auth/accounts/update",
                Some(owner()),
                &headers(),
                &payload,
                SESSION_TOKEN
            )
            .is_err());
        }
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM auth_accounts WHERE role='owner' AND disabled=0",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }
    #[test]
    fn rate_limits_are_database_local_and_init_leaves_no_transaction() {
        let first = db();
        let second = db();
        assert!(first.is_autocommit());
        init(&first).unwrap();
        assert!(first.is_autocommit());
        rate(&first, "login", 1).unwrap();
        assert!(rate(&first, "login", 1).is_err());
        assert!(rate(&second, "login", 1).is_ok());
        for _ in 0..100 {
            rate(&first, "auth:register:global", 100).unwrap();
        }
        assert_eq!(
            rate(&first, "auth:register:global", 100).unwrap_err().0,
            StatusCode::TOO_MANY_REQUESTS
        );
        for _ in 0..5 {
            rate(&second, "auth/register:subject-a", 5).unwrap();
        }
        assert_eq!(
            rate(&second, "auth/register:subject-a", 5).unwrap_err().0,
            StatusCode::TOO_MANY_REQUESTS
        );
        assert!(rate(&second, "auth/register:subject-b", 5).is_ok());
        first
            .execute_batch("BEGIN; CREATE TABLE addon_init_fixture(id INTEGER); COMMIT;")
            .unwrap();
    }
    #[test]
    fn tab_csrf_tokens_survive_refresh_and_revoke_with_family() {
        let db = db();
        let (issued, cookies) = claim(&db);
        let sid = issued["session_id"].as_str().unwrap();
        let first = issue_csrf(&db, sid).unwrap();
        let second = issue_csrf(&db, sid).unwrap();
        let mut h = headers();
        h.insert(header::HOST, HeaderValue::from_static("tv.example"));
        h.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://tv.example"),
        );
        h.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("viptv_refresh={}", cookies.unwrap().1)).unwrap(),
        );
        h.insert("x-csrf-token", HeaderValue::from_str(&first).unwrap());
        let rotated = dispatch(&db, "/auth/refresh", None, &h, &json!({}), SESSION_TOKEN).unwrap();
        assert_eq!(rotated.0["session_id"], sid);
        h.insert("x-csrf-token", HeaderValue::from_str(&second).unwrap());
        let primary: String = db
            .query_row(
                "SELECT csrf_hash FROM auth_sessions WHERE id=?1",
                [sid],
                |r| r.get(0),
            )
            .unwrap();
        assert!(csrf_for_session(&db, &h, sid, &primary).is_ok());
        db.execute("DELETE FROM auth_sessions WHERE id=?1", [sid])
            .unwrap();
        assert!(csrf_for_session(&db, &h, sid, &primary).is_err());
    }
    #[test]
    fn member_confirmation_binds_device_to_current_account_only() {
        let db = db();
        claim(&db);
        let member = dispatch(
            &db,
            "/auth/register",
            None,
            &headers(),
            &json!({"username":"pairedmember","password":"member-long-password"}),
            SESSION_TOKEN,
        )
        .unwrap()
        .0;
        let id = identifier(&member, "account_id").unwrap();
        let profile =
            create_profile(&db, id, &json!({"name":"Member","avatar_style":"thumbs"})).unwrap();
        let profile = identifier(&profile, "id").unwrap();
        assert!(owner().require_profile(&db, profile).is_err());
        let code = dispatch(
            &db,
            "/auth/device/code",
            None,
            &HeaderMap::new(),
            &json!({"device_name":"Member TV"}),
            SESSION_TOKEN,
        )
        .unwrap()
        .0;
        let member_principal = Principal::Account {
            account_id: id,
            role: "member".into(),
            profile_id: None,
            session_id: None,
        };
        assert!(dispatch(
            &db,
            "/auth/device/approve",
            Some(member_principal.clone()),
            &headers(),
            &json!({"account_id":1,"user_code":code["user_code"]}),
            SESSION_TOKEN,
        )
        .is_err());
        dispatch(
            &db,
            "/auth/device/approve",
            Some(member_principal),
            &headers(),
            &json!({"user_code":code["user_code"]}),
            SESSION_TOKEN,
        )
        .unwrap();
        let issued = dispatch(
            &db,
            "/auth/device/token",
            None,
            &HeaderMap::new(),
            &json!({"device_code":code["device_code"]}),
            SESSION_TOKEN,
        )
        .unwrap()
        .0;
        assert_eq!(identifier(&issued, "account_id").unwrap(), id);
        assert!(issued["profile_id"].is_null());
        let principal = Principal::Account {
            account_id: id,
            role: "device".into(),
            profile_id: None,
            session_id: Some(issued["session_id"].as_str().unwrap().into()),
        };
        assert!(principal.can_profile(&db, profile).unwrap());
        assert!(!principal.can_profile(&db, 1).unwrap());
    }
    #[tokio::test]
    async fn argon_admission_times_out_but_cheap_actions_bypass_the_queue() {
        let semaphore = tokio::sync::Semaphore::new(0);
        let start = std::time::Instant::now();
        let error = acquire_auth_cpu(true, &semaphore, std::time::Duration::from_millis(10))
            .await
            .unwrap_err();
        assert_eq!(error.0, StatusCode::SERVICE_UNAVAILABLE);
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
        assert!(
            acquire_auth_cpu(false, &semaphore, std::time::Duration::ZERO)
                .await
                .unwrap()
                .is_none()
        );
        semaphore.add_permits(1);
        assert!(
            acquire_auth_cpu(true, &semaphore, std::time::Duration::from_secs(1))
                .await
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn selected_avatar_is_exact_and_survives_name_edits() {
        let db = db();
        claim(&db);
        let profile = create_profile(
            &db,
            1,
            &json!({"name":"Kid","avatar_style":"pixelbot","avatar_choice":48}),
        )
        .unwrap();
        let id = profile["id"].as_str().unwrap().parse::<i64>().unwrap();
        assert_eq!(profile["avatar_choice"], 48);
        assert!(profile["avatar_url"]
            .as_str()
            .unwrap()
            .contains("seed=viptv-pixelbot-48&"));
        let renamed = update_profile(&db, 1, id, &json!({"name":"New name"})).unwrap();
        assert_eq!(renamed["avatar_url"], profile["avatar_url"]);
        let changed = update_profile(
            &db,
            1,
            id,
            &json!({"avatar_style":"sprouts","avatar_choice":2}),
        )
        .unwrap();
        assert!(changed["avatar_url"]
            .as_str()
            .unwrap()
            .contains("seed=viptv-sprouts-2&"));
        for choice in [
            json!(0),
            json!(49),
            json!(-1),
            json!(1.5),
            json!("2"),
            Value::Null,
        ] {
            assert!(update_profile(&db, 1, id, &json!({"avatar_choice":choice})).is_err());
        }
        assert!(update_profile(&db, 999, id, &json!({"avatar_choice":1})).is_err());
        let disney = update_profile(
            &db,
            1,
            id,
            &json!({"avatar_style":"disney","avatar_choice":1}),
        )
        .unwrap();
        assert_eq!(
            disney["avatar_url"],
            character_avatars()["disney"][0]["url"]
        );
        assert!(update_profile(&db, 1, id, &json!({"avatar_choice":48})).is_err());
        let renamed = update_profile(&db, 1, id, &json!({"name":"Mickey fan"})).unwrap();
        assert_eq!(renamed["avatar_url"], disney["avatar_url"]);
    }

    #[test]
    fn strict_profile_payloads_require_explicit_imported_setup_completion() {
        let db = db();
        claim(&db);
        db.execute("UPDATE profiles SET presentation_complete=0 WHERE id=1", [])
            .unwrap();
        for payload in [
            json!({}),
            json!({"name":"Renamed"}),
            json!({"avatar_style":"moods"}),
            json!({"name":42,"avatar_style":"moods","setup_complete":true}),
            json!({"name":"Renamed","avatar_style":42,"setup_complete":true}),
            json!({"name":"Renamed","avatar_style":"moods","setup_complete":false}),
        ] {
            assert!(update_profile(&db, 1, 1, &payload).is_err(), "{payload}");
            assert!(!db
                .query_row(
                    "SELECT presentation_complete FROM profiles WHERE id=1",
                    [],
                    |row| row.get::<_, bool>(0)
                )
                .unwrap());
        }
        let updated = update_profile(
            &db,
            1,
            1,
            &json!({"name":"Renamed","avatar_style":"moods","setup_complete":true}),
        )
        .unwrap();
        assert_eq!(updated["setup_complete"], true);
        assert!(update_profile(&db, 1, 1, &json!({})).is_err());
        assert!(update_profile(&db, 1, 1, &json!({"avatar_style":42})).is_err());
        assert!(create_profile(&db, 1, &json!({"name":"Invalid","avatar_style":42})).is_err());
        assert!(create_profile(&db, 1, &json!({"name":"Invalid","setup_complete":true})).is_err());
    }

    #[test]
    fn session_eviction_bounds_members_and_preserves_owner_admission() {
        let db = db();
        claim(&db);
        db.execute("DELETE FROM auth_sessions", []).unwrap();
        let tx = db.unchecked_transaction().unwrap();
        for account_id in 2..=251 {
            tx.execute(
                "INSERT INTO auth_accounts(id,username,name,password_hash,role,recovery_hash,created_at) VALUES(?1,?2,?2,'x','member',?2,0)",
                params![account_id, format!("member-{account_id}")],
            )
            .unwrap();
        }
        for index in 0..MAX_MEMBER_SESSIONS {
            let account_id = 2 + index / MAX_SESSIONS_PER_ACCOUNT;
            let session_id = format!("member-session-{index:05}");
            tx.execute(
                "INSERT INTO auth_sessions(id,account_id,access_hash,refresh_hash,csrf_hash,profile_id,kind,device_name,access_expires,refresh_expires,created_at) VALUES(?1,?2,?3,?4,?5,NULL,'browser','test',?6,?7,?8)",
                params![session_id, account_id, format!("access-{index}"), format!("refresh-{index}"), format!("csrf-{index}"), now()+ACCESS, now()+REFRESH, index],
            )
            .unwrap();
        }
        tx.commit().unwrap();

        let owner_session = session(&db, 1, None, "browser", "Owner browser").unwrap().0;
        assert!(db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_sessions WHERE id=?1)",
                [owner_session["session_id"].as_str().unwrap()],
                |row| row.get::<_, bool>(0)
            )
            .unwrap());
        session(&db, 2, None, "browser", "Member browser").unwrap();
        assert!(db
            .query_row(
                "SELECT count(*)<=?1 FROM auth_sessions s JOIN auth_accounts a ON a.id=s.account_id WHERE a.role='member'",
                [MAX_MEMBER_SESSIONS],
                |row| row.get::<_, bool>(0)
            )
            .unwrap());
        assert!(db
            .query_row(
                "SELECT count(*)<=?1 FROM auth_sessions WHERE account_id=2",
                [MAX_SESSIONS_PER_ACCOUNT],
                |row| row.get::<_, bool>(0)
            )
            .unwrap());
        assert!(db
            .query_row(
                "SELECT count(*)<=?1 FROM auth_sessions",
                [MAX_SESSIONS],
                |row| row.get::<_, bool>(0)
            )
            .unwrap());
    }

    #[test]
    fn attacker_keyed_limits_and_pairings_evict_without_fail_closed_capacity() {
        let db = db();
        claim(&db);
        let tx = db.unchecked_transaction().unwrap();
        tx.execute(
            "INSERT INTO auth_limits(bucket,count,expires) VALUES('auth:global:/auth/login',1,?1)",
            [now() + 300],
        )
        .unwrap();
        for index in 1..MAX_AUTH_BUCKETS {
            tx.execute(
                "INSERT INTO auth_limits(bucket,count,expires) VALUES(?1,1,?2)",
                params![format!("auth:subject:random-{index}"), now() + 300],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        rate(&db, "auth:global:/auth/login", 600).unwrap();
        rate(&db, "auth:subject:/auth/login:legitimate-owner", 5).unwrap();
        assert!(dispatch(
            &db,
            "/auth/login",
            None,
            &headers(),
            &json!({"username":"owner","password":"a-long-owner-password"}),
            SESSION_TOKEN,
        )
        .is_ok());
        assert_eq!(
            db.query_row("SELECT count(*) FROM auth_limits", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            MAX_AUTH_BUCKETS
        );
        assert!(db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_limits WHERE bucket='auth:global:/auth/login')",
                [],
                |row| row.get::<_, bool>(0)
            )
            .unwrap());

        db.execute("DELETE FROM auth_pairings", []).unwrap();
        for index in 0..MAX_PAIRINGS {
            db.execute(
                "INSERT INTO auth_pairings(code_hash,device_hash,device_name,expires) VALUES(?1,?2,'attacker',?3)",
                params![format!("code-{index}"), format!("device-{index}"), now()+600],
            )
            .unwrap();
        }
        let pairing = dispatch(
            &db,
            "/auth/device/code",
            None,
            &HeaderMap::new(),
            &json!({"device_name":"Owner Roku"}),
            SESSION_TOKEN,
        )
        .unwrap()
        .0;
        assert!(pairing["device_code"].is_string());
        assert!(db
            .query_row(
                "SELECT count(*)<=?1 FROM auth_pairings",
                [MAX_PAIRINGS],
                |row| row.get::<_, bool>(0)
            )
            .unwrap());
        dispatch(
            &db,
            "/auth/device/approve",
            Some(owner()),
            &headers(),
            &json!({"user_code":pairing["user_code"]}),
            SESSION_TOKEN,
        )
        .unwrap();
    }

    #[test]
    fn refresh_tombstones_are_bounded_without_blocking_rotation() {
        let db = db();
        let (issued, cookies) = claim(&db);
        let family = issued["session_id"].as_str().unwrap();
        let tx = db.unchecked_transaction().unwrap();
        for index in 0..MAX_REFRESH_TOMBSTONES {
            tx.execute(
                "INSERT INTO auth_refresh_used(hash,family,account_id,csrf_hash,kind,expires) VALUES(?1,?2,NULL,'x','device',?3)",
                params![format!("old-{index}"), format!("other-family-{index}"), now()+REFRESH],
            )
            .unwrap();
        }
        for index in 0..MAX_REFRESH_TOMBSTONES_PER_FAMILY {
            tx.execute(
                "INSERT INTO auth_refresh_used(hash,family,account_id,csrf_hash,kind,expires) VALUES(?1,?2,1,'x','browser',?3)",
                params![format!("family-old-{index}"), family, now()+REFRESH],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        let mut h = headers();
        h.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("viptv_refresh={}", cookies.unwrap().1)).unwrap(),
        );
        h.insert(
            "x-csrf-token",
            HeaderValue::from_str(issued["csrf_token"].as_str().unwrap()).unwrap(),
        );
        dispatch(&db, "/auth/refresh", None, &h, &json!({}), SESSION_TOKEN).unwrap();
        assert!(db
            .query_row(
                "SELECT count(*)<=?1 FROM auth_refresh_used WHERE family=?2",
                params![MAX_REFRESH_TOMBSTONES_PER_FAMILY, family],
                |row| row.get::<_, bool>(0)
            )
            .unwrap());
        assert!(db
            .query_row(
                "SELECT count(*)<=?1 FROM auth_refresh_used",
                [MAX_REFRESH_TOMBSTONES],
                |row| row.get::<_, bool>(0)
            )
            .unwrap());
    }

    #[test]
    fn bounded_limits_events_and_expiry() {
        let db = db();
        for _ in 0..3 {
            rate(&db, "login", 3).unwrap();
        }
        assert_eq!(
            rate(&db, "login", 3).unwrap_err().0,
            StatusCode::TOO_MANY_REQUESTS
        );
        db.execute("UPDATE auth_limits SET expires=0", []).unwrap();
        rate(&db, "login", 3).unwrap();
        for _ in 0..1010 {
            event(&db, "test", None).unwrap();
        }
        assert_eq!(
            db.query_row("SELECT count(*) FROM auth_events", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1000
        );
        let (v, c) = claim(&db);
        db.execute("UPDATE auth_sessions SET refresh_expires=0", [])
            .unwrap();
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_static("tv.example"));
        h.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://tv.example"),
        );
        h.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("viptv_refresh={}", c.unwrap().1)).unwrap(),
        );
        h.insert(
            "x-csrf-token",
            HeaderValue::from_str(v["csrf_token"].as_str().unwrap()).unwrap(),
        );
        assert!(dispatch(&db, "/auth/refresh", None, &h, &json!({}), SESSION_TOKEN).is_err());
    }
}
