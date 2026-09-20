use super::*;
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use std::sync::OnceLock;

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
    let current = stored_pin(&db, account)?;
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
    let current = stored_pin(&db, app.identity().account_id())?;
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
        let existing = stored_pin(&db, account)?;
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
    let current = stored_pin(&tx, account)?;
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
