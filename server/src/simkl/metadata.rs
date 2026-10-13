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

pub(super) fn allowed(app: &App, cards: Vec<Card>) -> Result<Vec<Card>, ApiError> {
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_media(&db)?;
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
    let cards = allowed(
        &app,
        values
            .iter()
            .filter_map(|v| projection::card(v, category).ok())
            .collect(),
    )?;
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
}

pub(super) async fn title(
    app: &App,
    profile: i64,
    credential: &store::Credential,
    category: &str,
    id: u64,
) -> Result<Card, ApiError> {
    projection::category(category).map_err(|_| ApiError::from("Invalid Simkl category"))?;
    if id == 0 {
        return Err("Invalid Simkl title".into());
    }
    let endpoint = if category == "movie" {
        "movies"
    } else {
        category
    };
    let value = cache::metadata(
        app,
        profile,
        credential,
        &format!("/{endpoint}/{id}?extended=full"),
    )
    .await?;
    if projection::id(&value) != Some(id) {
        return Err(ApiError(
            StatusCode::BAD_GATEWAY,
            "Simkl returned a different title".into(),
        ));
    }
    projection::card(&value, category)
        .map_err(|_| ApiError(StatusCode::BAD_GATEWAY, "Simkl title was invalid".into()))
}

pub(super) async fn episodes(
    app: &App,
    profile: i64,
    credential: &store::Credential,
    id: u64,
    parent: &Card,
) -> Result<Vec<Card>, ApiError> {
    let value = cache::metadata(app, profile, credential, &format!("/tv/episodes/{id}")).await?;
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
    if q.offset > 10_000 {
        return Err("Invalid episode offset".into());
    }
    let app = app.with_lease(lease);
    let credential = store::credential(&app, profile, Capability::Metadata)?;
    let item = title(&app, profile, &credential, &category, id).await?;
    let item = allowed(&app, vec![item])?
        .into_iter()
        .next()
        .ok_or_else(auth::forbidden)?;
    let all = if category == "tv" {
        episodes(&app, profile, &credential, id, &item).await?
    } else {
        vec![]
    };
    let next_cursor = (all.len() > q.offset + 24).then(|| (q.offset + 24).to_string());
    let episodes = allowed(&app, all.into_iter().skip(q.offset).take(24).collect())?;
    store::validate(&app, profile, &credential)?;
    Ok(axum::Json(
        serde_json::to_value(DetailSnapshot {
            item,
            episodes,
            next_cursor,
        })
        .map_err(|_| ApiError::from("Title unavailable"))?,
    ))
}
