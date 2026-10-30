use super::*;
use axum::http::HeaderMap;

pub(crate) fn dispatch_devices(
    db: &Connection,
    path: &str,
    p: Option<Principal>,
    h: &HeaderMap,
    v: &Value,
    prepared: &PreparedAuth,
) -> Result<(Value, Option<(String, String)>), ApiError> {
    match path {
        "/auth/device/deny" => {
            let principal = p.as_ref().ok_or_else(unauthorized)?;
            let code = pairing_code(v)?;
            let owner_account = db
                .query_row(
                    "SELECT account_id FROM auth_pairings WHERE code_hash=?1 AND expires>?2 AND denied=0",
                    params![hash(&code), now()],
                    |row| row.get::<_, Option<i64>>(0),
                )
                .optional()
                .map_err(crate::db_error)?
                .ok_or_else(|| {
                    ApiError(
                        StatusCode::BAD_REQUEST,
                        "Invalid or expired pairing code".into(),
                    )
                })?;
            if let Some(owner) = owner_account {
                if principal.account_id() != Some(owner) {
                    return Err(forbidden());
                }
            }
            db.execute(
                "UPDATE auth_pairings SET denied=1,account_id=NULL,profile_id=NULL WHERE code_hash=?1 AND expires>?2 AND denied=0",
                params![hash(&code), now()],
            )
            .map_err(crate::db_error)?;
            db.execute(
                "DELETE FROM auth_pairing_profiles WHERE code_hash=?1",
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
        "/auth/device/code" => {
            sweep_pairings(db)?;
            if let Some(device_code) = v
                .get("device_code")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|device_code| !device_code.is_empty())
            {
                let issued = now();
                db.execute(
                    "UPDATE auth_pairings SET expires=?1 WHERE device_hash=?2 AND account_id IS NULL AND denied=0 AND expires>?1",
                    params![issued, hash(device_code)],
                )
                .map_err(crate::db_error)?;
            }
            let pending: i64 = db
                .query_row(
                    "SELECT count(*) FROM auth_pairings WHERE account_id IS NULL AND denied=0 AND expires>?1",
                    [now()],
                    |row| row.get(0),
                )
                .map_err(crate::db_error)?;
            if pending >= MAX_PENDING_PAIRINGS {
                db.execute(
                    "DELETE FROM auth_pairings WHERE code_hash IN(SELECT code_hash FROM auth_pairings WHERE account_id IS NULL AND denied=0 AND expires>?1 ORDER BY expires,code_hash LIMIT ?2)",
                    params![now(), pending - MAX_PENDING_PAIRINGS + 1],
                )
                .map_err(crate::db_error)?;
            }
            let total: i64 = db
                .query_row("SELECT count(*) FROM auth_pairings", [], |row| row.get(0))
                .map_err(crate::db_error)?;
            if total >= MAX_PAIRINGS {
                db.execute(
                    "DELETE FROM auth_pairings WHERE code_hash IN(SELECT code_hash FROM auth_pairings ORDER BY denied=0 AND expires>?1,account_id IS NOT NULL,expires,code_hash LIMIT ?2)",
                    params![now(), total - MAX_PAIRINGS + 1],
                )
                .map_err(crate::db_error)?;
            }
            let name = v["device_name"].as_str().unwrap_or("TV").trim();
            if name.is_empty() || name.len() > 100 {
                return Err("Invalid device name".into());
            }
            let (user_code, device_code) = device_pairing::create_pairing(db, name)?;
            let origin = canonical_origin()?
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
                "SELECT device_name,expires,account_id IS NOT NULL FROM auth_pairings WHERE code_hash=?1 AND expires>?2 AND denied=0",
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
            sweep_pairings(db)?;
            let approved_count: i64 = db
                .query_row(
                    "SELECT count(*) FROM auth_pairings WHERE account_id=?1 AND expires>?2",
                    params![id, now()],
                    |row| row.get(0),
                )
                .map_err(crate::db_error)?;
            if approved_count >= MAX_APPROVED_PAIRINGS_PER_ACCOUNT {
                return Err(ApiError(
                    StatusCode::TOO_MANY_REQUESTS,
                    "Too many approved devices awaiting connection".into(),
                ));
            }
            let changed=db.execute("UPDATE auth_pairings SET account_id=?1,profile_id=NULL WHERE code_hash=?2 AND expires>?3 AND account_id IS NULL AND denied=0",params![id,code_hash,now()]).map_err(crate::db_error)?;
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
            let (account_id, device_name) = redeemable_pairing(db, &device_hash)?;
            let (output, cookies) = session(db, account_id, None, "device", &device_name)?;
            db.execute(
                "DELETE FROM auth_pairings WHERE device_hash=?1",
                [device_hash],
            )
            .map_err(crate::db_error)?;
            event(db, "device_paired", Some(account_id))?;
            Ok((output, cookies))
        }
        "/auth/device/revoke" | "/auth/device/revoke-own" | "/auth/device/revoke-admin" => {
            let principal = p.as_ref().ok_or_else(unauthorized)?;
            let own = principal.session_id();
            let device_principal = principal.is_device();
            if !device_principal {
                principal.require_account_owner()?;
            }
            let target = match v.get("session_id") {
                Some(_) => field(v, "session_id", 100)?,
                None if device_principal => own.ok_or_else(unauthorized)?,
                None => return Err("Missing session_id".into()),
            };
            if device_principal && own != Some(target) {
                return Err(forbidden());
            }
            let owner = db
                .query_row(
                    "SELECT account_id FROM auth_sessions WHERE id=?1 AND kind='device'",
                    [target],
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
                [target],
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

fn sweep_pairings(db: &Connection) -> Result<(), ApiError> {
    db.execute(
        "DELETE FROM auth_pairings WHERE expires<=?1",
        [now() - PAIRING_TOMBSTONE_SECS],
    )
    .map_err(crate::db_error)?;
    Ok(())
}

fn redeemable_pairing(db: &Connection, device_hash: &str) -> Result<(i64, String), ApiError> {
    let pairing = db
        .query_row(
            "SELECT p.denied<>0,p.expires<=?2,CASE WHEN a.disabled=0 THEN p.account_id END,p.device_name FROM auth_pairings p LEFT JOIN auth_accounts a ON a.id=p.account_id WHERE p.device_hash=?1",
            params![device_hash, now()],
            |r| {
                Ok((
                    r.get::<_, bool>(0)?,
                    r.get::<_, bool>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )
        .optional()
        .map_err(crate::db_error)?;
    let error = |message: &str| ApiError(StatusCode::BAD_REQUEST, message.into());
    let Some((denied, expired, approver, device_name)) = pairing else {
        return Err(error(MSG_AUTHORIZATION_PENDING));
    };
    if denied {
        return Err(error(MSG_ACCESS_DENIED));
    }
    if expired {
        return Err(error(MSG_EXPIRED_TOKEN));
    }
    let account_id = approver.ok_or_else(|| error(MSG_AUTHORIZATION_PENDING))?;
    Ok((account_id, device_name))
}
