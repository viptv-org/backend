use super::*;
use viptv_engine::{
    capabilities::{Availability, Capability},
    live::*,
    profile::IntegrationConnection,
};
mod configuration;
pub(crate) mod favorites;
mod fetch_gates;
mod snapshot;
#[cfg(feature = "live-runtime")]
mod xtream;
#[cfg(feature = "live-runtime")]
mod xtream_configuration;
pub(crate) use configuration::{available_m3u, configure, connections, remove};
pub(crate) use fetch_gates::FetchGates;
pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    configuration::init(db)?;
    favorites::init(db)
}
#[cfg(feature = "live-runtime")]
mod native;
#[derive(Clone)]
pub(crate) struct Cache {
    pub revision: i64,
    pub expires: Instant,
    pub channels: Vec<PrivateChannel>,
    pub groups: Vec<LiveGroup>,
    pub guides: HashMap<String, (Instant, LiveGuide)>,
    pub xmltv: Option<(Instant, Arc<str>)>,
}
fn scope(
    app: &App,
    lease: &ResourceLease,
    profile: i64,
) -> Result<
    (
        viptv_engine::profile::ProfileConfiguration,
        Vec<IntegrationConnection>,
    ),
    ApiError,
> {
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_profile(&db, profile)?;
    kids::require_parent(&db, &lease.principal)?;
    drop(db);
    let (config, connections) = super::configuration::read(app, lease, profile)?;
    let allowed = viptv_engine::registry::Registry::builtin()
        .connections_for(&config, &connections, Capability::LiveTv)
        .map_err(|_| ApiError::from("Live connections unavailable"))?
        .into_iter()
        .cloned()
        .collect();
    Ok((config, allowed))
}
pub(crate) async fn channels(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    Query(query): Query<LiveQuery>,
) -> ApiResult {
    query
        .validate()
        .map_err(|_| ApiError::from("Invalid live query"))?;
    let app = app.with_lease(lease.clone());
    let (config, connections) = scope(&app, &lease, profile)?;
    let Some(connection) = connections
        .iter()
        .find(|c| query.connection_id.as_ref().is_none_or(|id| id == &c.id))
    else {
        return snapshot::project(&app, profile, config.scope.revision, &query, "empty", 0, LivePage {
            channels: vec![],
            groups: vec![],
            connections,
            next_cursor: None,
            complete: true,
            availability: Availability::NeedsConfiguration,
            reason: Some("Assign a live TV connection".into())
        });
    };
    if connection.plugin_id == "m3u" || connection.id.starts_with("xtream:") {
        #[cfg(feature = "live-runtime")]
        return bounded(native::page(
            app,
            lease,
            profile,
            config.scope.revision,
            connection.clone(),
            connections,
            query,
        ))
        .await;
        #[cfg(not(feature = "live-runtime"))]
        return Err("Live runtime unavailable".into());
    }
    let id = connection
        .id
        .strip_prefix("iptv:")
        .and_then(|id| id.parse::<i64>().ok())
        .ok_or("Invalid live connection")?;
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    let mut group_query=db.prepare("SELECT id,name FROM provider_live_categories_v2 WHERE provider_id=?1 ORDER BY ordinal LIMIT 1000").map_err(db_error)?;
    let groups = group_query
        .query_map([id], |r| {
            Ok(LiveGroup {
                id: r.get(0)?,
                label: r.get(1)?,
            })
        })
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    let total:u64=db.query_row("SELECT count(*) FROM provider_live WHERE provider_id=?1 AND (?2 IS NULL OR category_id=?2) AND (?3 IS NULL OR instr(lower(name),lower(?3))>0)",params![id,query.group_id,query.search],|r|r.get(0)).map_err(db_error)?;
    let limit = query.limit.unwrap_or(100);
    let mut channel_query=db.prepare("SELECT id,name,logo,category_id,category FROM provider_live WHERE provider_id=?1 AND (?2 IS NULL OR category_id=?2) AND (?3 IS NULL OR instr(lower(name),lower(?3))>0) ORDER BY ordinal,id LIMIT ?4 OFFSET ?5").map_err(db_error)?;
    let channels = channel_query
        .query_map(
            params![id, query.group_id, query.search, limit, query.offset],
            |r| {
                let channel_id: String = r.get(0)?;
                let name: String = r.get(1)?;
                Ok(LiveChannel {
                    id: channel_id.clone(),
                    connection_id: connection.id.clone(),
                    card: live_card(
                        channel_id,
                        name,
                        artwork(r.get::<_, Option<String>>(2)?.as_deref()),
                        connection.id.clone(),
                    ),
                    group_id: r.get(3)?,
                    group_name: r.get(4)?,
                    group_ids: r.get::<_, Option<String>>(3)?.into_iter().collect(),
                })
            },
        )
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    let next = u64::from(query.offset) + u64::from(limit);
    let catalog_revision = snapshot::catalog_revision(&db, id)?;
    snapshot::project(&app, profile, config.scope.revision, &query, &catalog_revision, total, LivePage {
        channels,
        groups,
        connections,
        next_cursor: (next < total).then(|| next.to_string()),
        complete: next >= total,
        availability: Availability::Available,
        reason: None
    })
}
async fn connection_for(
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    channel: &str,
) -> Result<(u64, IntegrationConnection), ApiError> {
    let (config, connections) = scope(app, lease, profile)?;
    {
        let db = app.db.lock().unwrap();
        let provider: Option<i64> = db
            .query_row(
                "SELECT provider_id FROM provider_live WHERE id=?1",
                [channel],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?;
        if let Some(provider) = provider {
            if let Some(connection) = connections
                .iter()
                .find(|c| c.id == format!("iptv:{provider}"))
            {
                return Ok((config.scope.revision, connection.clone()));
            }
        }
    }
    let cache = app.live_cache.lock().await;
    let connection = connections
        .iter()
        .find(|c| {
            cache
                .get(&c.id)
                .is_some_and(|cache| cache.channels.iter().any(|c| c.channel.id == channel))
        })
        .cloned()
        .ok_or_else(auth::forbidden)?;
    Ok((config.scope.revision, connection))
}
pub(crate) async fn guide(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path((profile, channel)): Path<(i64, String)>,
) -> ApiResult {
    let mut response=guide_inner(State(app),Extension(lease),Path((profile,channel))).await?;
    let now=util::now_millis().max(0) as u64;
    let current=response.0["programs"].as_array().and_then(|programs|programs.iter().find(|program|program["start_millis"].as_u64().is_some_and(|start|start<=now)&&program["end_millis"].as_u64().is_some_and(|end|end>now))).map(|program|program["id"].clone());
    let start=now/1_800_000*1_800_000;
    response.0["view"]=json!({"current_program_id":current,"now_millis":now,"window_start_millis":start,"window_end_millis":start+7_200_000});
    Ok(response)
}
async fn guide_inner(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path((profile, channel)): Path<(i64, String)>,
) -> ApiResult {
    let app = app.with_lease(lease.clone());
    let (revision, connection) = connection_for(&app, &lease, profile, &channel).await?;
    if connection.plugin_id == "m3u" || connection.id.starts_with("xtream:") {
        #[cfg(feature = "live-runtime")]
        return bounded(native::guide(
            app, lease, profile, revision, connection, channel,
        ))
        .await;
        #[cfg(not(feature = "live-runtime"))]
        return Err("Live runtime unavailable".into());
    }
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        app.providers.guide(channel.clone()),
    )
    .await;
    super::configuration::validate_revision(&app, profile, revision)?;
    let guide = match result {
        Ok(Ok(value)) => {
            let programs: Vec<LiveProgram> = value["programs"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| {
                    Some(LiveProgram {
                        id: p["id"].as_str()?.into(),
                        title: p["title"].as_str()?.into(),
                        description: p["description"].as_str().map(str::to_owned),
                        start_millis: p["start"].as_u64()?.checked_mul(1000)?,
                        end_millis: p["end"].as_u64()?.checked_mul(1000)?,
                    })
                })
                .collect();
            let empty = programs.is_empty();
            LiveGuide {
                channel_id: channel.clone(),
                programs,
                availability: if empty {
                    Availability::Unsupported
                } else {
                    Availability::Available
                },
                reason: empty.then(|| "No programme guide for this channel".into()),
            }
        }
        _ => LiveGuide {
            channel_id: channel,
            programs: vec![],
            availability: Availability::TemporarilyUnavailable,
            reason: Some("Guide temporarily unavailable".into()),
        },
    };
    Ok(axum::Json(json!(guide)))
}
#[cfg(feature = "live-runtime")]
pub(super) fn honest_guide(guide: &mut LiveGuide) {
    if guide.availability == Availability::Available && guide.programs.is_empty() {
        guide.availability = Availability::Unsupported;
        guide.reason = Some("No programme guide for this channel".into());
    }
}
pub(crate) async fn sources(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path((profile, channel)): Path<(i64, String)>,
) -> ApiResult {
    let app = app.with_lease(lease.clone());
    let (revision, connection) = connection_for(&app, &lease, profile, &channel).await?;
    if connection.plugin_id == "m3u" || connection.id.starts_with("xtream:") {
        #[cfg(feature = "live-runtime")]
        return bounded(native::sources(
            app, lease, profile, revision, connection, channel,
        ))
        .await;
        #[cfg(not(feature = "live-runtime"))]
        return Err("Live runtime unavailable".into());
    }
    let axum::Json(value) = crate::provider::discovery_v2::live_source(
        State(app.clone()),
        Extension(lease.clone()),
        Path(channel.clone()),
    )
    .await
    .map_err(|_| ApiError::from("Live source unavailable"))?;
    super::configuration::validate_revision(&app, profile, revision)?;
    if let Some(id) = value["source"]["id"].as_str() {
        if let Some(entry) = app.streams.lock().unwrap().get_mut(id) {
            entry.profile_revision = Some((profile, revision));
        }
    }
    remember_sources(
        &app,
        profile,
        revision,
        &channel,
        &connection.id,
        &[value["source"].clone()],
    );
    let card = {
        let db = app.db.lock().map_err(|_| ApiError::from("Database unavailable"))?;
        db.query_row("SELECT name FROM provider_live WHERE id=?1", [&channel], |row| {
            Ok(viptv_engine::live::live_card(channel.clone(), row.get(0)?, None, connection.id.clone()))
        }).map_err(db_error)?
    };
    let action = viewing_action(&app, &lease, profile, &card)?;
    Ok(axum::Json(
        json!({"streams":[value["source"].clone()],"live":true,"action":action}),
    ))
}

