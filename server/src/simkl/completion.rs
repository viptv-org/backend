use super::*;

pub(super) fn confirmed(
    app: &App,
    profile: i64,
    id: &str,
    revision: u64,
) -> Result<bool, ApiError> {
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_profile(&db, profile)?;
    let state:Option<String>=db.query_row("SELECT state FROM simkl_tracking_receipts WHERE profile_id=?1 AND media_id=?2 AND link_revision=?3",params![profile,id,revision],|row|row.get(0)).optional().map_err(db_error)?;
    Ok(state.as_deref() == Some("done"))
}

pub(super) fn confirm(app: &App, profile: i64, id: &str, revision: u64) -> Result<(), ApiError> {
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_profile(&db, profile)?;
    db.execute("INSERT INTO simkl_tracking_receipts VALUES(?1,?2,?3,'done',?4) ON CONFLICT(profile_id,media_id,link_revision) DO UPDATE SET state='done',attempt_at=excluded.attempt_at",params![profile,id,revision,util::now()]).map_err(db_error)?;
    Ok(())
}
