//! Profile-private SDK credentials. Public records contain no credential values.
use super::*;
use viptv_engine::{
    capabilities::Capability,
    profile::IntegrationConnection,
    source_integration::{vsources_schema, VsourcesConfiguration},
};
pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS engine_source_integrations(id INTEGER PRIMARY KEY,account_id INTEGER NOT NULL,profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,revision INTEGER NOT NULL,enabled INTEGER NOT NULL,secret TEXT NOT NULL,UNIQUE(account_id,profile_id));")
}
pub(crate) fn connections(
    db: &Connection,
    account: i64,
    profile: i64,
) -> Result<Vec<IntegrationConnection>, ApiError> {
    if !cfg!(feature = "vsources-runtime") {
        return Ok(vec![]);
    }
    let mut query=db.prepare("SELECT id,revision,enabled FROM engine_source_integrations WHERE account_id=?1 AND profile_id=?2").map_err(db_error)?;
    let result = query
        .query_map(params![account, profile], |r| {
            Ok(IntegrationConnection {
                id: format!("vsources:{}", r.get::<_, i64>(0)?),
                plugin_id: "vsources".into(),
                account_id: account.to_string(),
                private_profile_id: Some(profile.to_string()),
                name: "VIPTV Sources".into(),
                enabled: r.get::<_, bool>(2)?,
                revision: r.get(1)?,
                capabilities: vec![Capability::Sources],
            })
        })
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error);
    result
}
pub(crate) async fn schemas(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
) -> ApiResult {
    let app = app.with_lease(lease.clone());
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_profile(&db, profile)?;
    let id: Option<i64> = db
        .query_row(
            "SELECT id FROM engine_source_integrations WHERE account_id=?1 AND profile_id=?2",
            params![lease.principal.account_id(), profile],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?;
    let mut schema = vsources_schema(id.is_some(), id.map(|id| format!("vsources:{id}")));
    if app.secret_vault.is_none() {
        schema.state = viptv_engine::source_integration::IntegrationState::Unavailable;
        schema.runtime_enabled = false;
    }
    // The saved provider *count* (never the ids) feeds the list's
    // "TMDB key saved · 8 providers" line; an empty selection means the default set.
    #[cfg(feature = "vsources-runtime")]
    if let (Some(id), Some(vault)) = (id, app.secret_vault.as_ref()) {
        if let Ok(envelope) = db.query_row(
            "SELECT secret FROM engine_source_integrations WHERE id=?1",
            [id],
            |r| r.get::<_, String>(0),
        ) {
            if let Ok(secret) = vault.open(
                lease.principal.account_id().unwrap_or_default(),
                "vsources",
                &format!("{profile}:{id}"),
                &envelope,
            ) {
                if let Ok(saved) = serde_json::from_slice::<VsourcesConfiguration>(secret.expose()) {
                    let selected = saved.providers.len() as u32;
                    schema.saved_provider_count = Some(if selected > 0 {
                        selected
                    } else {
                        viptv_engine::vsources_runtime::engine(
                            &saved,
                            Arc::new(viptv_engine::vsources_runtime::ScopedFetcher::new(
                                Arc::new(|| false),
                            )),
                        )
                        .map(|engine| engine.providers().len() as u32)
                        .unwrap_or(0)
                    });
                }
            }
        }
    }
    drop(db);
    let (configuration, connections) = configuration::read(&app, &lease, profile)?;
    let mut schemas = vec![schema];
    for plugin in ["xtream", "m3u"] {
        let connection = connections.iter().find(|c| c.plugin_id == plugin);
        schemas.push(viptv_engine::source_integration::live_schema(
            plugin,
            connection.is_some(),
            connection.map(|c| c.id.clone()),
            plugin == "xtream" || cfg!(feature = "live-runtime"),
        ));
    }
    Ok(axum::Json(
        json!({"schemas":schemas,"revision":configuration.scope.revision,"connections":connections.into_iter().filter(|c|["vsources","xtream","m3u"].contains(&c.plugin_id.as_str())).collect::<Vec<_>>()}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Configure {
    revision: u64,
    plugin_id: String,
    configuration: Value,
}
pub(crate) async fn configure(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    axum::Json(update): axum::Json<Configure>,
) -> ApiResult {
    if update.plugin_id == "xtream" || update.plugin_id == "m3u" {
        return super::live::configure(
            app,
            lease,
            profile,
            update.revision,
            &update.plugin_id,
            update.configuration,
        )
        .await;
    }
    let credentials: VsourcesConfiguration = serde_json::from_value(update.configuration)
        .map_err(|_| ApiError::from("Invalid integration configuration"))?;
    if !cfg!(feature = "vsources-runtime") || update.plugin_id != "vsources" {
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "Integration runtime unavailable".into(),
        ));
    }
    credentials
        .validate()
        .map_err(|_| ApiError::from("Invalid integration configuration"))?;
    #[cfg(feature = "vsources-runtime")]
    viptv_engine::vsources_runtime::engine(
        &credentials,
        Arc::new(viptv_engine::vsources_runtime::ScopedFetcher::new(
            Arc::new(|| false),
        )),
    )
    .map_err(|_| ApiError::from("Invalid provider selection"))?;
    let app = app.with_lease(lease.clone());
    let account = lease.principal.account_id().ok_or_else(auth::forbidden)?;
    let vault = app
        .secret_vault
        .as_ref()
        .ok_or("Credential vault unavailable")?;
    let (mut config, _) = configuration::read(&app, &lease, profile)?;
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
    let id: Option<i64> = tx
        .query_row(
            "SELECT id FROM engine_source_integrations WHERE account_id=?1 AND profile_id=?2",
            params![account, profile],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?;
    let id = if let Some(id) = id {
        id
    } else {
        tx.execute("INSERT INTO engine_source_integrations(account_id,profile_id,revision,enabled,secret)VALUES(?1,?2,0,1,'')",params![account,profile]).map_err(db_error)?;
        tx.last_insert_rowid()
    };
    let record = format!("{profile}:{id}");
    let secret = vault
        .seal(
            account,
            "vsources",
            &record,
            serde_json::to_vec(&credentials)
                .map_err(|_| ApiError::from("Invalid configuration"))?
                .as_slice(),
        )
        .map_err(|_| ApiError::from("Credentials could not be saved"))?;
    tx.execute(
        "UPDATE engine_source_integrations SET secret=?2,revision=revision+1,enabled=1 WHERE id=?1",
        params![id, secret],
    )
    .map_err(db_error)?;
    let connection_id = format!("vsources:{id}");
    if !config
        .assignments
        .iter()
        .any(|a| a.connection_id == connection_id)
    {
        config
            .assignments
            .push(viptv_engine::profile::ProfileAssignment {
                connection_id,
                enabled: vec![Capability::Sources],
            });
    }
    save_revision(&tx, &mut config, account)?;
    tx.commit().map_err(db_error)?;
    Ok(axum::Json(json!({"configuration":config})))
}
pub(crate) fn save_revision(
    tx: &Connection,
    config: &mut viptv_engine::profile::ProfileConfiguration,
    account: i64,
) -> Result<(), ApiError> {
    config.scope.revision = config
        .scope
        .revision
        .checked_add(1)
        .ok_or("Configuration revision unavailable")?;
    tx.execute("INSERT INTO engine_profile_configuration VALUES(?1,?2,?3,?4) ON CONFLICT(profile_id) DO UPDATE SET revision=excluded.revision,value=excluded.value",params![config.scope.profile_id,account,config.scope.revision,serde_json::to_string(config).map_err(|_|ApiError::from("Configuration unavailable"))?]).map_err(db_error)?;
    Ok(())
}
#[derive(Deserialize)]
pub(crate) struct Remove {
    revision: u64,
}
pub(crate) async fn remove(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path((profile, id)): Path<(i64, String)>,
    Query(remove): Query<Remove>,
) -> ApiResult {
    if !id.starts_with("vsources:") {
        return super::live::remove(app, lease, profile, id, remove.revision).await;
    }
    let id = id
        .strip_prefix("vsources:")
        .and_then(|id| id.parse::<i64>().ok())
        .ok_or_else(auth::forbidden)?;
    let app = app.with_lease(lease.clone());
    let account = lease.principal.account_id().ok_or_else(auth::forbidden)?;
    let (mut config, _) = configuration::read(&app, &lease, profile)?;
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
    if revision != remove.revision || config.scope.revision != revision {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Profile configuration changed".into(),
        ));
    }
    if tx.execute("DELETE FROM engine_source_integrations WHERE id=?1 AND account_id=?2 AND profile_id=?3",params![id,account,profile]).map_err(db_error)?!=1{return Err(auth::forbidden());}
    config
        .assignments
        .retain(|a| a.connection_id != format!("vsources:{id}"));
    save_revision(&tx, &mut config, account)?;
    tx.commit().map_err(db_error)?;
    Ok(axum::Json(json!({"configuration":config})))
}
/// Used at publication and every admission; profile revisions bind saved assignments.
pub(crate) fn available(app: &App, id: i64) -> bool {
    if !cfg!(feature = "vsources-runtime") {
        return false;
    }
    let lease = app.request_lease();
    let auth::Principal::Account {
        profile_id: Some(profile),
        ..
    } = lease.principal
    else {
        return false;
    };
    let Ok((configuration, connections)) = configuration::read(app, &lease, profile) else {
        return false;
    };
    viptv_engine::registry::Registry::builtin()
        .connections_for(&configuration, &connections, Capability::Sources)
        .is_ok_and(|connections| connections.iter().any(|c| c.id == format!("vsources:{id}")))
}
#[cfg(feature = "vsources-runtime")]
mod runtime;
#[cfg(feature = "vsources-runtime")]
pub(crate) use runtime::start;
#[cfg(not(feature = "vsources-runtime"))]
pub(crate) async fn start(
    _app: &App,
    _value: &Value,
) -> Vec<(
    String,
    viptv_engine::presentation::Card,
    viptv_engine::source_integration::VsourcesConfiguration,
    [u8; 32],
    u64,
)> {
    vec![]
}
pub(crate) fn available_db(db: &Connection, lease: &ResourceLease, id: i64) -> bool {
    let auth::Principal::Account {
        profile_id: Some(profile),
        ..
    } = lease.principal
    else {
        return false;
    };
    cfg!(feature="vsources-runtime") && db.query_row("SELECT EXISTS(SELECT 1 FROM engine_source_integrations s JOIN engine_profile_configuration p ON p.profile_id=s.profile_id JOIN json_each(p.value,'$.assignments') a WHERE s.id=?1 AND s.account_id=?2 AND s.profile_id=?3 AND s.enabled=1 AND json_extract(a.value,'$.connection_id')=?4 AND EXISTS(SELECT 1 FROM json_each(a.value,'$.enabled') c WHERE c.value='Sources'))",params![id,lease.principal.account_id(),profile,format!("vsources:{id}")],|r|r.get::<_,bool>(0)).unwrap_or(false)
}
#[cfg(all(test, feature = "vsources-runtime"))]
pub(crate) use runtime::canonical_card;
#[cfg(feature = "vsources-runtime")]
pub(crate) use runtime::spawn;

pub(crate) fn cache_detail(
    app: &App,
    lease: &ResourceLease,
    revision: u64,
    addon_revision: &str,
    value: &Value,
) {
    let Ok(detail) =
        serde_json::from_value::<viptv_engine::presentation::DetailSnapshot>(value.clone())
    else {
        return;
    };
    for card in std::iter::once(detail.item).chain(detail.episodes) {
        cache_card_at(app, lease, revision, addon_revision, card);
    }
}

#[cfg(test)]
pub(crate) fn cache_card(
    app: &App,
    lease: &ResourceLease,
    revision: u64,
    card: viptv_engine::presentation::Card,
) {
    let Some(account) = lease.principal.account_id() else {
        return;
    };
    let Ok(addon_revision) = app.addons.clone().for_account(account).revision() else {
        return;
    };
    cache_card_at(app, lease, revision, &addon_revision, card);
}

pub(crate) fn cache_card_at(
    app: &App,
    lease: &ResourceLease,
    revision: u64,
    addon_revision: &str,
    card: viptv_engine::presentation::Card,
) {
    let Ok(mut cache) = app.verified_cards.lock() else {
        return;
    };
    // Evict a slice on overflow; clearing all 8192 entries on one insert turned
    // the cap into a full-cache stampede.
    if cache.len() > 8192 {
        let drop: Vec<_> = cache.keys().take(2048).cloned().collect();
        for key in drop {
            cache.remove(&key);
        }
    }
    cache.insert(
        (
            lease.principal.key(),
            revision,
            card.provider_id.clone(),
            card.identity.kind.clone(),
            card.identity.id.clone(),
            addon_revision.to_owned(),
        ),
        card,
    );
}

pub(crate) fn merge_cached(
    app: &App,
    lease: &ResourceLease,
    revision: u64,
    cards: &mut [viptv_engine::presentation::Card],
) {
    let Ok(addon_revision) = app.addons.revision() else {
        return;
    };
    merge_cached_revision(app, lease, revision, addon_revision, cards);
}

pub(crate) fn merge_cached_with_db(
    app: &App,
    lease: &ResourceLease,
    revision: u64,
    db: &Connection,
    cards: &mut [viptv_engine::presentation::Card],
) {
    let Ok(addon_revision) = app.addons.revision_db(db) else {
        return;
    };
    merge_cached_revision(app, lease, revision, addon_revision, cards);
}

fn merge_cached_revision(
    app: &App,
    lease: &ResourceLease,
    revision: u64,
    addon_revision: String,
    cards: &mut [viptv_engine::presentation::Card],
) {
    let Ok(cache) = app.verified_cards.lock() else {
        return;
    };
    for card in cards {
        let identity = card.parent_identity.as_ref().unwrap_or(&card.identity);
        if let Some(metadata) = cache.get(&(
            lease.principal.key(),
            revision,
            card.provider_id.clone(),
            identity.kind.clone(),
            identity.id.clone(),
            addon_revision.clone(),
        )) {
            *card = viptv_engine::presentation::enrich_card(card, metadata);
        }
    }
}