pub(super) fn viewing_action(
    app: &App, lease: &ResourceLease, profile: i64, card: &viptv_engine::presentation::Card,
) -> Result<Value, ApiError> {
    let db = app.db.lock().map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_profile(&db, profile)?;
    lease.validate(&db)?;
    kids::require_parent(&db, &lease.principal)?;
    let preferences = super::preferences::read(&db, lease.principal.account_id().ok_or_else(auth::forbidden)?, profile)?;
    let target = viptv_engine::live::live_card(card.identity.id.clone(), card.title.clone(), None, card.provider_id.clone());
    Ok(json!({"card":target,"label":"Watch live","resume":false,"source_selection":preferences.preferences.source_selection}))
}

pub(super) fn remember_sources(
    app: &App,
    profile: i64,
    revision: u64,
    channel: &str,
    connection: &str,
    cards: &[Value],
) {
    let job = Arc::new(Job {metadata_proof:None,
        source_slots: std::sync::atomic::AtomicUsize::new(cards.len()),
        cancelled: std::sync::atomic::AtomicBool::new(false),
        configuration_revision: Some((profile, revision)),
        media_id: Some(channel.into()),
        exact_vod: None,
        kind: "live".into(),
        created: Instant::now(),
        state: Mutex::new(JobState {
            events: vec![json!({"seq":1,"source":connection,"streams":cards})],
            pending: 0,
        }),
        notify: Notify::new(),
    });
    app.prune();
    let mut jobs = app.jobs.lock().unwrap();
    if jobs.len() >= 256 {
        if let Some(oldest) = jobs
            .iter()
            .filter(|(_, job)| job.state.lock().unwrap().pending == 0)
            .min_by_key(|(_, job)| job.created)
            .map(|(id, _)| id.clone())
        {
            jobs.remove(&oldest);
        } else {
            return;
        }
    }
    jobs.insert(Uuid::new_v4().to_string(), job);
}

#[cfg(feature = "live-runtime")]
async fn bounded(future: impl std::future::Future<Output = ApiResult>) -> ApiResult {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .map_err(|_| {
            ApiError(
                StatusCode::GATEWAY_TIMEOUT,
                "Live data temporarily unavailable".into(),
            )
        })?
}

#[cfg(all(test, feature = "live-runtime"))]
#[path = "live/xtream_profile_tests.rs"]
mod xtream_profile_tests;

pub(crate) fn available_xtream(db: &Connection, lease: &ResourceLease, id: i64) -> bool {
    #[cfg(feature = "live-runtime")]
    return xtream_configuration::available(db, lease, id);
    #[cfg(not(feature = "live-runtime"))]
    {
        let _ = (db, lease, id);
        false
    }
}

#[cfg(all(test, feature = "live-runtime"))]
#[path = "live/xtream_projection_tests.rs"]
mod xtream_projection_tests;

#[cfg(all(test, feature = "live-runtime"))]
#[path = "live/xtream_authority_tests.rs"]
mod xtream_authority_tests;
