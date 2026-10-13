use super::*;
use viptv_engine::presentation::{CatalogPage, DetailSnapshot};

pub(crate) async fn discover(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Query(q): Query<crate::handlers_catalog::Discover>,
) -> ApiResult {
    let capability = if q.is_search() {
        viptv_engine::capabilities::Capability::Search
    } else {
        viptv_engine::capabilities::Capability::Discovery
    };
    let auth::Principal::Account { profile_id, .. } = &lease.principal;
    let profile = profile_id.ok_or_else(auth::forbidden)?;
    let scoped = app.with_lease(lease.clone());
    let (configuration, _) = configuration::read(&scoped, &lease, profile)?;
    if configuration.browsing_plugin == "simkl" {
        let result = super::simkl_catalog::discover(scoped.clone(), lease, profile, q).await?;
        configuration::validate_revision(&scoped, profile, configuration.scope.revision)?;
        return Ok(result);
    }
    let (scoped, revision) = configuration::apply(scoped, &lease, profile, capability)?;
    let axum::Json(value) =
        crate::handlers_catalog::discover(State(scoped.clone()), Extension(lease), Query(q))
            .await?;
    configuration::validate_revision(&scoped, profile, revision)?;
    let cards = projection::cards(
        &scoped,
        value["metas"].as_array().cloned().unwrap_or_default(),
        "stremio",
    )?;
    let page = CatalogPage {
        cards,
        next_cursor: value["next_skip"].as_u64().map(|n| n.to_string()),
        complete: value["has_more"].as_bool() == Some(false),
    };
    Ok(axum::Json(
        serde_json::to_value(page).map_err(|_| ApiError::from("Catalog unavailable"))?,
    ))
}

#[derive(Default, Deserialize)]
pub(crate) struct DetailPage {
    #[serde(default)]
    offset: usize,
}

pub(crate) async fn detail(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path((kind, id)): Path<(String, String)>,
    Query(page): Query<DetailPage>,
) -> ApiResult {
    if id.is_empty()
        || id.len() > 512
        || !["movie", "series"].contains(&kind.as_str())
        || page.offset > 100_000
    {
        return Err("Invalid metadata request".into());
    }
    let auth::Principal::Account { profile_id, .. } = &lease.principal;
    let profile = profile_id.ok_or_else(auth::forbidden)?;
    let app = app.with_lease(lease.clone());
    let (configuration, _) = configuration::read(&app, &lease, profile)?;
    if configuration.browsing_plugin == "simkl" {
        let result =
            super::simkl_catalog::detail(app.clone(), lease, profile, kind, id, page.offset)
                .await?;
        configuration::validate_revision(&app, profile, configuration.scope.revision)?;
        return Ok(result);
    }
    let (app, revision) = configuration::apply(
        app,
        &lease,
        profile,
        viptv_engine::capabilities::Capability::Metadata,
    )?;
    {
        let db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        app.require_media(&db)?;
        kids::require_item(&db, &app.identity(), &kind, &id)?;
    }
    let value = app.addons.meta(&kind, &id).await?;
    configuration::validate_revision(&app, profile, revision)?;
    let meta = if value["meta"].is_object() {
        value["meta"].clone()
    } else {
        value
    };
    if meta["id"].as_str() != Some(id.as_str()) {
        return Err(ApiError(
            StatusCode::BAD_GATEWAY,
            "Provider returned a different title".into(),
        ));
    }
    let mut parent = meta.clone();
    parent["type"] = json!(kind);
    parent["id"] = json!(id);
    let item = projection::cards(&app, vec![parent.clone()], "stremio")?
        .into_iter()
        .next()
        .ok_or_else(auth::forbidden)?;
    let all = meta["videos"].as_array().cloned().unwrap_or_default();
    let next_cursor = (all.len() > page.offset + 24).then(|| (page.offset + 24).to_string());
    let videos = all
        .iter()
        .skip(page.offset)
        .take(24)
        .map(|video| {
            let mut episode = video.clone();
            episode["type"] = parent["type"].clone();
            if episode["name"].is_null() {
                episode["name"] = episode["title"].clone();
            }
            if episode["background"].is_null() {
                episode["background"] = episode["thumbnail"].clone();
            }
            episode
        })
        .collect();
    let mut episodes = projection::cards(&app, videos, "stremio")?;
    for episode in &mut episodes { episode.parent_identity = Some(item.identity.clone()); }
    Ok(axum::Json(
        serde_json::to_value(DetailSnapshot {
            item,
            episodes,
            next_cursor,
        })
        .map_err(|_| ApiError::from("Title unavailable"))?,
    ))
}
