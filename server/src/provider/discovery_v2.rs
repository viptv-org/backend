//! V2 discovery uses account-owned inputs while retaining incremental source jobs.
use crate::account_api::Error;
use crate::*;
use axum::extract::rejection::{JsonRejection, QueryRejection};

fn boundary(error: ApiError) -> Error {
    if error.0 == StatusCode::UNAUTHORIZED
        || error.0 == StatusCode::FORBIDDEN
        || error.api_error_code().is_some()
    {
        return Error::Auth(error);
    }
    if let Some(code) = service_errors::provider(&error.1) {
        return Error::Code(code);
    }
    Error::Code(match error.0 {
        StatusCode::NOT_FOUND => "discovery_not_found",
        StatusCode::TOO_MANY_REQUESTS => "discovery_capacity",
        StatusCode::BAD_REQUEST => "invalid_discovery_request",
        _ => "provider_storage_unavailable",
    })
}

async fn scoped(app: App, lease: ResourceLease) -> Result<App, ApiError> {
    blocking(move || {
        let mut app = app.with_lease(lease);
        {
            let db = app.db.lock().unwrap();
            app.require_media(&db)?;
        }
        app.providers = app
            .providers
            .for_account(app.identity().account_id().ok_or_else(auth::unauthorized)?);
        app.addons = app.addons.with_protected_fetch();
        Ok(app)
    })
    .await
}

pub(crate) async fn start(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    value: Result<axum::Json<Value>, JsonRejection>,
) -> Result<axum::Json<Value>, Error> {
    let value = value.map_err(|_| Error::Code("invalid_discovery_request"))?;
    let app = scoped(app, lease).await?;
    start_streams(State(app), value).await.map_err(boundary)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PollQuery {
    #[serde(default)]
    after: usize,
}

pub(crate) async fn poll(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    path: Path<String>,
    query: Result<Query<PollQuery>, QueryRejection>,
) -> Result<axum::Json<Value>, Error> {
    let Query(query) = query.map_err(|_| Error::Code("invalid_discovery_cursor"))?;
    let app = scoped(app, lease).await?;
    blocking(move || {
    app.check_resource(&app.identity(), "job", &path.0)?;
    let job = job(&app,&path.0)?;
    // Preserve event sequence positions when an ownership grant disappears.
    // No stale source metadata or opaque playback handles survive this check.
    let db = app.db.lock().unwrap();
    app.require_media(&db)?;
    let state = job.state.lock().unwrap();
    let mut response = axum::Json(json!({"events":state.events.iter().skip(query.after).cloned().collect::<Vec<_>>(),"done":state.pending==0}));
    if let Some(events) = response.0["events"].as_array_mut() {
        for event in events {
            if let Some(id)=event["source"].as_str().and_then(|s|s.strip_prefix("addon:")).and_then(|id|id.parse::<i64>().ok()) {
                if !addon::Addons::available(&db,app.identity().account_id().unwrap_or(0),id) {
                    *event=json!({"seq":event["seq"],"source":"addon","streams":[],"error":account_api::description("source_not_found"),"error_code":"source_not_found"});
                }
            }
            if let Some(id) = event["source"]
                .as_str()
                .and_then(|s| s.strip_prefix("iptv:"))
                .and_then(|id| id.parse::<i64>().ok())
            {
                if app.providers.require_owner(&db, id).is_err() {
                    *event = json!({"seq":event["seq"],"source":"iptv","streams":[],"error":"This IPTV source is no longer available in your account.","error_code":"source_not_found"});
                }
            }
        }
    }
    Ok(response)
    }).await.map_err(boundary)
}

pub(crate) async fn guide(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
) -> Result<axum::Json<Value>, crate::account_api::Error> {
    let app = scoped(app, lease).await?;
    {
        let db = app.db.lock().unwrap();
        kids::require_parent(&db, &app.identity())?;
    }
    if id.len() > 256 {
        return Err("source_not_found".into());
    }
    let lookup = id.clone();
    app.providers
        .blocking(move |s| s.channel(&lookup).map(|_| ()))
        .await
        .map_err(|error| {
            crate::account_api::Error::Code(
                service_errors::provider(&error).unwrap_or("source_not_found"),
            )
        })?;
    let value = app.providers.guide(id).await.map_err(|error| {
        crate::account_api::Error::Code(
            service_errors::provider(&error).unwrap_or("source_unavailable"),
        )
    })?;
    blocking(move || app.require_media(&app.db.lock().unwrap())).await?;
    Ok(axum::Json(value))
}

/// Guide playback selects exactly this owned raw channel, never an addon result.
pub(crate) async fn live_source(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
) -> Result<axum::Json<Value>, Error> {
    if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
        return Err(Error::Code("source_not_found"));
    }
    let app = app.with_lease(lease);
    tokio::task::spawn_blocking(move || {
        app.prune();
        let (producer, configuration, raw) = {
            let db = app
                .db
                .lock()
                .map_err(|_| Error::Code("provider_storage_unavailable"))?;
            app.require_media(&db)?;
            kids::require_parent(&db, &app.identity())?;
            let account = app.identity().account_id().ok_or_else(auth::unauthorized)?;
            let (provider_id, stream, name): (i64, String, String) = db
                .query_row(
                    "SELECT l.provider_id,l.stream_id,l.name FROM provider_live l
                     JOIN providers p ON p.id=l.provider_id
                     JOIN provider_ownership o ON o.provider_id=p.id
                     WHERE l.id=?1 AND o.account_id=?2 AND p.enabled=1 AND p.enable_live=1",
                    params![id, account],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .map_err(|_| Error::Code("provider_storage_unavailable"))?
                .ok_or(Error::Code("source_not_found"))?;
            let provider =
                super::service::provider_row(&db, provider_id, app.providers.vault.as_deref())
                    .map_err(|error| {
                        Error::Code(
                            service_errors::provider(&error).unwrap_or("source_unavailable"),
                        )
                    })?;
            let producer = format!("iptv:{provider_id}");
            let configuration = crate::sources::source_configuration(&db, &producer)?
                .ok_or(Error::Code("source_not_found"))?;
            let routing = super::egress::headers(&db, provider_id)
                .map_err(|_| Error::Code("source_route_migration_required"))?;
            if !routing.is_empty() {
                return Err(Error::Code("source_route_migration_required"));
            }
            let url = super::media_url(
                &provider.url,
                &provider.username,
                &provider.password,
                "live",
                &stream,
                "ts",
            )
            .map_err(|_| Error::Code("source_format_unsupported"))?;
            (
                producer,
                configuration,
                json!({"url":url,"name":provider.name,"title":name}),
            )
        };
        let (mut cards, error) =
            app.register_with_configuration(&producer, vec![raw], "live", Some(configuration));
        if let Some(error) = error {
            return Err(Error::Code(if error == "source_configuration_changed" {
                "source_configuration_changed"
            } else {
                "source_unavailable"
            }));
        }
        {
            let db = app
                .db
                .lock()
                .map_err(|_| Error::Code("provider_storage_unavailable"))?;
            app.require_media(&db)?;
            kids::require_parent(&db, &app.identity())?;
            if crate::sources::source_configuration(&db, &producer)? != Some(configuration) {
                return Err(Error::Code("source_configuration_changed"));
            }
        }
        let source = cards.pop().ok_or(Error::Code("discovery_capacity"))?;
        Ok(axum::Json(json!({"source":source})))
    })
    .await
    .map_err(|_| Error::Code("provider_storage_unavailable"))?
}
