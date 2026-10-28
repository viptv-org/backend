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
            // Deny only a live pairing: pending codes are deniable by the
            // signed-in browser user, approved codes only by their owning
            // account. Expired, consumed, denied or unknown codes are refused
            // with the same wording lookup and approve already use.
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
            // Keep a tombstone so the polling device reads `access_denied`
            // rather than `authorization_pending`; the sweep removes it later.
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
            // A TV asking for a fresh code identifies itself by the device_code
            // it already holds. Expire that device's earlier pending code so a
            // superseded code can never be approved or redeemed: its token poll
            // reads expired_token and lookup/approve refuse it with the usual
            // wording. The row stays as a tombstone (see sweep_pairings); an
            // approved grant or another device's row is left untouched.
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
                // Tombstones (denied or expired) go first, then pending, then
                // approved. Under a flood of new codes a denied or expired
                // device can therefore lose its tombstone before the window
                // ends and read `authorization_pending` again until its own
                // client-side expiry.
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

/// Drop pairings whose tombstone window has passed. Rows between their expiry
/// and the end of the window stay so a late poll still reads `expired_token`
/// or `access_denied`; every live-row query filters on `expires` and `denied`.
fn sweep_pairings(db: &Connection) -> Result<(), ApiError> {
    db.execute(
        "DELETE FROM auth_pairings WHERE expires<=?1",
        [now() - PAIRING_TOMBSTONE_SECS],
    )
    .map_err(crate::db_error)?;
    Ok(())
}

/// The approved pairing a polling device may redeem, or its RFC 8628
/// section 3.5 error. Unknown codes (never issued, consumed or swept) stay
/// `authorization_pending` so a typo can never read as a denial.
fn redeemable_pairing(db: &Connection, device_hash: &str) -> Result<(i64, String), ApiError> {
    let pairing = db
        .query_row(
            // Third column: the approving account, only while it is enabled.
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
    // Still pending, or approved by an account that has since been disabled.
    let account_id = approver.ok_or_else(|| error(MSG_AUTHORIZATION_PENDING))?;
    Ok((account_id, device_name))
}
