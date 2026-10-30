use super::*;
use argon2::{
    password_hash::{PasswordHash, PasswordVerifier},
    Argon2,
};
use axum::http::HeaderValue;

struct Snapshot {
    account: i64,
    username: String,
    role: String,
    password_hash: String,
    pin_hash: Option<String>,
    profiles: Vec<i64>,
    providers: Vec<i64>,
}

fn load_snapshot(db: &Connection, account: i64) -> Result<Snapshot, ApiError> {
    let (username, role, password_hash) = db
        .query_row(
            "SELECT username,role,password_hash FROM auth_accounts WHERE id=?1 AND disabled=0",
            [account],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(crate::db_error)?
        .ok_or_else(unauthorized)?;
    let pin_hash = db
        .query_row(
            "SELECT pin_hash FROM parent_controls WHERE account_id=?1",
            [account],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(crate::db_error)?;
    let profiles = db
        .prepare("SELECT profile_id FROM profile_owners WHERE account_id=?1 ORDER BY profile_id")
        .and_then(|mut statement| {
            statement
                .query_map([account], |row| row.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(crate::db_error)?;
    let providers = db
        .prepare("SELECT provider_id FROM provider_ownership WHERE account_id=?1 ORDER BY provider_id")
        .and_then(|mut statement| {
            statement
                .query_map([account], |row| row.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(crate::db_error)?;
    Ok(Snapshot {
        account,
        username,
        role,
        password_hash,
        pin_hash,
        profiles,
        providers,
    })
}

fn matches_hash(hash: &str, candidate: &str) -> bool {
    PasswordHash::new(hash).ok().is_some_and(|parsed| {
        Argon2::default()
            .verify_password(candidate.as_bytes(), &parsed)
            .is_ok()
    })
}

fn worker_failed() -> ApiError {
    ApiError(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Authentication worker failed".into(),
    )
}

fn cleared_session() -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    for name in ["viptv_session", "viptv_refresh"] {
        response.headers_mut().append(
            header::SET_COOKIE,
            HeaderValue::from_str(&format!(
                "{name}=; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=0"
            ))
            .unwrap(),
        );
    }
    response
}

fn delete_owned(tx: &Connection, snapshot: &Snapshot) -> Result<(), ApiError> {
    let stranded: i64 = tx
        .query_row(
            "SELECT count(*) FROM auth_accounts WHERE id<>?1 AND disabled=0",
            [snapshot.account],
            |row| row.get(0),
        )
        .map_err(crate::db_error)?;
    if snapshot.role == "owner" && stranded > 0 {
        return Err(ApiError(StatusCode::CONFLICT, MSG_LAST_OWNER.into()));
    }
    let current: Option<(String, Option<String>)> = tx
        .query_row(
            "SELECT a.password_hash,(SELECT pin_hash FROM parent_controls WHERE account_id=a.id) FROM auth_accounts a WHERE a.id=?1",
            [snapshot.account],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(crate::db_error)?;
    if current != Some((snapshot.password_hash.clone(), snapshot.pin_hash.clone())) {
        return Err(ApiError(StatusCode::CONFLICT, MSG_ACCOUNT_CHANGED.into()));
    }
    let account = snapshot.account;
    for sql in [
        "DELETE FROM addon_credentials_v2 WHERE account_id=?1",
        "DELETE FROM addon_encryption_accounts_v2 WHERE account_id=?1",
        "DELETE FROM provider_credentials_v2 WHERE account_id=?1",
        "DELETE FROM provider_refresh_v2 WHERE account_id=?1",
        "DELETE FROM simkl_oauth_states WHERE account_id=?1",
        "DELETE FROM simkl_import_snapshots WHERE account_id=?1",
        "DELETE FROM engine_source_integrations WHERE account_id=?1",
        "DELETE FROM engine_xtream_connections WHERE account_id=?1",
        "DELETE FROM engine_m3u_connections WHERE account_id=?1",
        "DELETE FROM engine_profile_configuration WHERE account_id=?1",
        "DELETE FROM engine_profile_preferences WHERE account_id=?1",
        "DELETE FROM engine_profile_display_preferences WHERE account_id=?1",
        "DELETE FROM auth_refresh_used WHERE account_id=?1",
        "DELETE FROM auth_events WHERE account_id=?1",
        "DELETE FROM addons WHERE account_id=?1",
        "DELETE FROM addon_config_revisions WHERE account_id=?1",
    ] {
        tx.execute(sql, [account]).map_err(crate::db_error)?;
    }
    for provider in &snapshot.providers {
        tx.execute("DELETE FROM providers WHERE id=?1", [provider])
            .map_err(crate::db_error)?;
    }
    tx.execute("DELETE FROM provider_match_revisions_v2 WHERE account_id=?1", [account])
        .map_err(crate::db_error)?;
    tx.execute("DELETE FROM auth_accounts WHERE id=?1", [account])
        .map_err(crate::db_error)?;
    for profile in &snapshot.profiles {
        tx.execute("DELETE FROM profiles WHERE id=?1", [profile])
            .map_err(crate::db_error)?;
    }
    event(tx, "account_deleted", None)
}

pub(crate) async fn delete_account(
    app: App,
    p: Option<Extension<Principal>>,
    v: Value,
) -> Response {
    let Some(Extension(principal)) = p else {
        return auth_error(unauthorized());
    };
    if matches!(&principal, Principal::Account { role, .. } if role == "device") {
        return auth_error(ApiError(
            StatusCode::FORBIDDEN,
            MSG_ACCOUNT_SESSION_REQUIRED.into(),
        ));
    }
    let Some(account) = principal.account_id() else {
        return auth_error(unauthorized());
    };
    let snapshot = {
        let db = match app.db.lock() {
            Ok(db) => db,
            Err(_) => return auth_error(unauthorized()),
        };
        match load_snapshot(&db, account) {
            Ok(snapshot) => snapshot,
            Err(error) => return auth_error(error),
        }
    };
    let permit = match acquire_auth_cpu(true, &AUTH_CPU, Duration::from_secs(2)).await {
        Ok(permit) => permit,
        Err(error) => return auth_error(error),
    };
    let username = v
        .get("username")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let password = v
        .get("password")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let pin = v
        .get("pin")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let (password_hash, pin_hash) = (snapshot.password_hash.clone(), snapshot.pin_hash.clone());
    let checked = tokio::task::spawn_blocking(move || {
        drop(permit);
        let password_matches = matches_hash(&password_hash, &password);
        let pin_matches = pin_hash
            .as_deref()
            .map(|hash| matches_hash(hash, &pin));
        (password_matches, pin_matches)
    })
    .await;
    let Ok((password_matches, pin_matches)) = checked else {
        return auth_error(worker_failed());
    };
    if !constant_eq(&snapshot.username, &username) || !password_matches {
        return auth_error(ApiError(
            StatusCode::BAD_REQUEST,
            MSG_ACCOUNT_DELETE_REJECTED.into(),
        ));
    }
    if pin_matches == Some(false) {
        return auth_error(ApiError(
            StatusCode::BAD_REQUEST,
            MSG_PARENT_PIN_INCORRECT.into(),
        ));
    }
    let worker = app.clone();
    let deleted = tokio::task::spawn_blocking(move || {
        let mut db = worker.db.lock().map_err(|_| unauthorized())?;
        let tx = db.transaction().map_err(crate::db_error)?;
        principal.validate_scope(&tx)?;
        delete_owned(&tx, &snapshot)?;
        tx.commit().map_err(crate::db_error)
    })
    .await;
    match deleted {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return auth_error(error),
        Err(_) => return auth_error(worker_failed()),
    }
    app.release_resources(|principal| principal.account_id() == Some(account));
    cleared_session()
}
