use super::*;

pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS simkl_links(profile_id INTEGER PRIMARY KEY REFERENCES profiles(id) ON DELETE CASCADE, account_id INTEGER NOT NULL REFERENCES auth_accounts(id) ON DELETE CASCADE, revision INTEGER NOT NULL, credential TEXT);
        CREATE TABLE IF NOT EXISTS simkl_oauth_states(state_hash TEXT PRIMARY KEY,profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,account_id INTEGER NOT NULL,session_id TEXT NOT NULL,role TEXT NOT NULL,policy_revision INTEGER NOT NULL,link_revision INTEGER NOT NULL,expires INTEGER NOT NULL);")
}

pub(crate) fn connections(
    db: &Connection,
    account: i64,
    profile: i64,
) -> Result<Vec<IntegrationConnection>, ApiError> {
    let revision: Option<u64> = db.query_row("SELECT revision FROM simkl_links WHERE account_id=?1 AND profile_id=?2 AND credential IS NOT NULL", params![account,profile], |r| r.get(0)).optional().map_err(db_error)?;
    let supported = capabilities();
    Ok(vec![IntegrationConnection {
        id: format!("simkl:{profile}"),
        plugin_id: "simkl".into(),
        account_id: account.to_string(),
        private_profile_id: Some(profile.to_string()),
        name: "Simkl".into(),
        enabled: true,
        revision: revision.unwrap_or(0),
        capabilities: supported,
    }])
}

pub(super) fn save_token(
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    revision: u64,
    token: &str,
) -> Result<(), ApiError> {
    let account = lease.principal.account_id().ok_or_else(auth::forbidden)?;
    let vault = app.secret_vault.as_ref().ok_or_else(unavailable)?;
    let sealed = vault
        .seal(account, "simkl", &profile.to_string(), token.as_bytes())
        .map_err(|_| unavailable())?;
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    lease.validate(&db)?;
    kids::require_parent(&db, &lease.principal)?;
    let changed = db.execute("UPDATE simkl_links SET credential=?4,revision=revision+1 WHERE profile_id=?1 AND account_id=?2 AND revision=?3", params![profile,account,revision,sealed]).map_err(db_error)?;
    if changed != 1 {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Simkl link changed. Start again.".into(),
        ));
    }
    Ok(())
}

pub(super) struct Credential {
    pub revision: Option<u64>,
    pub capability: Capability,
    pub token: Option<crate::secret_store::SecretBytes>,
}

pub(super) fn credential(
    app: &App,
    profile: i64,
    capability: Capability,
) -> Result<Credential, ApiError> {
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_profile(&db, profile)?;
    assigned(&db, profile, capability)?;
    let account = app.identity().account_id().ok_or_else(auth::forbidden)?;
    if !app.simkl.configured() || app.secret_vault.is_none() {
        return Err(unavailable());
    }
    let record: Option<(u64,String)> = db.query_row(
        "SELECT revision,credential FROM simkl_links WHERE profile_id=?1 AND account_id=?2 AND credential IS NOT NULL", params![profile,account], |r| Ok((r.get(0)?,r.get(1)?))
    ).optional().map_err(db_error)?;
    let revision = record.as_ref().map(|r| r.0);
    let token = if capability == Capability::Tracking {
        let (_, sealed) = record.ok_or_else(|| {
            ApiError(
                StatusCode::UNAUTHORIZED,
                "Link Simkl for this profile".into(),
            )
        })?;
        Some(
            app.secret_vault
                .as_ref()
                .ok_or_else(unavailable)?
                .open(account, "simkl", &profile.to_string(), &sealed)
                .map_err(|_| unavailable())?,
        )
    } else {
        None
    };
    Ok(Credential {
        revision,
        capability,
        token,
    })
}

pub(super) fn validate(app: &App, profile: i64, credential: &Credential) -> Result<(), ApiError> {
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_profile(&db, profile)?;
    assigned(&db, profile, credential.capability)?;
    let revision: Option<u64> = db.query_row("SELECT revision FROM simkl_links WHERE profile_id=?1 AND account_id=?2 AND credential IS NOT NULL",params![profile, app.identity().account_id()], |r| r.get(0)).optional().map_err(db_error)?;
    if revision != credential.revision {
        return Err(ApiError(StatusCode::CONFLICT, "Simkl link changed".into()));
    }
    Ok(())
}

pub(super) async fn request(
    app: &App,
    profile: i64,
    credential: &Credential,
    path: &str,
    body: Option<Value>,
) -> Result<Value, ApiError> {
    let token = credential
        .token
        .as_ref()
        .map(|token| std::str::from_utf8(token.expose()))
        .transpose()
        .map_err(|_| unavailable())?;
    let value = app
        .simkl
        .request_checked(path, token, body, || validate(app, profile, credential))
        .await?;
    validate(app, profile, credential)?;
    Ok(value)
}

fn assigned(db: &Connection, profile: i64, capability: Capability) -> Result<(), ApiError> {
    let saved: Option<String> = db
        .query_row(
            "SELECT value FROM engine_profile_configuration WHERE profile_id=?1",
            [profile],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?;
    if let Some(saved) = saved {
        let config: viptv_engine::profile::ProfileConfiguration = serde_json::from_str(&saved)
            .map_err(|_| ApiError::from("Profile configuration unavailable"))?;
        if !config.assignments.iter().any(|a| {
            a.connection_id == format!("simkl:{profile}") && a.enabled.contains(&capability)
        }) {
            return Err(ApiError(
                StatusCode::FORBIDDEN,
                "Simkl capability is disabled for this profile".into(),
            ));
        }
    }
    Ok(())
}
