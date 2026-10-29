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
    total: u64,
    page: LivePage,
) -> ApiResult {
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
    let limit = query.limit.unwrap_or(100);
    let page_size = query.page_size.unwrap_or(limit);
    let before = u64::from(query.offset).min(total);
    let previous_limit = before.min(u64::from(page_size));
    let previous = (previous_limit > 0).then(|| json!({
        "cursor": (before - previous_limit).to_string(), "limit": previous_limit, "page_size":page_size,
    }));
    let next = value["next_cursor"].as_str().map(|cursor| json!({"cursor":cursor,"limit":page_size,"page_size":page_size}));
    value["paging"] = json!({"origin":query.offset.to_string(),"limit":limit,"page_size":page_size,"previous":previous,"next":next});
    Ok(axum::Json(value))
}
