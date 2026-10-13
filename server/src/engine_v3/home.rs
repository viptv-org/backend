use super::*;
use viptv_engine::{
    home::{RowRequest, assemble},
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
        let mut queue = continuation::queue(&db, profile, 0, 12)?;
        queue["items"] = json!(kids::filter_items(
            &db,
            &app.identity(),
            queue["items"].as_array().cloned().unwrap_or_default()
        )?);
        let allowed = kids::sql_allowed("favorites.type", "favorites.id", "?1");
        let mut q=db.prepare(&format!("SELECT id,type,name,poster,context,added_at_millis FROM favorites WHERE profile_id=?1 AND {allowed} ORDER BY added_at_millis DESC,name COLLATE NOCASE,type,id LIMIT 12")).map_err(db_error)?;
        let favorites=q.query_map([profile],|r|Ok(json!({"id":r.get::<_,String>(0)?,"type":r.get::<_,String>(1)?,"name":r.get::<_,String>(2)?,"poster":r.get::<_,Option<String>>(3)?,"added_at_millis":r.get::<_,Option<u64>>(5)?,"engine_favorite":serde_json::from_str::<Value>(&r.get::<_,String>(4)?).unwrap_or(Value::Null)}))).map_err(db_error)?.collect::<Result<Vec<_>,_>>().map_err(db_error)?;
        (
            queue,
            super::library::cards(&db, &app.identity(), favorites)?,
            configuration_revision.saturating_add(super::history::revision(&db, profile)?),
        )
    };
    let (configuration, _) = configuration::read(&app, &lease, profile)?;
    let simkl = configuration.browsing_plugin == "simkl";
    let catalogs = if simkl {
        json!([])
    } else {
        app.addons.catalogs()?
    };
    let queue = queue["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(super::history::record)
        .map(|record| record.card())
        .collect();
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
    if simkl {
        for (category, title) in [
            ("movie", "Simkl Trending Movies"),
            ("tv", "Simkl Trending TV Shows"),
            ("anime", "Simkl Trending Anime"),
        ] {
            let task_app = app.clone();
            requests.push(RowRequest {
                id: format!("simkl:{category}:trending"),
                title: title.into(),
                provider_id: "simkl".into(),
                load: Box::pin(crate::simkl::home_cards(task_app, profile, category)),
            });
        }
    }
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
    let mut snapshot = assemble(
        scope(&lease, profile, configuration_revision),
        requests,
        if simkl {
            app.simkl.home_budget()
        } else {
            Duration::from_secs(2)
        },
    )
    .await;
    if let Ok((metadata_app, _)) = configuration::apply(
        app.clone(),
        &lease,
        profile,
        viptv_engine::capabilities::Capability::Metadata,
    ) {
        enrich(&metadata_app, profile, &mut snapshot).await;
    }
    for row in &mut snapshot.rows{super::source_integrations::merge_cached(&app,&lease,configuration_revision,&mut row.cards);}
    if let Some(hero)=&mut snapshot.hero{super::source_integrations::merge_cached(&app,&lease,configuration_revision,std::slice::from_mut(hero));}
    snapshot.revision = revision;
    configuration::validate_revision(&app, profile, configuration_revision)?;
    lease.validate(
        &*app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?,
    )?;
    Ok(snapshot)
}

async fn enrich(app: &App, profile: i64, snapshot: &mut HomeSnapshot) {
    use futures::StreamExt;
    let mut candidates = snapshot.hero.iter().cloned().collect::<Vec<_>>();
    candidates.extend(
        snapshot
            .rows
            .iter()
            .filter(|r| r.id == "continue")
            .flat_map(|r| r.cards.iter().take(12))
            .cloned(),
    );
    let mut seen = std::collections::HashSet::new();
    candidates.retain(|card| {
        let identity = card.parent_identity.as_ref().unwrap_or(&card.identity);
        seen.insert((identity.kind.clone(), identity.id.clone()))
    });
    let mut tasks = futures::stream::iter(candidates.into_iter().map(|card| {
        let app = app.clone();
        async move {
            let result = if card
                .parent_identity
                .as_ref()
                .unwrap_or(&card.identity)
                .id
                .starts_with("simkl:")
            {
                crate::simkl::home_metadata(&app, profile, card.clone())
                    .await
                    .ok()
            } else {
                let identity = card.parent_identity.as_ref().unwrap_or(&card.identity);
                match app.addons.meta(&identity.kind, &identity.id).await {
                    Ok(raw) => viptv_engine::stremio::detail(
                        &raw.to_string(),
                        &card.provider_id,
                        &identity.kind,
                        &identity.id,
                        0,
                    )
                    .ok()
                    .map(|detail| detail.item),
                    Err(_) => None,
                }
            };
            (card, result)
        }
    }))
    .buffer_unordered(4);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    let mut presented = Vec::new();
    while let Ok(Some((original, result))) = tokio::time::timeout_at(deadline, tasks.next()).await {
        let Some(metadata) = result else {
            continue;
        };
        super::source_integrations::cache_card(app,&app.request_lease(),snapshot.scope.revision,metadata.clone());
        apply_metadata(snapshot, &original, &metadata);
        presented.push((original, metadata));
    }
    // Artwork cannot hold completed authoritative metadata behind its deadline.
    let mut logos = futures::stream::iter(
        presented
            .into_iter()
            .filter(|(_, card)| card.provider_id == "simkl")
            .map(|(original, mut metadata)| {
                let app = app.clone();
                async move {
                    crate::simkl::enrich_artwork(&app, &mut metadata).await;
                    (original, metadata)
                }
            }),
    )
    .buffer_unordered(4);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while let Ok(Some((original, metadata))) = tokio::time::timeout_at(deadline, logos.next()).await
    {
        super::source_integrations::cache_card(app,&app.request_lease(),snapshot.scope.revision,metadata.clone());
        apply_metadata(snapshot, &original, &metadata);
    }
}
fn apply_metadata(
    snapshot: &mut HomeSnapshot,
    original: &viptv_engine::presentation::Card,
    metadata: &viptv_engine::presentation::Card,
) {
    let identity = original
        .parent_identity
        .as_ref()
        .unwrap_or(&original.identity);
    for card in snapshot.hero.iter_mut().chain(
        snapshot
            .rows
            .iter_mut()
            .flat_map(|row| row.cards.iter_mut()),
    ) {
        let target = card.parent_identity.as_ref().unwrap_or(&card.identity);
        if target.id == identity.id && target.kind == identity.kind {
            *card = viptv_engine::presentation::enrich_card(card, metadata);
        }
    }
}
