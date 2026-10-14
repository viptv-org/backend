use super::*;
/// Resolve only persisted provider-verified equivalence, never caller-supplied IDs or titles.
pub(crate) fn remote(
    app: &App,
    profile: i64,
    kind: &str,
    id: &str,
) -> Result<Option<String>, ApiError> {
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_profile(&db, profile)?;
    let mut query=db.prepare("SELECT remote_id FROM simkl_import_entries WHERE profile_id=?1 AND local_kind=?2 AND local_id=?3 ORDER BY remote_id LIMIT 2").map_err(db_error)?;
    let ids = query
        .query_map(params![profile, kind, id], |r| r.get::<_, String>(0))
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    if ids.len() > 1 {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Verified title mapping is ambiguous".into(),
        ));
    }
    Ok(ids.into_iter().next())
}
