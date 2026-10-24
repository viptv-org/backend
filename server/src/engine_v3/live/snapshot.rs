//! Opaque pagination identity shared by remote consumers. Offsets are valid
//! only within one returned snapshot; clients discard mixed page windows.
use super::*;

pub(super) fn catalog_revision(db: &Connection, provider: i64) -> Result<String, ApiError> {
    let revision: i64 = db
        .query_row(
            "SELECT generation FROM provider_live_generations WHERE provider_id=?1",
            [provider],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_error)?
        .unwrap_or(0);
    Ok(format!("iptv:{provider}:{revision}"))
}

pub(super) fn project(
    app: &App,
    profile: i64,
    configuration_revision: u64,
    query: &LiveQuery,
    catalog: &str,
    page: LivePage,
) -> ApiResult {
    // Include the complete advertised connection set and query scope, so a
    // configuration/ownership change cannot retain an old page's metadata.
    let identity = json!({
        "account": app.identity().account_id().ok_or_else(auth::unauthorized)?,
        "profile": profile, "configuration": configuration_revision,
        "connections": &page.connections, "catalog": catalog,
        "connection": query.connection_id, "group": query.group_id, "search": query.search,
    });
    let snapshot_id = format!(
        "live-v1:{:x}",
        Sha256::digest(identity.to_string().as_bytes())
    );
    let mut value = json!(page);
    value["snapshot_id"] = json!(snapshot_id);
    Ok(axum::Json(value))
}
