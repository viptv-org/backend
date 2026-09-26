use super::*;
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{http::HeaderValue, Json};
use std::sync::LazyLock;

pub(crate) fn field<'a>(v: &'a Value, k: &str, max: usize) -> Result<&'a str, ApiError> {
    crate::text(v, k, max)
}
pub(crate) fn identifier(v: &Value, key: &str) -> Result<i64, ApiError> {
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
pub(crate) fn username(v: &Value) -> Result<String, ApiError> {
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
pub(crate) fn pairing_code(v: &Value) -> Result<String, ApiError> {
    Ok(v.get("user_code")
        .or_else(|| v.get("code"))
        .and_then(Value::as_str)
        .ok_or("Missing user_code")?
        .trim()
        .to_uppercase())
}
pub(crate) fn event(db: &Connection, kind: &str, id: Option<i64>) -> Result<(), ApiError> {
    db.execute(
        "INSERT INTO auth_events(kind,account_id,created_at) VALUES(?1,?2,?3)",
        params![kind, id, now()],
    )
    .map_err(crate::db_error)?;
    db.execute("DELETE FROM auth_events WHERE id NOT IN (SELECT id FROM auth_events ORDER BY id DESC LIMIT 1000)",[]).map_err(crate::db_error)?;
    Ok(())
}
pub(crate) fn rate(db: &Connection, bucket: &str, limit: i64) -> Result<(), ApiError> {
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
pub(crate) fn session(
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
pub(crate) fn response(v: Value, cookies: Option<(String, String)>) -> Response {
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
pub(crate) fn canonical(path: &str, v: &mut Value) -> Result<String, ApiError> {
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
pub(crate) static AUTH_CPU: LazyLock<tokio::sync::Semaphore> = LazyLock::new(|| {
    tokio::sync::Semaphore::new(std::thread::available_parallelism().map_or(1, |value| value.get()))
});
pub(crate) async fn acquire_auth_cpu(
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
pub(crate) struct PreparedAuth {
    pub(crate) new_hash: Option<String>,
    pub(crate) login_hash: Option<String>,
    pub(crate) login_valid: bool,
}
pub(crate) fn login_snapshot(
    db: &Connection,
    path: &str,
    v: &Value,
) -> Result<Option<String>, ApiError> {
    if !matches!(path, "/auth/login" | "/auth/device/login") {
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
pub(crate) fn prepare_auth(
    path: &str,
    v: &Value,
    login_hash: Option<String>,
) -> Result<PreparedAuth, ApiError> {
    let needs_hash = matches!(path, "/auth/register" | "/auth/recover");
    let new_hash = if needs_hash { Some(password(v)?) } else { None };
    let login_valid = if matches!(path, "/auth/login" | "/auth/device/login") {
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
pub(crate) fn endpoint_global_limit(path: &str) -> i64 {
    match path {
        "/auth/register" => 100,
        "/auth/login" | "/auth/device/login" => 600,
        "/auth/recover" => 100,
        "/auth/device/code" => 120,
        "/auth/device/token" | "/auth/device/lookup" => 600,
        "/auth/refresh" | "/auth/device/refresh" => 1_200,
        _ => 600,
    }
}
