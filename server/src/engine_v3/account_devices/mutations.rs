use super::*;

pub(crate) async fn remove(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
) -> Result<Response, account_api::Error> {
    if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "Invalid device identity".into()).into());
    }
    let current = lease.session_id.clone().ok_or_else(auth::unauthorized)?;
    account_api::run(app, lease, move |db, account| {
        let tx = db.unchecked_transaction().map_err(db_error)?;
        let owned: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM auth_sessions WHERE id=?1 AND account_id=?2 AND kind IN ('browser','device'))",params![id,account],|row| row.get(0)).map_err(db_error)?;
        if !owned {
            return Err(ApiError(StatusCode::NOT_FOUND,"This device is unavailable.".into()).into());
        }
        if id == current {
            return Err(ApiError(StatusCode::CONFLICT,"Use Sign out to leave this browser.".into()).into());
        }
        tx.execute("DELETE FROM auth_sessions WHERE id=?1 AND account_id=?2",params![id,account]).map_err(db_error)?;
        tx.execute("DELETE FROM auth_refresh_used WHERE family=?1 AND account_id=?2",params![id,account]).map_err(db_error)?;
        auth::event(&tx,"session_revoked",Some(account))?;
        tx.commit().map_err(db_error)?;
        Ok(())
    }).await?;
    Ok(auth::response(json!({"signed_out":true}), None))
}

pub(crate) async fn remove_all(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
) -> Result<Response, account_api::Error> {
    account_api::run(app, lease, move |db, account| {
        let tx = db.unchecked_transaction().map_err(db_error)?;
        tx.execute("DELETE FROM auth_sessions WHERE account_id=?1", [account])
            .map_err(db_error)?;
        tx.execute(
            "DELETE FROM auth_refresh_used WHERE account_id=?1",
            [account],
        )
        .map_err(db_error)?;
        tx.execute("DELETE FROM auth_pairings WHERE account_id=?1", [account])
            .map_err(db_error)?;
        auth::event(&tx, "sessions_revoked", Some(account))?;
        tx.commit().map_err(db_error)?;
        Ok(())
    })
    .await?;
    Ok(auth::response(
        json!({"signed_out":true}),
        Some((String::new(), String::new())),
    ))
}
