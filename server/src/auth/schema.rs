use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const ACCESS: i64 = 1800;
pub(crate) const REFRESH: i64 = 30 * 86400;
// Device grants persist until explicit revocation; short-lived access tokens still rotate.
pub(crate) const DEVICE_EXPIRY: i64 = 253_402_300_799;
pub(crate) const MAX_PROFILES: i64 = 12;
pub(crate) const MAX_SESSIONS_PER_ACCOUNT: i64 = 32;
pub(crate) const MAX_MEMBER_SESSIONS: i64 = 8_000;
pub(crate) const MAX_SESSIONS: i64 = 10_000;
pub(crate) const MAX_AUTH_BUCKETS: i64 = 10_000;
pub(crate) const MAX_PENDING_PAIRINGS: i64 = 192;
pub(crate) const MAX_PAIRINGS: i64 = 256;
pub(crate) const MAX_APPROVED_PAIRINGS_PER_ACCOUNT: i64 = 8;
pub(crate) const MAX_REFRESH_TOMBSTONES_PER_FAMILY: i64 = 64;
pub(crate) const MAX_REFRESH_TOMBSTONES_PER_ACCOUNT: i64 = 2_048;
pub(crate) const MAX_REFRESH_TOMBSTONES: i64 = 20_000;
pub(crate) const AVATAR_STYLES: &[&str] = &[
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
pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
pub(crate) fn token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}
pub(crate) fn hash(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}
