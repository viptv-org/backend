use super::*;
use viptv_engine::{
    presentation::{Card, CatalogPage, DetailSnapshot},
    simkl as projection,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Search {
    pub(crate) category: String,
    pub(crate) query: String,
    #[serde(default = "first_page")]
    pub(crate) page: usize,
}
fn first_page() -> usize {
    1
}

pub(super) fn allowed(app: &App, mut cards: Vec<Card>) -> Result<Vec<Card>, ApiError> {
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_media(&db)?;
    for card in &mut cards {
        card.metadata_origin = Some(viptv_engine::origin::MetadataOrigin {
            provider_id: "simkl".into(),
            connection_id: None,
            incarnation: None,
        });
    }
    if !kids::restricted(&db, &app.identity())? {
        return Ok(cards);
    }
    // Metadata acquired by this new adapter is not an approved kids-policy observation.
    // The existing policy remains authoritative, including after an in-flight profile change.
    Ok(cards
        .into_iter()
        .filter(|card| {
            kids::require_item(&db, &app.identity(), &card.identity.kind, &card.identity.id).is_ok()
        })
        .collect())
}

pub(crate) async fn search(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    Query(q): Query<Search>,
) -> ApiResult {
    let app = app.with_lease(lease);
    let category =
        projection::category(&q.category).map_err(|_| ApiError::from("Invalid Simkl category"))?;
    if q.query.trim().is_empty() || q.query.chars().count() > 256 || !(1..=20).contains(&q.page) {
        return Err("Invalid Simkl search".into());
    }
    let credential = store::credential(&app, profile, Capability::Search)?;
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("q", q.query.trim())
        .append_pair("page", &q.page.to_string())
        .append_pair("limit", "24")
        .append_pair("extended", "full")
        .finish();
    let value = store::request(
        &app,
        profile,
        &credential,
        &format!("/search/{category}?{query}"),
        None,
    )
    .await?;
    let values = value
        .as_array()
        .ok_or_else(|| ApiError(StatusCode::BAD_GATEWAY, "Simkl search was invalid".into()))?;
    if values.len() > 24 {
        return Err("Simkl page exceeds the limit".into());
    }
    let more = values.len() == 24 && q.page < 20;
    let mut cards = allowed(
        &app,
        values
            .iter()
            .filter_map(|v| projection::card(v, category).ok())
            .collect(),
    )?;
    viptv_engine::search::rank(&mut cards, &q.query);
    Ok(axum::Json(
        serde_json::to_value(CatalogPage {
            cards,
            next_cursor: more.then(|| (q.page + 1).to_string()),
            complete: !more,
        })
        .map_err(|_| ApiError::from("Catalog unavailable"))?,
    ))
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Page {
    #[serde(default)]
    pub(crate) offset: usize,
    pub(crate) limit: Option<usize>,
}

pub(super) async fn title(
    app: &App,
    profile: i64,
    credential: &store::Credential,
    category: &str,
    id: u64,
) -> Result<Card, ApiError> {
    Ok(title_in(app, profile, credential, category, id).await?.0)
}

pub(super) async fn title_in(
    app: &App,
    profile: i64,
    credential: &store::Credential,
    category: &str,
    id: u64,
) -> Result<(Card, &'static str), ApiError> {
    let category = crate::simkl::key::category(category);
    projection::category(category.as_ref())
        .map_err(|_| ApiError::from("Invalid Simkl category"))?;
    let category = match category.as_ref() {
        "movie" => "movie",
        "tv" => "tv",
        _ => "anime",
    };
    if id == 0 {
        return Err("Invalid Simkl title".into());
    }
    let path = |category: &str| {
        format!(
            "/{}/{id}?extended=full",
            if category == "movie" {
                "movies"
            } else {
                category
            }
        )
    };
    let (value, served) = match cache::metadata(app, profile, credential, &path(category)).await {
        Ok(value) => (value, category),
        Err(primary) if category == "movie" => {
            match cache::metadata(app, profile, credential, &path("anime")).await {
                Ok(value) => (value, "anime"),
                Err(_) => return Err(primary),
            }
        }
        Err(other) => return Err(other),
    };
    if projection::id(&value) != Some(id) {
        return Err(ApiError(
            StatusCode::BAD_GATEWAY,
            "Simkl returned a different title".into(),
        ));
    }
    let card = projection::card(&value, served)
        .map_err(|_| ApiError(StatusCode::BAD_GATEWAY, "Simkl title was invalid".into()))?;
    if category == "movie" && card.identity.kind != "movie" {
        return Err(ApiError(
            StatusCode::BAD_GATEWAY,
            "Simkl title was invalid".into(),
        ));
    }
    store::validate(app, profile, credential)?;
    Ok((card, served))
}

pub(super) async fn episodes(
    app: &App,
    profile: i64,
    credential: &store::Credential,
    id: u64,
    parent: &Card,
) -> Result<Vec<Card>, ApiError> {
    let category = if parent.identity.id.starts_with("simkl:anime:") {
        "anime"
    } else {
        "tv"
    };
    let value = cache::metadata(
        app,
        profile,
        credential,
        &format!("/{category}/episodes/{id}"),
    )
    .await?;
    projection::episodes(&value, parent).map_err(|_| {
        ApiError(
            StatusCode::BAD_GATEWAY,
            "Simkl episode identity was invalid".into(),
        )
    })
}

pub(crate) async fn detail(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path((profile, category, id)): Path<(i64, String, u64)>,
    Query(q): Query<Page>,
) -> ApiResult {
    if q.offset > 10_000 || q.limit.is_some_and(|limit| !(1..=128).contains(&limit)) {
        return Err("Invalid episode offset".into());
    }
    let app = app.with_lease(lease);
    let credential = store::credential(&app, profile, Capability::Metadata)?;
    let category = crate::simkl::key::category(&category).into_owned();
    let (item, served) = title_in(&app, profile, &credential, &category, id).await?;
    let mut item = allowed(&app, vec![item])?
        .into_iter()
        .next()
        .ok_or_else(auth::forbidden)?;
    let all = if served == "tv" || served == "anime" && item.identity.kind == "series" {
        episodes(&app, profile, &credential, id, &item).await?
    } else {
        vec![]
    };
    let endpoint = if served == "movie" { "movies" } else { served };
    let document = cache::metadata(
        &app,
        profile,
        &credential,
        &format!("/{endpoint}/{id}?extended=full"),
    )
    .await?;
    let recommendations = allowed(
        &app,
        projection::recommendations(&document, served, &item.identity.id),
    )?;
    let all = viptv_engine::presentation::canonical_episodes(allowed(&app, all)?);
    let seasons = viptv_engine::presentation::episode_seasons(&all);
    let limit = q.limit.unwrap_or(24);
    let next_cursor = (all.len() > q.offset + limit).then(|| (q.offset + limit).to_string());
    let episodes = allowed(&app, all.into_iter().skip(q.offset).take(limit).collect())?;
    let _ = tokio::time::timeout(
        Duration::from_secs(2),
        super::artwork::enrich(&app, &mut item),
    )
    .await;
    store::validate(&app, profile, &credential)?;
    Ok(axum::Json(
        serde_json::to_value(DetailSnapshot {
            item,
            recommendations,
            episodes,
            next_cursor,
            seasons,
        })
        .map_err(|_| ApiError::from("Title unavailable"))?,
    ))
}

pub(crate) async fn home_metadata(
    app: &App,
    profile: i64,
    original: Card,
) -> Result<Card, ApiError> {
    let identity = original
        .parent_identity
        .as_ref()
        .unwrap_or(&original.identity);
    let mapped = if identity.id.starts_with("simkl:") {
        None
    } else {
        imports::remote_identity(app, profile, &identity.kind, &identity.id)?
    };
    let lookup = mapped.as_deref().unwrap_or(&identity.id);
    let parts = lookup.split(':').collect::<Vec<_>>();
    let ["simkl", category, id] = parts.as_slice() else {
        return Ok(original);
    };
    let id = id
        .parse()
        .map_err(|_| ApiError::from("Invalid Simkl identity"))?;
    let access = store::credential(app, profile, Capability::Metadata)?;
    let detail = title(app, profile, &access, category, id).await?;
    let mut detail = allowed(app, vec![detail])?
        .into_iter()
        .next()
        .ok_or_else(auth::forbidden)?;
    if mapped.is_some() {
        if !detail
            .identity
            .ids
            .iter()
            .any(|external| external.namespace == "imdb" && external.value == identity.id)
        {
            return Err(ApiError::from("Verified title mapping changed"));
        }
        detail.identity.id = identity.id.clone();
    }
    Ok(detail)
}
