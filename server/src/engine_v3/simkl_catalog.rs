//! Route the selected browsing integration without mixing metadata providers.
use super::*;

pub(super) async fn discover(
    app: App,
    lease: ResourceLease,
    profile: i64,
    q: crate::handlers_catalog::Discover,
) -> ApiResult {
    let category = match q.kind.as_str() {
        "movie" => "movie",
        "series" => "tv",
        "anime" => "anime",
        _ => return Err("Unsupported Simkl media type".into()),
    };
    if q.addon_id.is_some()
        || q.catalog
            .as_deref()
            .is_some_and(|c| c != "simkl-trending" && c != "trending")
    {
        return Err("Unsupported Simkl catalog options".into());
    }
    if q.genre.is_some() || q.extras.as_deref().is_some_and(|s|s!="{}") {
        if q.is_search() {return Err("Search cannot be combined with Simkl browse filters".into());}
        let page=crate::simkl::browse::page(&app,profile,category,q.skip,q.genre.as_deref(),q.extras.as_deref()).await?;
        return Ok(axum::Json(serde_json::to_value(page).map_err(|_|ApiError::from("Catalog unavailable"))?));
    }
    if let Some(search) = q.search.filter(|s| !s.trim().is_empty()) {
        if q.skip % 24 != 0 {
            return Err("Invalid Simkl search offset".into());
        }
        let axum::Json(mut value) = crate::simkl::search(
            State(app),
            Extension(lease),
            Path(profile),
            Query(crate::simkl::Search {
                category: category.into(),
                query: search,
                page: q.skip / 24 + 1,
            }),
        )
        .await?;
        value["next_cursor"] = if value["next_cursor"].is_string() {
            json!((q.skip + 24).to_string())
        } else {
            Value::Null
        };
        return Ok(axum::Json(value));
    }
    let page = crate::simkl::page(&app, profile, category, q.skip).await?;
    Ok(axum::Json(
        serde_json::to_value(page).map_err(|_| ApiError::from("Catalog unavailable"))?,
    ))
}

pub(super) async fn detail(
    app: App,
    lease: ResourceLease,
    profile: i64,
    kind: String,
    id: String,
    offset: usize,
) -> ApiResult {
    let parts: Vec<_> = id.split(':').collect();
    let ["simkl", category, title] = parts.as_slice() else {
        return Err("A Simkl title identity is required".into());
    };
    let title = title
        .parse::<u64>()
        .map_err(|_| ApiError::from("Invalid Simkl title identity"))?;
    let axum::Json(value) = crate::simkl::detail(
        State(app),
        Extension(lease),
        Path((profile, category.to_string(), title)),
        Query(crate::simkl::Page { offset }),
    )
    .await?;
    if value["item"]["identity"]["kind"].as_str() != Some(kind.as_str()) {
        return Err("Simkl title type does not match".into());
    }
    Ok(axum::Json(value))
}
