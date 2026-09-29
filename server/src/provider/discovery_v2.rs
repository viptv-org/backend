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
            kids::require_parent(&db, &app.identity())?;
        }
        app.providers = app
            .providers
            .for_account(app.identity().account_id().ok_or_else(auth::unauthorized)?);
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
