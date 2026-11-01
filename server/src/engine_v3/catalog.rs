use super::*;
use crate::gateway::diagnostics::{timed, timed_sync};
use viptv_engine::presentation::{CatalogPage, DetailSnapshot};

pub(crate) async fn discover(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Query(q): Query<crate::handlers_catalog::Discover>,
) -> ApiResult {
    let profile = super::watched::selected_profile(&lease);
    let mut page = discover_cards(State(app.clone()), Extension(lease.clone()), Query(q)).await?;
    if let Some(profile) = profile {
        super::watched::Watched::for_profile(&app, profile).mark_values(&mut page.0["cards"]);
        super::viewing_presentation::for_profile(&app,&lease,profile,&mut page.0)?;
    }
    Ok(page)
}

async fn discover_cards(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Query(q): Query<crate::handlers_catalog::Discover>,
) -> ApiResult {
    if q.kind != "all" {
        return discover_one(State(app), Extension(lease), Query(q)).await;
    }
    if !q.is_search()
        || q.skip != 0
        || q.catalog.is_some()
        || q.addon_id.is_some()
        || q.genre.is_some()
        || !q.selections()?.is_empty()
        || q.extras.as_deref().is_some_and(|s| s != "{}")
        || q.search.as_ref().is_some_and(|s| s.chars().count() > 256)
    {
        return Err("All-types search requires an unfiltered text query at offset zero".into());
    }
    let auth::Principal::Account {
        profile_id: Some(profile),
        ..
    } = lease.principal
    else {
        return Err(auth::forbidden());
    };
    let scoped = app.clone().with_lease(lease.clone());
    let (configuration, _) = configuration::read(&scoped, &lease, profile)?;
    let kinds = if configuration.browsing_plugin == "simkl" {
        vec!["movie".to_owned(), "series".to_owned(), "anime".to_owned()]
    } else {
        let (selected, _) = configuration::apply(
            scoped.clone(),
            &lease,
            profile,
            viptv_engine::capabilities::Capability::Search,
        )?;
        let mut kinds = std::collections::BTreeSet::new();
        for catalog in selected.addons.catalogs()?.as_array().into_iter().flatten() {
            if catalog["supports_search"] != true {
                continue;
            }
            if let Some(kind) = catalog["type"]
                .as_str()
                .filter(|kind| matches!(*kind, "movie" | "series"))
            {
                kinds.insert(kind.to_owned());
            }
        }
        kinds.into_iter().collect()
    };
    if kinds.is_empty() {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Search is not configured for this profile".into(),
        ));
    }
    let tasks = kinds.into_iter().take(3).map(|kind| {
        let mut query = q.clone();
        query.kind = kind;
        discover_one(State(app.clone()), Extension(lease.clone()), Query(query))
    });
    let pages = tokio::time::timeout(Duration::from_secs(12), futures::future::join_all(tasks))
        .await
        .map_err(|_| {
            ApiError(
                StatusCode::GATEWAY_TIMEOUT,
                "Search exceeded its response deadline".into(),
            )
        })?;
    let mut cards = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut complete = true;
    for result in pages {
        let axum::Json(value) = match result {
            Ok(value) => value,
            Err(error) if error.0.is_server_error() => {
                complete = false;
                continue;
            }
            Err(error) => return Err(error),
        };
        let page: CatalogPage = serde_json::from_value(value)
            .map_err(|_| ApiError::from("Search response was invalid"))?;
        complete &= page.complete;
        for card in page.cards {
            if cards.len() < 200
                && seen.insert((card.identity.kind.clone(), card.identity.id.clone()))
            {
                cards.push(card);
            }
        }
    }
    viptv_engine::search::rank(&mut cards, q.search.as_deref().unwrap_or_default());
    configuration::validate_revision(&scoped, profile, configuration.scope.revision)?;
    Ok(axum::Json(
        serde_json::to_value(CatalogPage {
            cards,
            next_cursor: None,
            complete,
        })
        .map_err(|_| ApiError::from("Search unavailable"))?,
    ))
}

