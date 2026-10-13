use super::*;
use viptv_engine::{
    capabilities::ExecutionMode,
    profile::{IntegrationConnection, ProfileAssignment, ProfileConfiguration},
    registry::Registry,
};

pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS engine_profile_configuration(profile_id INTEGER PRIMARY KEY REFERENCES profiles(id) ON DELETE CASCADE,account_id INTEGER NOT NULL,revision INTEGER NOT NULL,value TEXT NOT NULL);")
}

pub(crate) fn apply(
    mut app: App,
    lease: &ResourceLease,
    profile: i64,
    cap: viptv_engine::capabilities::Capability,
) -> Result<(App, u64), ApiError> {
    let (configuration, connections) = read(&app, lease, profile)?;
    // Removed connections are omitted rather than reviving a stale assignment.
    let mut effective = configuration.clone();
    effective
        .assignments
        .retain(|a| connections.iter().any(|c| c.id == a.connection_id));
    let allowed = Registry::builtin()
        .connections_for(&effective, &connections, cap)
        .map_err(|_| ApiError::from("Integration unavailable"))?;
    let mut ids: Vec<i64> = allowed
        .iter()
        .filter_map(|c| c.id.strip_prefix("addon:")?.parse().ok())
        .collect();
    if configuration.scope.revision == 0 && cap == viptv_engine::capabilities::Capability::Sources {
        // Default assignments include enabled account addons. An unreadable legacy
        // credential must still publish its safe migration error without egress.
        // Explicit saved assignments continue to exclude unassigned connections.
        let (_, errors) = app.addons.entries_with_errors()?;
        ids.extend(errors.into_iter().map(|(id, _)| id));
    }
    app.addons = app.addons.with_engine_ids(ids);
    Ok((app, configuration.scope.revision))
}

pub(crate) fn validate_revision(app: &App, profile: i64, expected: u64) -> Result<(), ApiError> {
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_profile(&db, profile)?;
    let current: u64 = db
        .query_row(
            "SELECT revision FROM engine_profile_configuration WHERE profile_id=?1",
            [profile],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?
        .unwrap_or(0);
    if current != expected {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Profile configuration changed".into(),
        ));
    }
    Ok(())
}

pub(crate) fn read(
    app: &App,
    lease: &ResourceLease,
    profile: i64,
) -> Result<(ProfileConfiguration, Vec<IntegrationConnection>), ApiError> {
    let account = lease.principal.account_id().ok_or_else(auth::forbidden)?;
    let mut connections = app.addons.engine_connections(account)?;
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_profile(&db, profile)?;
    if app.simkl.configured() && app.secret_vault.is_some() {
        connections.extend(crate::simkl::connections(&db, account, profile)?);
    }
    connections.extend(super::source_integrations::connections(&db,account,profile)?);
    let stored: Option<String> = db
        .query_row(
            "SELECT value FROM engine_profile_configuration WHERE profile_id=?1 AND account_id=?2",
            params![profile, account],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?;
    let mut configuration:ProfileConfiguration = if let Some(value) = stored {
        serde_json::from_str(&value)
            .map_err(|_| ApiError::from("Profile configuration unavailable"))?
    } else {
        ProfileConfiguration {
            scope: scope(lease, profile, 0),
            mode: ExecutionMode::Server,
            name: db
                .query_row("SELECT name FROM profiles WHERE id=?1", [profile], |r| {
                    r.get(0)
                })
                .map_err(db_error)?,
            avatar_id: None,
            browsing_plugin: if connections.iter().any(|c| c.plugin_id == "simkl") {
                "simkl"
            } else {
                "stremio"
            }
            .into(),
            assignments: connections
                .iter()
                .map(|c| ProfileAssignment {
                    connection_id: c.id.clone(),
                    enabled: c.capabilities.clone(),
                })
                .collect(),
        }
    };
    configuration.name=db.query_row("SELECT name FROM profiles WHERE id=?1",[profile],|r|r.get(0)).map_err(db_error)?;
    configuration.avatar_id=super::profiles::avatar(&db,profile)?;
    Ok((configuration, connections))
}

pub(crate) async fn get(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
) -> ApiResult {
    let app = app.with_lease(lease.clone());
    let (configuration, connections) = read(&app, &lease, profile)?;
    Ok(axum::Json(
        json!({"configuration":configuration,"connections":connections}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Update {
    revision: u64,
    browsing_plugin: String,
    assignments: Vec<ProfileAssignment>,
}

pub(crate) async fn update(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    axum::Json(update): axum::Json<Update>,
) -> ApiResult {
    let app = app.with_lease(lease.clone());
    let (mut configuration, connections) = read(&app, &lease, profile)?;
    if update.assignments.len() > 128
        || !matches!(update.browsing_plugin.as_str(), "stremio" | "simkl")
    {
        return Err("Integration is not available".into());
    }
    if update.browsing_plugin == "simkl"
        && !connections.iter().any(|c| {
            c.plugin_id == "simkl"
                && update.assignments.iter().any(|a| {
                    a.connection_id == c.id
                        && a.enabled
                            .contains(&viptv_engine::capabilities::Capability::Discovery)
                })
        })
    {
        return Err("Enable Simkl discovery for this profile first".into());
    }
    configuration.browsing_plugin = update.browsing_plugin;
    configuration.assignments = update.assignments;
    Registry::builtin()
        .validate(&configuration, &connections)
        .map_err(|_| ApiError::from("Invalid integration assignments"))?;
    let mut db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_profile(&db, profile)?;
    kids::require_parent(&db, &lease.principal)?;
    let tx = db.transaction().map_err(db_error)?;
    let revision: u64 = tx
        .query_row(
            "SELECT revision FROM engine_profile_configuration WHERE profile_id=?1",
            [profile],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?
        .unwrap_or(0);
    if revision != update.revision {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Profile configuration changed".into(),
        ));
    }
    configuration.scope.revision = revision
        .checked_add(1)
        .ok_or_else(|| ApiError::from("Configuration revision unavailable"))?;
    tx.execute("INSERT INTO engine_profile_configuration VALUES(?1,?2,?3,?4) ON CONFLICT(profile_id) DO UPDATE SET revision=excluded.revision,value=excluded.value",params![profile,lease.principal.account_id(),configuration.scope.revision,serde_json::to_string(&configuration).map_err(|_|ApiError::from("Invalid profile configuration"))?]).map_err(db_error)?;
    tx.commit().map_err(db_error)?;
    Ok(axum::Json(json!({"configuration":configuration})))
}
