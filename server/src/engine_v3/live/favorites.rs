use super::*;
use viptv_engine::{profile::ProfileConfiguration, registry::Registry};
const MAX_FAVORITES: usize = 10_000;

pub(super) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS engine_live_favorites(profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,channel_id TEXT NOT NULL,connection_id TEXT NOT NULL,connection_revision INTEGER NOT NULL,PRIMARY KEY(profile_id,channel_id));")
}

fn allowed(
    config: &ProfileConfiguration,
    connections: &[IntegrationConnection],
) -> Result<Vec<IntegrationConnection>, ApiError> {
    let mut effective = config.clone();
    effective.assignments.retain(|assignment| {
        connections
            .iter()
            .any(|connection| connection.id == assignment.connection_id)
    });
    for assignment in &mut effective.assignments {
        if let Some(connection) = connections
            .iter()
            .find(|connection| connection.id == assignment.connection_id)
        {
            assignment
                .enabled
                .retain(|capability| connection.capabilities.contains(capability));
        }
    }
    Registry::builtin()
        .connections_for(&effective, connections, Capability::LiveTv)
        .map(|connections| connections.into_iter().cloned().collect())
        .map_err(|_| ApiError::from("Live connections unavailable"))
}

fn validate(
    db: &Connection,
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    expected: u64,
) -> Result<(), ApiError> {
    app.require_profile(db, profile)?;
    lease.validate(db)?;
    kids::require_parent(db, &lease.principal)?;
    let revision: u64 = db
        .query_row(
            "SELECT revision FROM engine_profile_configuration WHERE profile_id=?1",
            [profile],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_error)?
        .unwrap_or(0);
    if revision != expected {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Profile configuration changed".into(),
        ));
    }
    Ok(())
}

fn saved(db: &Connection, profile: i64) -> Result<Vec<(String, String, u64)>, ApiError> {
    let mut query=db.prepare("SELECT channel_id,connection_id,connection_revision FROM engine_live_favorites WHERE profile_id=?1 ORDER BY channel_id LIMIT 10001").map_err(db_error)?;
    let rows = query
        .query_map([profile], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    if rows.len() > MAX_FAVORITES {
        return Err("Live favorites exceed their limit".into());
    }
    Ok(rows)
}

fn known(
    db: &Connection,
    caches: &HashMap<String, Cache>,
    config: &ProfileConfiguration,
    connections: &[IntegrationConnection],
    wanted: &std::collections::HashSet<String>,
) -> Result<HashMap<String, (String, u64)>, ApiError> {
    let mut found = HashMap::new();
    let enabled = connections
        .iter()
        .map(|connection| connection.id.as_str())
        .collect::<std::collections::HashSet<_>>();
    for connection in connections.iter().filter(|connection| {
        connection.id.starts_with("m3u:") || connection.id.starts_with("xtream:")
    }) {
        if let Some(cache) = caches
            .get(&connection.id)
            .filter(|cache| cache.revision == config.scope.revision as i64)
        {
            for entry in &cache.channels {
                if entry.channel.connection_id == connection.id
                    && wanted.contains(&entry.channel.id)
                {
                    found.insert(
                        entry.channel.id.clone(),
                        (connection.id.clone(), connection.revision),
                    );
                }
            }
        }
    }
    let ids = wanted.iter().collect::<Vec<_>>();
    for batch in ids.chunks(400) {
        let placeholders = vec!["?"; batch.len()].join(",");
        let mut query = db
            .prepare(&format!(
                "SELECT id,provider_id FROM provider_live WHERE id IN ({placeholders})"
            ))
            .map_err(db_error)?;
        let rows = query
            .query_map(
                rusqlite::params_from_iter(batch.iter().map(|id| id.as_str())),
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .map_err(db_error)?;
        for row in rows {
            let (id, provider) = row.map_err(db_error)?;
            let connection = format!("iptv:{provider}");
            if enabled.contains(connection.as_str()) {
                found.insert(id, (connection, 0));
            }
        }
    }
    Ok(found)
}

async fn execute(
    app: App,
    lease: ResourceLease,
    profile: i64,
    change: Option<(String, bool)>,
) -> ApiResult {
    let app = app.with_lease(lease.clone());
    {
        let db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        app.require_profile(&db, profile)?;
        kids::require_parent(&db, &lease.principal)?;
    }
    let (config, _) = super::super::configuration::read(&app, &lease, profile)?;
    // No provider IO while the live-cache and DB locks are held.
    let caches = app.live_cache.lock().await;
    let mut db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    let tx = db.transaction().map_err(db_error)?;
    validate(&tx, &app, &lease, profile, config.scope.revision)?;
    let account = lease.principal.account_id().ok_or_else(auth::forbidden)?;
    let connections = allowed(&config, &configuration::connections(&tx, account, profile)?)?;
    let before = saved(&tx, profile)?;
    let mut wanted = before
        .iter()
        .map(|(id, _, _)| id.clone())
        .collect::<std::collections::HashSet<_>>();
    if let Some((id, _)) = &change {
        wanted.insert(id.clone());
    }
    let registry = known(&tx, &caches, &config, &connections, &wanted)?;
    if let Some((id, add)) = change {
        let existing = before.iter().any(|(channel, _, _)| channel == &id);
        if add {
            let (connection, connection_revision) =
                registry.get(&id).ok_or_else(auth::forbidden)?;
            if !existing && before.len() >= MAX_FAVORITES {
                return Err(ApiError(
                    StatusCode::CONFLICT,
                    "Live favorite limit reached".into(),
                ));
            }
            tx.execute("INSERT INTO engine_live_favorites(profile_id,channel_id,connection_id,connection_revision)VALUES(?1,?2,?3,?4) ON CONFLICT(profile_id,channel_id) DO UPDATE SET connection_id=excluded.connection_id,connection_revision=excluded.connection_revision",params![profile,id,connection,connection_revision]).map_err(db_error)?;
        } else {
            if !existing && !registry.contains_key(&id) {
                return Err(auth::forbidden());
            }
            tx.execute(
                "DELETE FROM engine_live_favorites WHERE profile_id=?1 AND channel_id=?2",
                params![profile, id],
            )
            .map_err(db_error)?;
        }
    }
    let channel_ids = saved(&tx, profile)?
        .into_iter()
        .filter(|(id, connection, revision)| {
            registry.get(id) == Some(&(connection.clone(), *revision))
        })
        .map(|(id, _, _)| id)
        .collect::<Vec<_>>();
    tx.commit().map_err(db_error)?;
    Ok(axum::Json(json!({"channel_ids":channel_ids})))
}

pub(crate) async fn get(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
) -> ApiResult {
    execute(app, lease, profile, None).await
}
fn channel(id: &str) -> Result<(), ApiError> {
    if id.is_empty() || id.len() > 512 || id.chars().any(char::is_control) {
        return Err("Invalid live channel".into());
    }
    Ok(())
}
pub(crate) async fn add(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path((profile, id)): Path<(i64, String)>,
) -> ApiResult {
    channel(&id)?;
    execute(app, lease, profile, Some((id, true))).await
}
pub(crate) async fn remove(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path((profile, id)): Path<(i64, String)>,
) -> ApiResult {
    channel(&id)?;
    execute(app, lease, profile, Some((id, false))).await
}

#[cfg(test)]
#[path = "favorites_tests.rs"]
mod tests;
