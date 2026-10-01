//! Read-only migration fence for archived routing. V2 never applies a legacy
//! WARP/proxy policy or silently changes it before reviewed export.
use super::*;
pub(crate) fn table_exists(db: &Connection) -> Result<bool, String> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='provider_routes')",
        [],
        |row| row.get(0),
    )
    .map_err(db_error)
}
pub(crate) fn enabled(db: &Connection, id: i64) -> Result<bool, String> {
    use rusqlite::OptionalExtension;
    if !table_exists(db)? {
        return Ok(false);
    }
    db.query_row(
        "SELECT warp FROM provider_routes WHERE provider_id=?1",
        [id],
        |row| row.get(0),
    )
    .optional()
    .map(|value| value.unwrap_or(false))
    .map_err(db_error)
}
pub(crate) fn headers(db: &Connection, id: i64) -> Result<HashMap<String, String>, String> {
    if enabled(db, id)? {
        return Err("source_route_migration_required".into());
    }
    Ok(HashMap::new())
}
