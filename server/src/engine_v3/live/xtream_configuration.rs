use super::*;

pub(super) fn connections(
    db: &Connection,
    account: i64,
    profile: i64,
) -> Result<Vec<IntegrationConnection>, ApiError> {
    let mut query=db.prepare("SELECT id,revision,enabled FROM engine_xtream_connections WHERE account_id=?1 AND profile_id=?2").map_err(db_error)?;
    let result = query
        .query_map(params![account, profile], |row| {
            Ok(IntegrationConnection {
                id: format!("xtream:{}", row.get::<_, i64>(0)?),
                plugin_id: "xtream".into(),
                account_id: account.to_string(),
                private_profile_id: Some(profile.to_string()),
                name: "Xtream live TV".into(),
                enabled: row.get(2)?,
                revision: row.get(1)?,
                capabilities: vec![Capability::LiveTv, Capability::Sources],
            })
        })
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error);
    result
}
pub(super) fn available(db: &Connection, lease: &ResourceLease, id: i64) -> bool {
    let auth::Principal::Account {
        profile_id: Some(profile),
        ..
    } = lease.principal
    else {
        return false;
    };
    db.query_row("SELECT EXISTS(SELECT 1 FROM engine_xtream_connections c JOIN engine_profile_configuration p ON p.profile_id=c.profile_id JOIN json_each(p.value,'$.assignments') a WHERE c.id=?1 AND c.account_id=?2 AND c.profile_id=?3 AND c.enabled=1 AND json_extract(a.value,'$.connection_id')=?4 AND EXISTS(SELECT 1 FROM json_each(a.value,'$.enabled') f WHERE f.value='LiveTv'))",params![id,lease.principal.account_id(),profile,format!("xtream:{id}")],|row|row.get(0)).unwrap_or(false)
}
pub(super) fn credential(
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    connection: &IntegrationConnection,
) -> Result<XtreamConfiguration, ApiError> {
    let id = connection
        .id
        .strip_prefix("xtream:")
        .and_then(|id| id.parse::<i64>().ok())
        .ok_or_else(auth::forbidden)?;
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    lease.validate(&db)?;
    app.require_profile(&db, profile)?;
    if !available(&db, lease, id) {
        return Err(auth::forbidden());
    }
    let secret:String=db.query_row("SELECT secret FROM engine_xtream_connections WHERE id=?1 AND account_id=?2 AND profile_id=?3",params![id,lease.principal.account_id(),profile],|row|row.get(0)).map_err(db_error)?;
    let vault = app
        .secret_vault
        .as_ref()
        .ok_or("Credential vault unavailable")?;
    let opened = vault
        .open(
            lease.principal.account_id().unwrap(),
            "xtream-profile",
            &format!("{profile}:{id}"),
            &secret,
        )
        .map_err(|_| ApiError::from("Live credentials unavailable"))?;
    serde_json::from_slice(opened.expose())
        .map_err(|_| ApiError::from("Live credentials unavailable"))
}
pub(super) fn save(
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    revision: u64,
    mut config: viptv_engine::profile::ProfileConfiguration,
    value: Value,
) -> ApiResult {
    let credentials: XtreamConfiguration = serde_json::from_value(value)
        .map_err(|_| ApiError::from("Invalid Xtream configuration"))?;
    credentials
        .validate()
        .map_err(|_| ApiError::from("Invalid Xtream configuration"))?;
    let account = lease.principal.account_id().ok_or_else(auth::forbidden)?;
    let vault = app
        .secret_vault
        .as_ref()
        .ok_or("Credential vault unavailable")?;
    let mut db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    lease.validate(&db)?;
    app.require_profile(&db, profile)?;
    kids::require_parent(&db, &lease.principal)?;
    let tx = db.transaction().map_err(db_error)?;
    let current: u64 = tx
        .query_row(
            "SELECT revision FROM engine_profile_configuration WHERE profile_id=?1",
            [profile],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_error)?
        .unwrap_or(0);
    if current != revision {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Profile configuration changed".into(),
        ));
    }
    let id: Option<i64> = tx
        .query_row(
            "SELECT id FROM engine_xtream_connections WHERE account_id=?1 AND profile_id=?2",
            params![account, profile],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_error)?;
    let id = if let Some(id) = id {
        id
    } else {
        tx.execute("INSERT INTO engine_xtream_connections(account_id,profile_id,revision,enabled,secret) VALUES(?1,?2,0,1,'')",params![account,profile]).map_err(db_error)?;
        tx.last_insert_rowid()
    };
    let secret = vault
        .seal(
            account,
            "xtream-profile",
            &format!("{profile}:{id}"),
            &serde_json::to_vec(&credentials)
                .map_err(|_| ApiError::from("Invalid Xtream configuration"))?,
        )
        .map_err(|_| ApiError::from("Credentials could not be saved"))?;
    tx.execute(
        "UPDATE engine_xtream_connections SET secret=?2,revision=revision+1,enabled=1 WHERE id=?1",
        params![id, secret],
    )
    .map_err(db_error)?;
    let connection = format!("xtream:{id}");
    if !config
        .assignments
        .iter()
        .any(|assignment| assignment.connection_id == connection)
    {
        config
            .assignments
            .push(viptv_engine::profile::ProfileAssignment {
                connection_id: connection,
                enabled: vec![Capability::LiveTv, Capability::Sources],
            });
    }
    super::super::source_integrations::save_revision(&tx, &mut config, account)?;
    tx.commit().map_err(db_error)?;
    Ok(axum::Json(json!({"configuration":config})))
}

pub(super) fn validate(
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    revision: u64,
    connection: &IntegrationConnection,
) -> Result<(), ApiError> {
    super::super::configuration::validate_revision(app, profile, revision)?;
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    lease.validate(&db)?;
    if !super::configuration::available_private(&db, lease, connection) {
        return Err(auth::forbidden());
    }
    Ok(())
}