async fn discover_one(
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
    let addon_revision = scoped.addons.revision()?;
    let (value, issuer) =
        crate::handlers_catalog::discover_origin(scoped.clone(), lease, q).await?;
    let provider = issuer
        .map(|id| format!("addon:{id}"))
        .unwrap_or_else(|| "stremio".into());
    configuration::validate_revision(&scoped, profile, revision)?;
    if scoped.addons.revision()? != addon_revision {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Catalog integrations changed. Try again.".into(),
        ));
    }
    let items = value["metas"].as_array().cloned().unwrap_or_default();
    let mut cards = Vec::new();
    for items in items.chunks(24).take(9) {
        cards.extend(projection::cards(&scoped, items.to_vec(), &provider)?);
    }

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
    pub(crate) offset: usize,
    pub(crate) limit: Option<usize>,
    #[serde(alias = "metadata_provider_id")]
    pub(crate) provider_id: Option<String>,
    #[serde(alias = "metadata_addon_id")]
    pub(crate) addon_id: Option<i64>,
    pub(crate) metadata_origin: Option<String>,
    pub(crate) card: Option<String>,
}

pub(crate) async fn detail(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path((kind, id)): Path<(String, String)>,
    Query(page): Query<DetailPage>,
) -> ApiResult {
    let profile = super::watched::selected_profile(&lease);
    let mut detail = resolve(app.clone(), lease.clone(), kind, id, page, true).await?;
    if let Some(profile) = profile {
        super::watched::Watched::for_profile(&app, profile)
            .mark_values(&mut detail.0["recommendations"]);
        super::viewing_presentation::for_profile(&app,&lease,profile,&mut detail.0)?;
    }
    Ok(detail)
}

pub(crate) async fn title_card(
    app: App,
    lease: ResourceLease,
    kind: String,
    id: String,
) -> Result<viptv_engine::presentation::Card, ApiError> {
    let page = DetailPage {
        limit: Some(1),
        ..Default::default()
    };
    let value = resolve(app, lease, kind, id, page, false).await?.0;
    serde_json::from_value::<DetailSnapshot>(value)
        .map(|detail| detail.item)
        .map_err(|_| ApiError::from("Metadata unavailable"))
}

