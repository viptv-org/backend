//! V2 discovery uses account-owned inputs while retaining incremental source jobs.
use crate::*;

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
    value: axum::Json<Value>,
) -> ApiResult {
    let app = scoped(app, lease).await?;
    start_streams(State(app), value).await
}

pub(crate) async fn poll(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    path: Path<String>,
    query: Query<Cursor>,
) -> ApiResult {
    let app = scoped(app, lease).await?;
    blocking(move || {
    app.check_resource(&app.identity(), "job", &path.0)?;
    let job = job(&app,&path.0)?;
    // Preserve event sequence positions when an ownership grant disappears.
    // No stale source metadata or opaque playback handles survive this check.
    let db = app.db.lock().unwrap();
    app.require_media(&db)?;
    let state = job.state.lock().unwrap();
    let mut response = axum::Json(json!({"events":state.events.iter().skip(query.0.after).cloned().collect::<Vec<_>>(),"done":state.pending==0}));
    if let Some(events) = response.0["events"].as_array_mut() {
        for event in events {
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
    }).await
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
        .map_err(|_| crate::account_api::Error::Code("source_not_found"))?;
    let value = app
        .providers
        .guide(id)
        .await
        .map_err(|_| crate::account_api::Error::Code("source_unavailable"))?;
    blocking(move || app.require_media(&app.db.lock().unwrap())).await?;
    Ok(axum::Json(value))
}
