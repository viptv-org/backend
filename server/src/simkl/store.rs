use super::*;

pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    super::imports::init(db)?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS simkl_links(profile_id INTEGER PRIMARY KEY REFERENCES profiles(id) ON DELETE CASCADE, account_id INTEGER NOT NULL REFERENCES auth_accounts(id) ON DELETE CASCADE, revision INTEGER NOT NULL, credential TEXT);
        CREATE TABLE IF NOT EXISTS simkl_tracking_receipts(profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,media_id TEXT NOT NULL,link_revision INTEGER NOT NULL,state TEXT NOT NULL,attempt_at INTEGER NOT NULL,PRIMARY KEY(profile_id,media_id,link_revision));
        CREATE TABLE IF NOT EXISTS simkl_oauth_states(state_hash TEXT PRIMARY KEY,profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,account_id INTEGER NOT NULL,session_id TEXT NOT NULL,role TEXT NOT NULL,policy_revision INTEGER NOT NULL,link_revision INTEGER NOT NULL,expires INTEGER NOT NULL);")?;
    let columns = db
        .prepare("PRAGMA table_info(simkl_oauth_states)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !columns.iter().any(|c| c == "verifier") {
        db.execute_batch("ALTER TABLE simkl_oauth_states ADD COLUMN verifier TEXT NOT NULL DEFAULT ''; DELETE FROM simkl_oauth_states;")?;
    }
    Ok(())
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

/// Whether the profile holds a Simkl credential. The plugin connection exists
/// even when it does not, so a read-only lookup must ask this instead of the
/// browsing plugin alone.
pub(crate) fn linked(app: &App, profile: i64) -> Result<bool, ApiError> {
    let account = app.identity().account_id().ok_or_else(auth::forbidden)?;
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_profile(&db, profile)?;
    linked_db(&db, account, profile)
}

pub(crate) fn linked_db(db: &Connection, account: i64, profile: i64) -> Result<bool, ApiError> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM simkl_links WHERE profile_id=?1 AND account_id=?2 AND credential IS NOT NULL)",
        params![profile, account],
        |row| row.get(0),
    )
    .map_err(db_error)
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
    pub parent_scope: Option<crate::kids::parent_scope::ParentScope>,
    pub revision: Option<u64>,
    pub capability: Capability,
    pub configuration_revision: u64,
    pub token: Option<crate::secret_store::SecretBytes>,
}

pub(super) fn credential(
    app: &App,
    profile: i64,
    capability: Capability,
) -> Result<Credential, ApiError> {
    credential_access(app, profile, capability, None)
}
pub(super) fn credential_parent(
    app: &App,
    parent: &crate::kids::parent_scope::ParentScope,
    capability: Capability,
) -> Result<Credential, ApiError> {
    if !matches!(capability, Capability::Search | Capability::Metadata) {
        return Err(auth::forbidden());
    }
    credential_access(app, parent.profile(), capability, Some(parent))
}
pub(super) fn reload_credential(
    app: &App,
    profile: i64,
    credential: &Credential,
) -> Result<Credential, ApiError> {
    credential_access(
        app,
        profile,
        credential.capability,
        credential.parent_scope.as_ref(),
    )
}
pub(super) fn credential_access(
    app: &App,
    profile: i64,
    capability: Capability,
    parent: Option<&crate::kids::parent_scope::ParentScope>,
) -> Result<Credential, ApiError> {
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    if let Some(parent) = parent {
        if parent.profile() != profile {
            return Err(auth::forbidden());
        }
        parent.validate(&db)?;
    } else {
        app.require_profile(&db, profile)?;
    }
    assigned(&db, profile, capability)?;
    let account = app.identity().account_id().ok_or_else(auth::forbidden)?;
    if !app.simkl.configured() || app.secret_vault.is_none() {
        return Err(unavailable());
    }
    let record: Option<(u64,String)> = db.query_row(
        "SELECT revision,credential FROM simkl_links WHERE profile_id=?1 AND account_id=?2 AND credential IS NOT NULL", params![profile,account], |r| Ok((r.get(0)?,r.get(1)?))
    ).optional().map_err(db_error)?;
    let revision = record.as_ref().map(|r| r.0);
    if capability == Capability::Tracking && record.is_none() {
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "Link Simkl for this profile".into(),
        ));
    }
    let token = record
        .map(|(_, sealed)| {
            app.secret_vault
                .as_ref()
                .ok_or_else(unavailable)?
                .open(account, "simkl", &profile.to_string(), &sealed)
                .map_err(|_| unavailable())
        })
        .transpose()?;
    Ok(Credential {
        parent_scope: parent.cloned(),
        revision,
        configuration_revision: configuration_revision(&db, profile)?,
        capability,
        token,
    })
}

