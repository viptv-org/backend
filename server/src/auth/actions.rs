use super::*;
use axum::{extract::OriginalUri, http::HeaderMap, Json};

async fn enforce_rate_limit(
    app: App,
    bucket: String,
    limit: i64,
) -> Result<(), (ApiError, Option<i64>)> {
    match tokio::task::spawn_blocking(move || {
        let db = app.db.lock().map_err(|_| (unauthorized(), None))?;
        rate(&db, &bucket, limit).map_err(|error| (error, Some(retry_after(&db, &bucket))))
    })
    .await
    {
        Ok(Ok(())) => Ok(()),
        Ok(Err(pair)) => Err(pair),
        Err(_) => Err((
            ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Authentication admission worker failed".into(),
            ),
            None,
        )),
    }
}

pub(crate) async fn action(
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
        "/auth/register" | "/auth/login" | "/auth/device/login" | "/auth/recover"
    ) && !h.contains_key(header::ORIGIN)
    {
        return auth_error(forbidden());
    }
    if let Err(error) = if device_path(&path) {
        device_origin(&h)
    } else {
        origin(&h)
    } {
        return auth_error(error);
    }
    let global_bucket = format!("auth:global:{path}");
    let global_limit = endpoint_global_limit(&path);
    if let Err((error, retry_seconds)) =
        enforce_rate_limit(app.clone(), global_bucket, global_limit).await
    {
        return auth_error_with_retry(error, retry_seconds);
    }
    let subject = if matches!(
        path.as_str(),
        "/auth/login" | "/auth/device/login" | "/auth/recover" | "/auth/register"
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
    } else if let Some(account_id) = p.as_ref().and_then(|principal| principal.0.account_id()) {
        format!("account-{account_id}")
    } else {
        "anonymous".into()
    };
    let bucket = format!("auth:subject:{}:{}", path, hash(&subject));
    let subject_limit = if path == "/auth/register" {
        5
    } else if path.contains("lookup") || path.contains("token") {
        120
    } else {
        30
    };
    if let Err((error, retry_seconds)) = enforce_rate_limit(app.clone(), bucket, subject_limit).await {
        return auth_error_with_retry(error, retry_seconds);
    }
    if path == "/auth/account" {
        return super::delete_account(app, p, v).await;
    }
    let expensive = matches!(
        path.as_str(),
        "/auth/register" | "/auth/login" | "/auth/device/login" | "/auth/recover" | "/auth/password"
    );
    let permit =
        match acquire_auth_cpu(expensive, &AUTH_CPU, std::time::Duration::from_secs(2)).await {
            Ok(permit) => permit,
            Err(error) => return auth_error(error),
        };
    let result = tokio::task::spawn_blocking(move || {
        let snapshot = {
            let db = app.db.lock().map_err(|_| unauthorized())?;
            login_snapshot(&db, &path, &v, p.as_ref().and_then(|value| value.0.account_id()))?
        };
        let prepared = prepare_auth(&path, &v, snapshot)?;
        if path == "/auth/profile" {
            if let Some(principal) = p.as_ref() {
                crate::kids::profile_pin::leave_gate(&app, &principal.0, &v)?;
            }
        }
        drop(permit);
        let mut db = app.db.lock().map_err(|_| unauthorized())?;
        let tx = db.transaction().map_err(crate::db_error)?;
        let authority = match p.as_ref() {
            Some(principal) => principal.0.validate_scope(&tx),
            None => Ok(()),
        };
        let result = authority.and_then(|()| {
            dispatch_prepared(&tx, &path, p.map(|p| p.0), &h, &v, &prepared)
        });
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
pub(crate) fn dispatch(
    db: &Connection,
    path: &str,
    p: Option<Principal>,
    h: &HeaderMap,
    v: &Value,
    _fixture_token: &str,
) -> Result<(Value, Option<(String, String)>), ApiError> {
    let prepared = prepare_auth(
        path,
        v,
        login_snapshot(db, path, v, p.as_ref().and_then(|value| value.account_id()))?,
    )?;
    dispatch_prepared(db, path, p, h, v, &prepared)
}

pub(crate) fn dispatch_prepared(
    db: &Connection,
    path: &str,
    p: Option<Principal>,
    h: &HeaderMap,
    v: &Value,
    prepared: &PreparedAuth,
) -> Result<(Value, Option<(String, String)>), ApiError> {
    // The credential and session routes stay listed so a TV reaches their account_session_required reply.
    if matches!(p,Some(Principal::Account{ref role,..}) if role=="device")
        && !matches!(
            path,
            "/auth/profile"
                | "/auth/logout"
                | "/auth/device/lookup"
                | "/auth/device/approve"
                | "/auth/device/deny"
                | "/auth/device/revoke"
                | "/auth/device/revoke-own"
                | "/auth/password"
                | "/auth/recovery-codes"
                | "/auth/sessions"
        )
    {
        return Err(forbidden());
    }
    if let Some(ref principal) = p {
        if path != "/auth/profile" {
            crate::kids::require_parent(db, principal)?;
        }
    }
    match path {
        "/auth/password" => {
            let principal = p.as_ref().ok_or_else(unauthorized)?;
            principal.require_account_session()?;
            let Principal::Account {
                account_id,
                profile_id,
                ..
            } = principal;
            let stored: Option<String> = db
                .query_row(
                    "SELECT password_hash FROM auth_accounts WHERE id=?1 AND disabled=0",
                    params![account_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(crate::db_error)?;
            if !prepared.login_valid || stored.as_deref() != prepared.login_hash.as_deref() {
                return Err(ApiError(
                    StatusCode::BAD_REQUEST,
                    MSG_PASSWORD_INCORRECT.into(),
                ));
            }
            let password_hash = prepared.new_hash.as_ref().ok_or_else(unauthorized)?;
            let session_id = p
                .as_ref()
                .and_then(|value| value.session_id())
                .ok_or_else(unauthorized)?;
            let access_expires: i64 = db
                .query_row(
                    "SELECT access_expires FROM auth_sessions WHERE id=?1",
                    [session_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(crate::db_error)?
                .ok_or_else(unauthorized)?;
            db.execute(
                "UPDATE auth_accounts SET password_hash=?1 WHERE id=?2",
                params![password_hash, account_id],
            )
            .map_err(crate::db_error)?;
            event(db, "password_changed", Some(*account_id))?;
            let csrf_token = issue_csrf(db, session_id)?;
            Ok((
                json!({
                    "session_id": session_id,
                    "account_id": account_id,
                    "profile_id": profile_id,
                    "expires_in": (access_expires - now()).max(0),
                    "csrf_token": csrf_token
                }),
                None,
            ))
        }
        "/auth/recovery-codes" => {
            let principal = p.as_ref().ok_or_else(unauthorized)?;
            principal.require_account_session()?;
            let account_id = principal.account_id().ok_or_else(forbidden)?;
            let recovery = token();
            db.execute(
                "UPDATE auth_accounts SET recovery_hash=?1 WHERE id=?2",
                params![hash(&recovery), account_id],
            )
            .map_err(crate::db_error)?;
            event(db, "recovery_codes_regenerated", Some(account_id))?;
            Ok((json!({"recovery_codes": [recovery]}), None))
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
        "/auth/login" | "/auth/device/login" | "/auth/recover" => {
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
            let native = path == "/auth/device/login";
            let device_name = if native {
                let name = v["device_name"].as_str().unwrap_or("VIPTV Android").trim();
                if name.is_empty() || name.len() > 100 {
                    return Err("Invalid device name".into());
                }
                name
            } else {
                ""
            };
            let (mut out, c) = session(
                db,
                id,
                None,
                if native { "device" } else { "browser" },
                device_name,
            )?;
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
        _ => dispatch_profiles(db, path, p, h, v, prepared),
    }
}
