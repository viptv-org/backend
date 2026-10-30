use super::*;

pub(crate) fn stored(db: &Connection, profile: i64) -> Result<Option<String>, ApiError> {
    let hash: Option<String> = db
        .query_row(
            "SELECT leave_pin_hash FROM kids_profiles WHERE profile_id=?1",
            [profile],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?
        .flatten();
    Ok(hash)
}
fn attempts(db: &Connection, profile: i64) -> Result<(i64, i64), ApiError> {
    db.query_row(
        "SELECT leave_failures,leave_blocked_until FROM kids_profiles WHERE profile_id=?1",
        [profile],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
    .map_err(db_error)
    .map(|row| row.unwrap_or((0, 0)))
}
fn record(db: &Connection, profile: i64, failures: i64, blocked_until: i64) -> Result<(), ApiError> {
    db.execute(
        "UPDATE kids_profiles SET leave_failures=?1,leave_blocked_until=?2 WHERE profile_id=?3",
        params![failures, blocked_until, profile],
    )
    .map_err(db_error)?;
    Ok(())
}
// Must stay outside the /auth/profile transaction: a rejected attempt has to survive the rollback
// that follows its error, or the five-attempt lockout never sticks.
pub(crate) fn leave_gate(app: &App, p: &auth::Principal, value: &Value) -> Result<(), ApiError> {
    let auth::Principal::Account {
        profile_id: Some(current),
        ..
    } = p
    else {
        return Ok(());
    };
    let target = value["profile_id"]
        .as_i64()
        .or_else(|| value["profile_id"].as_str().and_then(|id| id.parse().ok()));
    if target == Some(*current) {
        return Ok(());
    }
    let (hash, failures, blocked_until) = {
        let db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        p.validate_scope(&db)?;
        require_parent(&db, p)?;
        let (failures, blocked_until) = attempts(&db, *current)?;
        (stored(&db, *current)?, failures, blocked_until)
    };
    let Some(hash) = hash else {
        return Ok(());
    };
    let now = util::now();
    if blocked_until > now {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many PIN attempts. Try again in five minutes".into(),
        ));
    }
    let Some(pin) = value["pin"].as_str() else {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            MSG_LEAVE_PIN_REQUIRED.into(),
        ));
    };
    let next = if blocked_until > 0 { 1 } else { failures + 1 };
    {
        let db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        p.validate_scope(&db)?;
        record(&db, *current, next, if next >= 5 { now + 300 } else { 0 })?;
    }
    if !pin::verify_hash(pin, &hash) {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            MSG_LEAVE_PIN_INVALID.into(),
        ));
    }
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    p.validate_scope(&db)?;
    if stored(&db, *current)?.as_deref() != Some(hash.as_str()) {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "PIN changed. Try again".into(),
        ));
    }
    record(&db, *current, 0, 0)
}
fn owns(db: &Connection, p: &auth::Principal, profile: i64) -> Result<(), ApiError> {
    p.require_account_session()?;
    let account = p.account_id().ok_or_else(auth::unauthorized)?;
    let owned: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM profile_owners WHERE profile_id=?1 AND account_id=?2)",
            params![profile, account],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    if !owned {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Profile access denied".into(),
        ));
    }
    Ok(())
}
pub(crate) async fn status(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<i64>,
) -> ApiResult {
    let app = app.with_lease(lease);
    blocking(move || {
        let db = app.db.lock().unwrap();
        app.request_lease().validate(&db)?;
        owns(&db, &app.identity(), id)?;
        Ok(axum::Json(
            json!({"availability":"Available","message":Value::Null,"configured":stored(&db,id)?.is_some()}),
        ))
    })
    .await
}
pub(crate) async fn set(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<i64>,
    axum::Json(value): axum::Json<Value>,
) -> ApiResult {
    let app = app.with_lease(lease);
    let existing = {
        let db = app.db.lock().unwrap();
        app.request_lease().validate(&db)?;
        manager(&db, &app.identity(), Some(id))?;
        stored(&db, id)?
    };
    if let Some(existing) = &existing {
        let current = pin::pin_text(&value, "current_pin")?;
        let expected = existing.clone();
        let valid = pin::pin_cpu(&app, move || Ok(pin::verify_hash(&current, &expected))).await?;
        if !valid {
            return Err(ApiError(
                StatusCode::FORBIDDEN,
                MSG_LEAVE_PIN_INVALID.into(),
            ));
        }
    }
    let removing = value["pin"].as_str() == Some("");
    let next = if removing {
        None
    } else {
        let pin = pin::pin_text(&value, "pin")?;
        Some(pin::pin_cpu(&app, move || pin::new_hash(&pin)).await?)
    };
    let mut db = app.db.lock().unwrap();
    let tx = db.transaction().map_err(db_error)?;
    app.request_lease().validate(&tx)?;
    manager(&tx, &app.identity(), Some(id))?;
    if stored(&tx, id)? != existing {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "PIN changed. Try again".into(),
        ));
    }
    if let Some(next) = next {
        tx.execute("INSERT INTO kids_profiles(profile_id,leave_pin_hash,leave_failures,leave_blocked_until) VALUES(?1,?2,0,0) ON CONFLICT(profile_id) DO UPDATE SET leave_pin_hash=excluded.leave_pin_hash,leave_failures=0,leave_blocked_until=0",params![id,next]).map_err(db_error)?;
    } else {
        tx.execute(
            "UPDATE kids_profiles SET leave_pin_hash=NULL,leave_failures=0,leave_blocked_until=0 WHERE profile_id=?1",
            [id],
        )
        .map_err(db_error)?;
    }
    tx.commit().map_err(db_error)?;
    Ok(axum::Json(json!({"configured":!removing})))
}