pub(super) fn validate(app: &App, profile: i64, credential: &Credential) -> Result<(), ApiError> {
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    require_scope(&db, app, profile, credential)?;
    assigned(&db, profile, credential.capability)?;
    if configuration_revision(&db, profile)? != credential.configuration_revision {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Profile configuration changed".into(),
        ));
    }
    let revision: Option<u64> = db.query_row("SELECT revision FROM simkl_links WHERE profile_id=?1 AND account_id=?2 AND credential IS NOT NULL",params![profile, app.identity().account_id()], |r| r.get(0)).optional().map_err(db_error)?;
    if revision != credential.revision {
        return Err(ApiError(StatusCode::CONFLICT, "Simkl link changed".into()));
    }
    Ok(())
}

pub(super) fn require_scope(
    db: &Connection,
    app: &App,
    profile: i64,
    credential: &Credential,
) -> Result<(), ApiError> {
    if let Some(parent) = &credential.parent_scope {
        if parent.profile() != profile {
            return Err(auth::forbidden());
        }
        parent.validate(db)
    } else {
        app.require_profile(db, profile)
    }
}

pub(super) async fn request(
    app: &App,
    profile: i64,
    credential: &Credential,
    path: &str,
    body: Option<Value>,
) -> Result<Value, ApiError> {
    let token = tokens::access(app, profile, credential).await?;
    let token = token.as_deref().map(String::as_str);
    // Public documents stay credential-free unless this application requires OAuth.
    let initial_token = (credential.capability == Capability::Tracking)
        .then_some(token)
        .flatten();
    let result = app
        .simkl
        .request_checked(path, initial_token, body.clone(), || {
            validate(app, profile, credential)
        })
        .await;
    let value = match result {
        Err(error)
            if error.1 == "Simkl authorization is required"
                && initial_token.is_none()
                && token.is_some() =>
        {
            app.simkl
                .request_checked(path, token, body, || validate(app, profile, credential))
                .await?
        }
        result => result?,
    };
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

fn configuration_revision(db: &Connection, profile: i64) -> Result<u64, ApiError> {
    db.query_row(
        "SELECT revision FROM engine_profile_configuration WHERE profile_id=?1",
        [profile],
        |r| r.get(0),
    )
    .optional()
    .map_err(db_error)
    .map(|value| value.unwrap_or(0))
}

pub(super) async fn private_read(
    app: &App,
    profile: i64,
    credential: &Credential,
    path: &str,
) -> Result<Value, ApiError> {
    let raw = credential.token.as_ref().ok_or_else(|| {
        ApiError(
            StatusCode::UNAUTHORIZED,
            "Link Simkl for this profile".into(),
        )
    })?;
    if let Ok(document) = serde_json::from_slice::<Value>(raw.expose()) {
        if !document["scope"]
            .as_str()
            .is_some_and(|scope| scope.split_whitespace().any(|s| s == "media:read"))
        {
            return Err(ApiError(
                StatusCode::FORBIDDEN,
                "Simkl read authorization is unavailable".into(),
            ));
        }
    }
    let token = tokens::access(app, profile, credential)
        .await?
        .ok_or_else(|| {
            ApiError(
                StatusCode::UNAUTHORIZED,
                "Link Simkl for this profile".into(),
            )
        })?;
    validate(app, profile, credential)?;
    let value = app
        .simkl
        .request_checked(path, Some(&token), None, || {
            validate(app, profile, credential)
        })
        .await?;
    validate(app, profile, credential)?;
    Ok(value)
}
