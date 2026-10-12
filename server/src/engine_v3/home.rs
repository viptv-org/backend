use super::*;
use viptv_engine::{
    home::{assemble, RowRequest},
    presentation::HomeSnapshot,
};

pub(super) async fn load(
    app: App,
    lease: ResourceLease,
    profile: i64,
) -> Result<HomeSnapshot, ApiError> {
    let app = app.with_lease(lease.clone());
    let (app, configuration_revision) = configuration::apply(
        app,
        &lease,
        profile,
        viptv_engine::capabilities::Capability::Discovery,
    )?;
    let (queue, favorites, revision) = {
        let db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        app.require_profile(&db, profile)?;
        let queue = continuation::queue(&db, profile, 0, 12)?;
        let allowed = kids::sql_allowed("favorites.type", "favorites.id", "?1");
        let mut q=db.prepare(&format!("SELECT id,type,name,poster FROM favorites WHERE profile_id=?1 AND {allowed} ORDER BY rowid DESC LIMIT 12")).map_err(db_error)?;
        let favorites=q.query_map([profile],|r|Ok(json!({"id":r.get::<_,String>(0)?,"type":r.get::<_,String>(1)?,"name":r.get::<_,String>(2)?,"poster":r.get::<_,Option<String>>(3)?}))).map_err(db_error)?.collect::<Result<Vec<_>,_>>().map_err(db_error)?;
        (queue, favorites, configuration_revision)
    };
    let catalogs = app.addons.catalogs()?;
    let queue = projection::cards(
        &app,
        queue["items"].as_array().cloned().unwrap_or_default(),
        "library",
    )?;
    let favorites = projection::cards(&app, favorites, "library")?;
    let mut requests = vec![
        RowRequest {
            id: "continue".into(),
            title: "Continue Watching".into(),
            provider_id: "library".into(),
            load: Box::pin(async move { Ok(queue) }),
        },
        RowRequest {
            id: "favorites".into(),
            title: "My List".into(),
            provider_id: "library".into(),
            load: Box::pin(async move { Ok(favorites) }),
        },
    ];
    for c in catalogs.as_array().into_iter().flatten().take(6) {
        let Some(catalog) = c["id"].as_str().map(str::to_owned) else {
            continue;
        };
        let Some(kind) = c["type"].as_str().map(str::to_owned) else {
            continue;
        };
        let Some(addon) = c["addon_id"].as_i64() else {
            continue;
        };
        let provider = format!("addon:{addon}");
        let task_app = app.clone();
        requests.push(RowRequest {
            id: format!("{provider}:{kind}:{catalog}"),
            title: c["name"].as_str().unwrap_or(&catalog).to_owned(),
            provider_id: provider.clone(),
            load: Box::pin(async move {
                let result = task_app
                    .addons
                    .discover(kind, Some(catalog), Some(addon), 0, None, None)
                    .await?;
                projection::cards(
                    &task_app,
                    result["metas"].as_array().cloned().unwrap_or_default(),
                    &provider,
                )
                .map_err(|_| "Row unavailable".into())
            }),
        });
    }
    let snapshot = assemble(
        scope(&lease, profile, revision),
        requests,
        Duration::from_secs(2),
    )
    .await;
    configuration::validate_revision(&app, profile, configuration_revision)?;
    lease.validate(
        &*app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?,
    )?;
    Ok(snapshot)
}