async fn resolve(
    app: App,
    lease: ResourceLease,
    kind: String,
    id: String,
    mut page: DetailPage,
    with_recommendations: bool,
) -> ApiResult {
    if id.is_empty()
        || id.len() > 512
        || !["movie", "series"].contains(&kind.as_str())
        || page.offset > 100_000
        || page.limit.is_some_and(|limit| !(1..=128).contains(&limit))
    {
        return Err("Invalid metadata request".into());
    }
    let auth::Principal::Account { profile_id, .. } = &lease.principal;
    let profile = profile_id.ok_or_else(auth::forbidden)?;
    let app = app.with_lease(lease.clone());
    let id = crate::simkl::key::media(&kind, &id).into_owned();
    let mut expected = super::origins::parse_card(page.card.as_deref())?;
    if let Some(card) = expected.as_mut() {
        crate::simkl::key::card(card);
    }
    if expected
        .as_ref()
        .is_some_and(|card| card.identity.kind != kind || card.identity.id != id)
    {
        return Err("Metadata expectation does not match lookup".into());
    }
    let requested_origin = super::origins::parse_origin(page.metadata_origin.as_deref())?;
    if requested_origin
        .as_ref()
        .zip(
            expected
                .as_ref()
                .and_then(|card| card.metadata_origin.as_ref()),
        )
        .is_some_and(|(a, b)| a != b)
    {
        return Err("Conflicting metadata origin".into());
    }
    let requested_origin = requested_origin.or_else(|| {
        expected
            .as_ref()
            .and_then(|card| card.metadata_origin.clone())
    });
    (page.provider_id, page.addon_id) = super::origins::selectors(
        &app,
        page.provider_id.as_deref(),
        page.addon_id,
        requested_origin.as_ref(),
    )?;
    let (configuration, _) = configuration::read(&app, &lease, profile)?;
    let addon_revision = app.addons.revision()?;
    if page.provider_id.is_none() && page.addon_id.is_some() {
        return Err("Metadata provider is required".into());
    }
    let explicit = page
        .provider_id
        .as_deref()
        .map(|provider| {
            super::search::metadata_scope(app.clone(), &lease, profile, provider, page.addon_id)
        })
        .transpose()?;
    let simkl = page
        .provider_id
        .as_deref()
        .unwrap_or(&configuration.browsing_plugin)
        == "simkl";
    let stremio_fallback = simkl
        && page.provider_id.is_none()
        && !id.starts_with("simkl:")
        && !crate::simkl::linked(&app, profile)?;
    let scoped_policy = if let Some(origin) = &requested_origin {
        kids::scoped::require(&app, origin, &kind, &id, expected.as_ref())?
    } else {
        kids::consumers::require(
            &app,
            &kind,
            &id,
            if simkl { "simkl" } else { "stremio" },
            true,
        )?;
        None
    };
    let expected = expected.or(scoped_policy);
    if simkl && !stremio_fallback {
        let mut result = timed(
            "simkl_detail",
            super::simkl_catalog::detail(
                app.clone(),
                lease.clone(),
                profile,
                kind,
                id,
                page.offset,
                page.limit,
            ),
        )
        .await?;
        let mut detail: DetailSnapshot =
            serde_json::from_value(result.0).map_err(|_| ApiError::from("Metadata unavailable"))?;
        let show = timed(
            "anime_show",
            super::anime_show::detail(
                &app,
                &lease,
                profile,
                &detail.item,
                "simkl",
                page.offset,
                page.limit.unwrap_or(24),
            ),
        )
        .await;
        if let Some(show) = show {
            detail = show;
        }
        super::origins::stamp(&app, std::slice::from_mut(&mut detail.item), "simkl")?;
        super::origins::stamp(&app, &mut detail.episodes, "simkl")?;
        super::origins::stamp(&app, &mut detail.recommendations, "simkl")?;
        crate::simkl::key::card(&mut detail.item);
        for episode in &mut detail.episodes {
            crate::simkl::key::identity(&mut episode.identity);
        }
        for card in &mut detail.recommendations {
            crate::simkl::key::card(card);
        }
        super::origins::check_expected_at(&app, expected.as_ref(), &detail.item)?;
        result.0 =
            serde_json::to_value(detail).map_err(|_| ApiError::from("Metadata unavailable"))?;
        configuration::validate_revision(&app, profile, configuration.scope.revision)?;
        super::source_integrations::cache_detail(
            &app,
            &lease,
            configuration.scope.revision,
            &addon_revision,
            &result.0,
        );
        // Private watch state goes in after the shared verified-card cache.
        super::episode_progress::mark(&app, profile, &mut result.0);
        return Ok(result);
    }
    let (app, origin, revision) = if let Some(explicit) = explicit {
        explicit
    } else if stremio_fallback {
        super::search::metadata_scope(app, &lease, profile, "stremio", None)?
    } else {
        let (app, revision) = configuration::apply(
            app,
            &lease,
            profile,
            viptv_engine::capabilities::Capability::Metadata,
        )?;
        (app, "stremio".to_owned(), revision)
    };
    let lookup = if kind == "series" {
        episode_parent(&id).unwrap_or(&id)
    } else {
        id.as_str()
    };
    {
        let db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        app.require_media(&db)?;
        if requested_origin.is_none() {
            kids::require_item(&db, &app.identity(), &kind, &id)?;
        }
    }
    let (value, issuer) = timed("addons_meta", async {
        if page.addon_id.is_some() {
            app.addons.meta_from_selected_addon(&kind, lookup).await
        } else {
            app.addons.meta_with_origin(&kind, lookup).await
        }
    })
    .await
    .map_err(|error| {
        if page.addon_id.is_some() && error == "Metadata not found" {
            ApiError(
                StatusCode::BAD_GATEWAY,
                "The selected metadata add-on did not return this title".into(),
            )
        } else {
            ApiError::from(error)
        }
    })?;
    if app.addons.revision()? != addon_revision {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Metadata integrations changed. Try again.".into(),
        ));
    }
    let origin = if origin == "stremio" {
        format!("addon:{issuer}")
    } else {
        origin
    };
    configuration::validate_revision(&app, profile, revision)?;
    let meta = if value["meta"].is_object() {
        value["meta"].clone()
    } else {
        value
    };
    if meta["type"].as_str().is_some_and(|actual| actual != kind)
        || !provider_identity::same_title(&meta, lookup)
    {
        return Err(ApiError(
            StatusCode::BAD_GATEWAY,
            "Provider returned a different title".into(),
        ));
    }
    let mut parent = meta.clone();
    parent["type"] = json!(kind);
    parent["id"] = json!(lookup);
    if kind == "series" {
        let document = timed(
            "tvdb_document",
            super::anime_show::tvdb_document(&app, &lease, profile, &parent),
        )
        .await;
        if let Some(document) = document {
            parent = document;
        }
    }
    if requested_origin.is_none() {
        projection::cards(&app, vec![parent.clone()], &origin)?
            .into_iter()
            .next()
            .ok_or_else(auth::forbidden)?;
    }
    let mut parent = parent;
    let videos = parent["videos"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|mut video| {
            video["type"] = json!("series");
            video
        })
        .collect();
    {
        let db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        app.require_media(&db)?;
        parent["videos"] = if requested_origin.is_some() {
            json!(videos)
        } else {
            json!(kids::filter_items(&db, &app.identity(), videos)?)
        };
    }
    let mut projected = timed_sync(
        "detail_page",
        || {
            viptv_engine::stremio::detail_page(
                &json!({"meta":parent.clone()}).to_string(),
                &origin,
                &kind,
                &lookup,
                page.offset,
                page.limit.unwrap_or(24),
            )
        },
    )
    .map_err(|_| ApiError::from("Invalid episode metadata"))?;
    let seasons = projected.seasons;
    let mut episodes = projected.episodes;
    let mut item = projected.item;
    provider_identity::merge(&mut item.identity, &meta);
    for episode in &mut episodes {
        episode.parent_identity = Some(item.identity.clone());
    }
    super::origins::stamp(&app, std::slice::from_mut(&mut item), &origin)?;
    super::origins::stamp(&app, &mut episodes, &origin)?;
    super::origins::check_expected_at(&app, expected.as_ref(), &item)?;
    if requested_origin.is_some() {
        let mut all_episodes = timed_sync(
            "episode_cards",
            || viptv_engine::stremio::episode_cards(&parent, &origin, &item),
        )
        .map_err(|_| ApiError::from("Invalid episode metadata"))?;
        super::origins::stamp(&app, &mut all_episodes, &origin)?;
        let mut db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        let tx = db.transaction().map_err(db_error)?;
        lease.validate(&tx)?;
        kids::scoped::observe_document(
            &tx,
            lease.principal.account_id().ok_or_else(auth::forbidden)?,
            &item,
            &all_episodes,
            &parent,
        )?;
        let allowed = kids::scoped::require_card_db(&tx, &lease.principal, &item);
        episodes.retain(|card| kids::scoped::require_card_db(&tx, &lease.principal, card).is_ok());
        tx.commit().map_err(db_error)?;
        allowed?;
    }
    let next_cursor = projected.next_cursor;
    let recommendations = std::mem::take(&mut projected.recommendations);
    let mut recommendations = recommendations;
    super::origins::stamp(&app, &mut recommendations, &origin)?;
    let mut recommendations = super::projection::allowed_cards(&app, recommendations)?;
    if with_recommendations && recommendations.is_empty() {
        if let Some(genre) = item
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.genres.first())
        {
            if let Ok((discovery, _)) = configuration::apply(
                app.clone(),
                &lease,
                profile,
                viptv_engine::capabilities::Capability::Discovery,
            ) {
                if let Ok(Ok(result)) = timed(
                    "recommendations",
                    tokio::time::timeout(
                        Duration::from_secs(2),
                        discovery.addons.discover(
                            kind.clone(),
                            None,
                            None,
                            0,
                            None,
                            Some(genre.clone()),
                        ),
                    ),
                )
                .await
                {
                    recommendations = projection::cards(
                        &discovery,
                        result["metas"].as_array().cloned().unwrap_or_default(),
                        "stremio",
                    )?
                    .into_iter()
                    .filter(|card| {
                        card.identity.id != item.identity.id
                            && card.identity.kind == item.identity.kind
                            && card.identity.episode.is_none()
                    })
                    .take(24)
                    .collect();
                }
            }
        }
    }
    configuration::validate_revision(&app, profile, revision)?;
    if let Some(provider) = page.provider_id.as_deref() {
        super::search::metadata_scope(app.clone(), &lease, profile, provider, page.addon_id)?;
    }
    if app.addons.revision()? != addon_revision {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Metadata integrations changed. Try again.".into(),
        ));
    }
    let mut value = serde_json::to_value(DetailSnapshot {
        item,
        recommendations,
        episodes,
        next_cursor,
        seasons,
    })
    .map_err(|_| ApiError::from("Title unavailable"))?;
    super::source_integrations::cache_detail(&app, &lease, revision, &addon_revision, &value);
    // Private watch state goes in after the shared verified-card cache.
    super::episode_progress::mark(&app, profile, &mut value);
    Ok(axum::Json(value))
}

pub(super) fn episode_parent(id: &str) -> Option<&str> {
    let numeric = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    let mut parts = id.split(':');
    let (Some(title), Some(season), Some(episode), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    (title.strip_prefix("tt").is_some_and(numeric) && numeric(season) && numeric(episode))
        .then_some(title)
}
