//! Complete toggle membership without projecting cards or reading a provider.
use super::*;
use viptv_engine::identity::MediaIdentity;
pub(crate) fn snapshot(app: &App, lease: &ResourceLease, profile: i64) -> Result<Value, ApiError> {
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_profile(&db, profile)?;
    lease.validate(&db)?;
    let allowed = kids::sql_allowed("f.type", "f.id", "?1");
    let mut query=db.prepare(&format!("SELECT f.id,f.type FROM favorites f WHERE f.profile_id=?1 AND f.type IN ('movie','series') AND {allowed} ORDER BY f.type,f.id LIMIT 10001")).map_err(db_error)?;
    let keys = query
        .query_map([profile], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    if keys.len() > 10000 {
        return Err("Library exceeds its bounded item limit".into());
    }
    let mut identities = keys
        .iter()
        .map(|(id, kind)| MediaIdentity {
            id: id.clone(),
            kind: kind.clone(),
            ids: vec![],
            episode: None,
            edition: None,
        })
        .collect::<Vec<_>>();
    let mut aliases=db.prepare(&format!("SELECT i.statistics FROM simkl_import_entries i JOIN favorites f ON f.profile_id=i.profile_id AND f.type=i.local_kind AND f.id=i.local_id WHERE f.profile_id=?1 AND {allowed} ORDER BY i.remote_kind,i.remote_id LIMIT 10001")).map_err(db_error)?;
    let mut seen = keys.into_iter().collect::<std::collections::HashSet<_>>();
    let values = aliases
        .query_map([profile], |r| r.get::<_, String>(0))
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    if values.len() > 10000 {
        return Err("Library aliases exceed the bounded item limit".into());
    }
    for value in values {
        if let Ok(statistics) =
            serde_json::from_str::<viptv_engine::library::LibraryStatistics>(&value)
        {
            for provenance in statistics.provenance {
                let id = provenance.external_identity;
                if kids::require_item(&db, &lease.principal, &id.kind, &id.id).is_ok()
                    && seen.insert((id.id.clone(), id.kind.clone()))
                {
                    identities.push(id);
                }
            }
        }
    }
    let revision = super::super::history::revision(&db, profile)?;
    let configuration: u64 = db
        .query_row(
            "SELECT revision FROM engine_profile_configuration WHERE profile_id=?1",
            [profile],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?
        .unwrap_or(0);
    serde_json::to_value(viptv_engine::library::LibraryMembershipSnapshot {
        identities,
        revision: revision.saturating_add(configuration),
        complete: true,
    })
    .map_err(|_| ApiError::from("Membership unavailable"))
}
pub(crate) async fn get(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
) -> ApiResult {
    let app = app.with_lease(lease.clone());
    Ok(axum::Json(snapshot(&app, &lease, profile)?))
}
pub(super) fn resolve(
    db: &Connection,
    profile: i64,
    kind: &str,
    id: &str,
) -> Result<String, ApiError> {
    db.query_row("SELECT local_id FROM simkl_import_entries WHERE profile_id=?1 AND remote_kind=?2 AND remote_id=?3",params![profile,kind,id],|r|r.get(0)).optional().map_err(db_error).map(|mapped|mapped.unwrap_or_else(||id.to_owned()))
}
