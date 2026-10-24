use super::*;
use viptv_engine::{
    home::{RowRequest, assemble},
    presentation::HomeSnapshot,
};

pub(super) struct PresentedHome {
    pub snapshot: HomeSnapshot,
    previews: Vec<Vec<Value>>,
}
impl PresentedHome {
    /// Applied after the shared card decorator, without trusting provider or
    /// client-supplied view fields. Both arrays come from this same snapshot.
    pub fn attach(&self, value: &mut Value) {
        for (row, previews) in self.previews.iter().enumerate() {
            for (column, preview) in previews.iter().enumerate() {
                value["rows"][row]["cards"][column]["view"]["preview"] = preview.clone();
            }
        }
    }
}
fn preview_key(card: &viptv_engine::presentation::Card) -> String {
    let identity = card.parent_identity.as_ref().unwrap_or(&card.identity);
    serde_json::to_string(&(&identity.kind, &identity.id, &card.metadata_origin,
        card.metadata_origin.is_none().then_some(&card.provider_id)))
        .expect("presentation identity contains only serializable scalar fields")
}

pub(super) async fn load(
    app: App,
    lease: ResourceLease,
    profile: i64,
) -> Result<PresentedHome, ApiError> {
    let app = app.with_lease(lease.clone());
    let (app, configuration_revision) = configuration::apply(
        app,
        &lease,
        profile,
        viptv_engine::capabilities::Capability::Discovery,
    )?;
    let home_addon_revision = app.addons.revision()?;
    crate::simkl::imports::schedule(&app, &lease, profile);
    super::continuation::releases::schedule(&app, &lease, profile);
    // Use the same origin-aware personal projections as the explicit routes.
    // This snapshot owns its DB lock; never call it inside the favorites lock.
    let continuation = super::continuation::snapshot(&app, &lease, profile, 0, 12)?;
    let queue = continuation
        .entries
        .iter()
        .map(|entry| entry.card.clone())
        .collect();
    let (favorites, revision) = {
        let db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        app.require_profile(&db, profile)?;
        (
            super::scoped_library::home_favorites_db(&db, &app, profile)?,
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
    let mut browse_actions = HashMap::from([
        (
            "continue".to_owned(),
            viptv_engine::home_browse::HomeBrowseAction::continuation(),
        ),
        (
            "favorites".to_owned(),
            viptv_engine::home_browse::HomeBrowseAction::library(),
        ),
    ]);
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
            let row_id = format!("simkl:{category}:trending");
            browse_actions.insert(
                row_id.clone(),
                viptv_engine::home_browse::HomeBrowseAction::discover(
                    if category == "tv" { "series" } else { category },
                    "simkl-trending",
                    None,
                ),
            );
            requests.push(RowRequest {
                id: row_id,
                title: title.into(),
                provider_id: "simkl".into(),
                load: Box::pin(crate::simkl::home_cards(task_app, profile, category)),
            });
        }
    }
    for c in catalogs.as_array().into_iter().flatten().take(10) {
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
        let row_id = format!("{provider}:{kind}:{catalog}");
        browse_actions.insert(
            row_id.clone(),
            viptv_engine::home_browse::HomeBrowseAction::discover(
                &kind,
                &catalog,
                Some(("addon_id", addon.to_string())),
            ),
        );
        requests.push(RowRequest {
            id: row_id,
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
    for row in &mut snapshot.rows {
        row.browse = browse_actions.remove(&row.id);
        super::source_integrations::merge_cached(
            &app,
            &lease,
            configuration_revision,
            &mut row.cards,
        );
    }
    if let Some(hero) = &mut snapshot.hero {
        super::source_integrations::merge_cached(
            &app,
            &lease,
            configuration_revision,
            std::slice::from_mut(hero),
        );
    }
    super::source_integrations::merge_cached(
        &app,
        &lease,
        configuration_revision,
        &mut snapshot.featured,
    );
    snapshot.hero = snapshot.featured.first().cloned();
    snapshot.revision = revision;
    if app.addons.revision()? != home_addon_revision {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Home integrations changed. Try again.".into(),
        ));
    }
    configuration::validate_revision(&app, profile, configuration_revision)?;
    // Continue Watching cards come marked by the queue itself: only a finished
    // episode its series stays on, never a title in progress (a rewatch of a
    // finished movie keeps its progress bar and no check).
    let watched = super::watched::Watched::for_profile(&app, profile);
    for row in &mut snapshot.rows {
        if row.id != "continue" {
            watched.mark_all(row.cards.iter_mut());
        }
    }
    watched.mark_all(snapshot.hero.iter_mut().chain(snapshot.featured.iter_mut()));
    lease.validate(
        &*app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?,
    )?;
    // Reuse the already-verified parent metadata in one bounded cache read.
    // Episode artwork stays on the tile; the preview can use its series art.
    let parents = snapshot.rows.iter().flat_map(|row| &row.cards).filter_map(|card| {
        let parent = card.parent_identity.as_ref()?;
        let mut preview = card.clone();
        preview.identity = parent.clone(); preview.parent_identity = None;
        Some((preview_key(card), preview))
    }).collect::<Vec<_>>();
    let (keys, mut parents): (Vec<_>, Vec<_>) = parents.into_iter().unzip();
    super::source_integrations::merge_cached(&app, &lease, configuration_revision, &mut parents);
    let backgrounds = keys.into_iter().zip(parents.into_iter().map(|card| card.landscape)).collect::<HashMap<_,_>>();
    let previews = snapshot.rows.iter().map(|row| row.cards.iter().map(|card| {
        let background = backgrounds.get(&preview_key(card)).and_then(|image| image.clone()).or_else(|| card.landscape.clone());
        let entry = continuation.entries.iter().find(|entry| super::viewing_action::entry_matches(entry, card));
        let (caption, progress) = if let Some(entry) = entry {
            let (progress, remaining) = super::viewing_action::continuation_display(entry);
            let label = super::viewing_action::action_label(&entry.card, entry.status == viptv_engine::continuation::ContinuationStatus::Resume);
            (Some(remaining.map(|remaining| format!("{label} · {remaining}")).unwrap_or(label)), progress)
        } else { (None, None) };
        let action = if row.id == "continue" && entry.is_some() { "play" } else { "details" };
        json!({"background":background,"caption":caption,"progress":progress,"action":action})
    }).collect()).collect();
    Ok(PresentedHome { snapshot, previews })
}

async fn enrich(app: &App, profile: i64, snapshot: &mut HomeSnapshot) {
    use futures::StreamExt;
    let Ok(addon_revision) = app.addons.revision() else {
        return;
    };
    let mut candidates = snapshot
        .featured
        .iter()
        .take(5)
        .cloned()
        .collect::<Vec<_>>();
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
        seen.insert((
            card.metadata_origin.as_ref().and_then(|o| o.key().ok()),
            card.provider_id.clone(),
            serde_json::to_string(identity).unwrap_or_default(),
        ))
    });
    let mut tasks = futures::stream::iter(candidates.into_iter().map(|card| {
        let app = app.clone();
        async move {
            let simkl_origin = card
                .metadata_origin
                .as_ref()
                .map(|origin| origin.provider_id == "simkl");
            let result = if simkl_origin == Some(true)
                || simkl_origin.is_none()
                    && (card
                        .parent_identity
                        .as_ref()
                        .unwrap_or(&card.identity)
                        .id
                        .starts_with("simkl:")
                        || crate::simkl::imports::remote_identity(
                            &app,
                            profile,
                            &card.parent_identity.as_ref().unwrap_or(&card.identity).kind,
                            &card.parent_identity.as_ref().unwrap_or(&card.identity).id,
                        )
                        .ok()
                        .flatten()
                        .is_some())
            {
                crate::simkl::home_metadata(&app, profile, card.clone())
                    .await
                    .ok()
            } else {
                let identity = card.parent_identity.as_ref().unwrap_or(&card.identity);
                let metadata = if let Some(origin) = &card.metadata_origin {
                    let selected = super::origins::selectors(&app, None, None, Some(origin));
                    selected
                        .and_then(|(provider, addon)| {
                            super::search::metadata_scope(
                                app.clone(),
                                &app.request_lease(),
                                profile,
                                provider.as_deref().ok_or_else(auth::forbidden)?,
                                addon,
                            )
                        })
                        .map(|(app, _, _)| app)
                } else {
                    Ok(app.clone())
                };
                let fetched = match metadata {
                    Ok(host) => host.addons.meta(&identity.kind, &identity.id).await,
                    Err(_) => Err("Metadata scope unavailable".into()),
                };
                match fetched {
                    Ok(raw) => viptv_engine::stremio::detail(
                        &raw.to_string(),
                        &card.provider_id,
                        &identity.kind,
                        &identity.id,
                        0,
                    )
                    .ok()
                    .map(|detail| {
                        let mut item = detail.item;
                        item.metadata_origin = card.metadata_origin.clone();
                        item
                    }),
                    Err(_) => None,
                }
            };
            (card, result)
        }
    }))
    .buffer_unordered(3);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    let mut presented = Vec::new();
    while let Ok(Some((original, result))) = tokio::time::timeout_at(deadline, tasks.next()).await {
        let Some(metadata) = result else {
            continue;
        };
        let mut expected = original.clone();
        if let Some(parent) = &original.parent_identity {
            expected.identity = parent.clone();
            expected.parent_identity = None;
        }
        if super::origins::check_expected_at(app, Some(&expected), &metadata).is_err() {
            continue;
        }
        super::source_integrations::cache_card_at(
            app,
            &app.request_lease(),
            snapshot.scope.revision,
            &addon_revision,
            metadata.clone(),
        );
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
    .buffer_unordered(3);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while let Ok(Some((original, metadata))) = tokio::time::timeout_at(deadline, logos.next()).await
    {
        super::source_integrations::cache_card_at(
            app,
            &app.request_lease(),
            snapshot.scope.revision,
            &addon_revision,
            metadata.clone(),
        );
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
    for card in snapshot
        .hero
        .iter_mut()
        .chain(snapshot.featured.iter_mut())
        .chain(
            snapshot
                .rows
                .iter_mut()
                .flat_map(|row| row.cards.iter_mut()),
        )
    {
        let target = card.parent_identity.as_ref().unwrap_or(&card.identity);
        if target.id == identity.id
            && target.kind == identity.kind
            && card.metadata_origin == original.metadata_origin
            && (card.metadata_origin.is_some() || card.provider_id == original.provider_id)
        {
            *card = viptv_engine::presentation::enrich_card(card, metadata);
        }
    }
}
