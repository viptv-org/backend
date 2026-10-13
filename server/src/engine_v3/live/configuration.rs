use super::*;
pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS engine_m3u_connections(id INTEGER PRIMARY KEY AUTOINCREMENT,account_id INTEGER NOT NULL,profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,revision INTEGER NOT NULL,enabled INTEGER NOT NULL,secret TEXT NOT NULL,UNIQUE(account_id,profile_id))")
}
pub(crate) fn connections(
    db: &Connection,
    account: i64,
    profile: i64,
) -> Result<Vec<IntegrationConnection>, ApiError> {
    let mut statement=db.prepare("SELECT p.id,p.name,p.enabled,p.enable_live FROM providers p JOIN provider_ownership o ON o.provider_id=p.id WHERE o.account_id=?1 ORDER BY p.id LIMIT 64").map_err(db_error)?;
    let mut connections = statement
        .query_map([account], |r| {
            Ok(IntegrationConnection {
                id: format!("iptv:{}", r.get::<_, i64>(0)?),
                plugin_id: "xtream".into(),
                account_id: account.to_string(),
                private_profile_id: None,
                name: r.get(1)?,
                enabled: r.get(2)?,
                revision: 0,
                capabilities: if r.get::<_, bool>(3)? {
                    vec![Capability::LiveTv, Capability::Sources]
                } else {
                    vec![Capability::Sources]
                },
            })
        })
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    if cfg!(feature = "live-runtime") {
        let mut statement=db.prepare("SELECT id,revision,enabled FROM engine_m3u_connections WHERE account_id=?1 AND profile_id=?2").map_err(db_error)?;
        let m3u = statement
            .query_map(params![account, profile], |r| {
                Ok(IntegrationConnection {
                    id: format!("m3u:{}", r.get::<_, i64>(0)?),
                    plugin_id: "m3u".into(),
                    account_id: account.to_string(),
                    private_profile_id: Some(profile.to_string()),
                    name: "M3U live TV".into(),
                    enabled: r.get(2)?,
                    revision: r.get(1)?,
                    capabilities: vec![Capability::LiveTv, Capability::Sources],
                })
            })
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        connections.extend(m3u);
    }
    Ok(connections)
}
pub(crate) async fn configure(
    app: App,
    lease: ResourceLease,
    profile: i64,
    revision: u64,
    plugin: &str,
    value: Value,
) -> ApiResult {
    let app = app.with_lease(lease.clone());
    let account = lease.principal.account_id().ok_or_else(auth::forbidden)?;
    let (mut config, _) = super::super::configuration::read(&app, &lease, profile)?;
    if config.scope.revision != revision {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Profile configuration changed".into(),
        ));
    }
    {
        let db = app.db.lock().unwrap();
        app.require_profile(&db, profile)?;
        kids::require_parent(&db, &lease.principal)?;
    }
    let vault = app
        .secret_vault
        .clone()
        .ok_or("Credential vault unavailable")?;
    if plugin == "xtream" {
        let credentials: XtreamConfiguration = serde_json::from_value(value)
            .map_err(|_| ApiError::from("Invalid Xtream configuration"))?;
        credentials
            .validate()
            .map_err(|_| ApiError::from("Invalid Xtream configuration"))?;
        let input:crate::provider::connections_v2::Input=serde_json::from_value(json!({"name":"Xtream live TV","url":credentials.server_url,"username":credentials.username,"password":credentials.password,"enabled":true,"enable_live":true,"enable_movies":false,"enable_series":false})).map_err(|_|ApiError::from("Invalid Xtream configuration"))?;
        let axum::Json(created) = crate::provider::connections_v2::create(
            State(app.clone()),
            Extension(lease.clone()),
            Ok(axum::Json(input)),
        )
        .await
        .map_err(|_| ApiError::from("Xtream connection could not be verified"))?;
        let id = created["id"]
            .as_i64()
            .ok_or("Live connection unavailable")?;
        let mut db = app.db.lock().unwrap();
        let tx = db.transaction().map_err(db_error)?;
        let current: u64 = tx
            .query_row(
                "SELECT revision FROM engine_profile_configuration WHERE profile_id=?1",
                [profile],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?
            .unwrap_or(0);
        if current != revision {
            tx.execute("DELETE FROM providers WHERE id=?1", [id])
                .map_err(db_error)?;
            tx.commit().map_err(db_error)?;
            return Err(ApiError(
                StatusCode::CONFLICT,
                "Profile configuration changed".into(),
            ));
        }
        config
            .assignments
            .push(viptv_engine::profile::ProfileAssignment {
                connection_id: format!("iptv:{id}"),
                enabled: vec![Capability::LiveTv, Capability::Sources],
            });
        super::super::source_integrations::save_revision(&tx, &mut config, account)?;
        tx.commit().map_err(db_error)?;
        return Ok(axum::Json(json!({"configuration":config})));
    }
    if !cfg!(feature = "live-runtime") || plugin != "m3u" {
        return Err("Live runtime unavailable".into());
    }
    let credentials: M3uConfiguration = serde_json::from_value(value)
        .map_err(|_| ApiError::from("Invalid playlist configuration"))?;
    credentials
        .validate()
        .map_err(|_| ApiError::from("Invalid playlist configuration"))?;
    let mut db = app.db.lock().unwrap();
    app.require_profile(&db, profile)?;
    lease.validate(&db)?;
    let tx = db.transaction().map_err(db_error)?;
    let current: u64 = tx
        .query_row(
            "SELECT revision FROM engine_profile_configuration WHERE profile_id=?1",
            [profile],
            |r| r.get(0),
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
            "SELECT id FROM engine_m3u_connections WHERE account_id=?1 AND profile_id=?2",
            params![account, profile],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?;
    let id = if let Some(id) = id {
        id
    } else {
        tx.execute("INSERT INTO engine_m3u_connections(account_id,profile_id,revision,enabled,secret)VALUES(?1,?2,0,1,'')",params![account,profile]).map_err(db_error)?;
        tx.last_insert_rowid()
    };
    let secret = vault
        .seal(
            account,
            "m3u",
            &format!("{profile}:{id}"),
            serde_json::to_vec(&credentials)
                .map_err(|_| ApiError::from("Invalid configuration"))?
                .as_slice(),
        )
        .map_err(|_| ApiError::from("Credentials could not be saved"))?;
    tx.execute(
        "UPDATE engine_m3u_connections SET secret=?2,revision=revision+1,enabled=1 WHERE id=?1",
        params![id, secret],
    )
    .map_err(db_error)?;
    let connection = format!("m3u:{id}");
    if !config
        .assignments
        .iter()
        .any(|a| a.connection_id == connection)
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
pub(crate) async fn remove(
    app: App,
    lease: ResourceLease,
    profile: i64,
    connection: String,
    revision: u64,
) -> ApiResult {
    let app = app.with_lease(lease.clone());
    let account = lease.principal.account_id().ok_or_else(auth::forbidden)?;
    let (mut config, _) = super::super::configuration::read(&app, &lease, profile)?;
    let mut db = app.db.lock().unwrap();
    app.require_profile(&db, profile)?;
    kids::require_parent(&db, &lease.principal)?;
    let tx = db.transaction().map_err(db_error)?;
    let current: u64 = tx
        .query_row(
            "SELECT revision FROM engine_profile_configuration WHERE profile_id=?1",
            [profile],
            |r| r.get(0),
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
    let (kind, id) = connection.split_once(':').ok_or_else(auth::forbidden)?;
    let id = id.parse::<i64>().map_err(|_| auth::forbidden())?;
    let changed=if kind=="m3u"{tx.execute("DELETE FROM engine_m3u_connections WHERE id=?1 AND account_id=?2 AND profile_id=?3",params![id,account,profile])}else if kind=="iptv"{tx.execute("DELETE FROM providers WHERE id=?1 AND EXISTS(SELECT 1 FROM provider_ownership WHERE provider_id=?1 AND account_id=?2)",params![id,account])}else{return Err(auth::forbidden());}.map_err(db_error)?;
    if changed != 1 {
        return Err(auth::forbidden());
    }
    config.assignments.retain(|a| a.connection_id != connection);
    super::super::source_integrations::save_revision(&tx, &mut config, account)?;
    tx.commit().map_err(db_error)?;
    Ok(axum::Json(json!({"configuration":config})))
}
pub(crate) fn available_m3u(db: &Connection, lease: &ResourceLease, id: i64) -> bool {
    let auth::Principal::Account {
        profile_id: Some(profile),
        ..
    } = lease.principal
    else {
        return false;
    };
    cfg!(feature="live-runtime") && db.query_row("SELECT EXISTS(SELECT 1 FROM engine_m3u_connections c JOIN engine_profile_configuration p ON p.profile_id=c.profile_id JOIN json_each(p.value,'$.assignments') a WHERE c.id=?1 AND c.account_id=?2 AND c.profile_id=?3 AND c.enabled=1 AND json_extract(a.value,'$.connection_id')=?4 AND EXISTS(SELECT 1 FROM json_each(a.value,'$.enabled') f WHERE f.value='LiveTv'))",params![id,lease.principal.account_id(),profile,format!("m3u:{id}")],|r|r.get::<_,bool>(0)).unwrap_or(false)
}
