use super::*;
use viptv_engine::{capabilities::Capability, presentation::Card, registry::Registry};

const ROW_BUDGET: Duration = Duration::from_secs(4);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SearchQuery {
    query: String,
    profile_id: Option<i64>,
}

#[derive(Clone)]
enum Plan {
    Simkl(&'static str, &'static str),
    Addon(i64, String, Option<String>, Vec<String>),
}
impl Plan {
    fn row(&self, cards: Vec<Card>, failed: bool) -> Value {
        let (id, title, provider, addon) = match self {
            Self::Simkl(category, title) => (
                format!("simkl:{category}"),
                title.to_string(),
                "simkl",
                None,
            ),
            Self::Addon(id, label, catalog, _) => (
                match catalog {
                    Some(catalog) => format!("addon:{id}:{catalog}"),
                    None => format!("addon:{id}"),
                },
                label.clone(),
                "stremio",
                Some(*id),
            ),
        };
        let state = if cards.is_empty() {
            if failed {
                "Failed"
            } else {
                "Empty"
            }
        } else {
            "Ready"
        };
        json!({"id":id,"title":title,"provider_id":provider,"addon_id":addon,"state":state,"cards":cards,
            "message":failed.then_some("Some search results are unavailable. Try again.")})
    }
}

pub(crate) async fn get(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Query(q): Query<SearchQuery>,
) -> ApiResult {
    let profile = super::watched::selected_profile(&lease);
    let mut result = run(app.clone(), lease, q, false).await?;
    if let Some(profile) = profile {
        let watched = super::watched::Watched::for_profile(&app, profile);
        for row in result.0["rows"].as_array_mut().into_iter().flatten() {
            watched.mark_values(&mut row["cards"]);
        }
    }
    Ok(result)
}
pub(crate) async fn parent(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Query(q): Query<SearchQuery>,
) -> ApiResult {
    run(app, lease, q, true).await
}
async fn run(app: App, lease: ResourceLease, q: SearchQuery, parent: bool) -> ApiResult {
    let query = q.query.trim();
    if query.is_empty() || query.chars().count() > 256 || query.chars().any(char::is_control) {
        return Err("Enter a search query of at most 256 characters".into());
    }
    let auth::Principal::Account {
        profile_id: Some(selected_profile),
        ..
    } = lease.principal
    else {
        return Err(auth::forbidden());
    };
    let app = app.with_lease(lease.clone());
    if !parent && q.profile_id.is_some() {
        return Err("Target profiles are only supported in parent search".into());
    }
    let profile = if parent {
        q.profile_id.unwrap_or(selected_profile)
    } else {
        selected_profile
    };
    let parent_scope = parent
        .then(|| crate::kids::parent_scope::ParentScope::new(&app, profile))
        .transpose()?;
    let (mut configuration, connections) = if let Some(scope) = &parent_scope {
        configuration::read_parent(&app, scope)?
    } else {
        configuration::read(&app, &lease, profile)?
    };
    configuration.assignments.retain(|assignment| {
        connections
            .iter()
            .any(|connection| connection.id == assignment.connection_id)
    });
    let registry = Registry::builtin();
    let selected = registry
        .connections_for(&configuration, &connections, Capability::Search)
        .map_err(|_| ApiError::from("Integration unavailable"))?;
    let mut plans = Vec::new();
    if selected
        .iter()
        .any(|connection| connection.plugin_id == "simkl")
    {
        plans.extend([
            Plan::Simkl("anime", "Anime"),
            Plan::Simkl("movie", "Movies"),
            Plan::Simkl("tv", "TV Shows"),
        ]);
    }
    let ids = selected
        .iter()
        .filter_map(|connection| connection.id.strip_prefix("addon:")?.parse::<i64>().ok())
        .take(32)
        .collect::<Vec<_>>();
    let mut addons = app.clone();
    addons.addons = addons.addons.with_engine_ids(ids.clone());
    let addon_revision = addons.addons.revision()?;
    let catalogs = addons.addons.catalogs()?;
    for id in ids {
        let mut by_catalog: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
            std::collections::BTreeMap::new();
        for catalog in catalogs.as_array().into_iter().flatten() {
            if catalog["addon_id"] == id && catalog["supports_search"] == true {
                let Some(kind) = catalog["type"]
                    .as_str()
                    .filter(|kind| matches!(*kind, "movie" | "series"))
                else {
                    continue;
                };
                let catalog_id = catalog["id"].as_str().unwrap_or("").to_owned();
                let entry = by_catalog.entry(catalog_id).or_default();
                entry.insert(kind.to_owned());
                if let Some(name) = catalog["name"].as_str() {
                    entry.insert(format!("name:{name}"));
                }
            }
        }
        if by_catalog.is_empty() {
            continue;
        }
        let name = selected
            .iter()
            .find(|connection| connection.id == format!("addon:{id}"))
            .map(|connection| {
                connection
                    .name
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(128)
                    .collect::<String>()
            })
            .unwrap_or_else(|| "Stremio addon".into());
        let split = by_catalog.len() > 1;
        for (catalog_id, kinds) in by_catalog {
            let label = if split {
                kinds
                    .iter()
                    .find_map(|k| k.strip_prefix("name:"))
                    .map(|label| format!("{name} · {label}"))
                    .unwrap_or_else(|| name.clone())
            } else {
                name.clone()
            };
            let kinds: Vec<String> = kinds
                .into_iter()
                .filter(|k| !k.starts_with("name:"))
                .collect();
            if kinds.is_empty() {
                continue;
            }
            plans.push(Plan::Addon(
                id,
                label,
                split.then_some(catalog_id),
                kinds,
            ));
        }
    }
    let rows = futures::future::join_all(plans.into_iter().map(|plan| {
        let app = addons.clone();
        let lease = lease.clone();
        let query = query.to_owned();
        let parent_scope = parent_scope.clone();
        async move {
            let result = match &plan {
                Plan::Simkl(_, _) => tokio::time::timeout(
                    ROW_BUDGET,
                    load(&app, &lease, profile, &plan, &query, parent_scope.as_ref()),
                )
                .await
                .ok(),
                Plan::Addon(_, _, _, _) => {
                    Some(load(&app, &lease, profile, &plan, &query, parent_scope.as_ref()).await)
                }
            };
            match result {
                Some(Ok((mut cards, failed))) => {
                    let provider = match &plan {
                        Plan::Simkl(_, _) => "simkl".to_owned(),
                        Plan::Addon(id, _, _, _) => format!("addon:{id}"),
                    };
                    match super::origins::stamp(&app, &mut cards, &provider) {
                        Ok(()) => {
                            dedupe_display_names(&mut cards);
                            plan.row(cards, failed)
                        }
                        Err(_) => plan.row(vec![], true),
                    }
                }
                _ => plan.row(vec![], true),
            }
        }
    }))
    .await;
    if let Some(scope) = &parent_scope {
        scope.recheck(&app)?;
    } else {
        configuration::validate_revision(&app, profile, configuration.scope.revision)?;
    }
    if addons.addons.revision()? != addon_revision {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Search integrations changed. Try again.".into(),
        ));
    }
    let rows = if parent {
        rows.into_iter().map(|mut row| {
            let provider=row["provider_id"].as_str().unwrap_or("");
            let metadata_ready=metadata_scope_parent(app.clone(),parent_scope.as_ref().expect("parent scope"),provider,row["addon_id"].as_i64()).is_ok_and(|(_,_,revision)|revision==configuration.scope.revision);
            let cards=row.as_object_mut().and_then(|row|row.remove("cards")).unwrap_or(json!([]));
            row["items"]=json!(cards.as_array().into_iter().flatten().map(|card| {
                let provider=row["provider_id"].as_str().unwrap_or("");
                let supported=metadata_ready && (crate::kids::titles::canonical(provider,card["identity"]["kind"].as_str().unwrap_or(""),card["identity"]["id"].as_str().unwrap_or("")) || provider=="stremio" && card["metadata_origin"].is_object() && !card["identity"]["id"].as_str().is_some_and(|id|id.starts_with("simkl:")));
                let message=if !metadata_ready {Some("Metadata is not configured for this result.")}else if !supported {Some("This result does not have a supported metadata origin.")}else{None};
                json!({"card":card,"approval_supported":supported,"message":message})
            }).collect::<Vec<_>>());
            row
        }).collect()
    } else {
        rows
    };
    if parent {
        parent_scope.as_ref().expect("parent scope").recheck(&app)?;
        if addons.addons.revision()? != addon_revision {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "Search integrations changed. Try again.".into(),
            ));
        }
    }
    let complete = rows.iter().all(|row| row["message"].is_null());
    Ok(axum::Json(json!({"rows":rows,"complete":complete})))
}

async fn load(
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    plan: &Plan,
    query: &str,
    parent: Option<&crate::kids::parent_scope::ParentScope>,
) -> Result<(Vec<Card>, bool), ApiError> {
    match plan {
        Plan::Simkl(category, _) => {
            if let Some(scope) = parent {
                return Ok((
                    crate::simkl::parent_search(app, scope, category, query).await?,
                    false,
                ));
            }
            let axum::Json(value) = crate::simkl::search(
                State(app.clone()),
                Extension(lease.clone()),
                Path(profile),
                Query(crate::simkl::Search {
                    category: category.to_string(),
                    query: query.to_owned(),
                    page: 1,
                }),
            )
            .await?;
            let page: viptv_engine::presentation::CatalogPage = serde_json::from_value(value)
                .map_err(|_| ApiError::from("Invalid search response"))?;
            Ok((page.cards, false))
        }
        Plan::Addon(id, _, catalog, kinds) => {
            let (items, failed) = app
                .addons
                .search_results(*id, kinds.clone(), catalog.clone(), query.to_owned(), ROW_BUDGET)
                .await?;
            let mut cards = Vec::new();
            for batch in items.chunks(24) {
                if parent.is_some() {
                    cards.extend(
                        batch
                            .iter()
                            .filter(|meta| !crate::kids::adult(meta))
                            .filter_map(|meta| projection::card(meta, &format!("addon:{id}"))),
                    );
                } else {
                    cards.extend(projection::cards(
                        app,
                        batch.to_vec(),
                        &format!("addon:{id}"),
                    )?);
                }
            }
            viptv_engine::search::rank(&mut cards, query);
            Ok((cards, failed))
        }
    }
}

fn display_title(title: &str) -> String {
    title
        .chars()
        .flat_map(char::to_lowercase)
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
fn dedupe_display_names(cards: &mut Vec<Card>) {
    let titled = |card: &Card| (card.identity.kind.clone(), display_title(&card.title));
    let dated = cards
        .iter()
        .filter(|card| card.metadata.as_ref().and_then(|m| m.year).is_some())
        .map(|card| titled(card))
        .collect::<std::collections::HashSet<_>>();
    cards.retain(|card| {
        card.metadata.as_ref().and_then(|m| m.year).is_some()
            || !dated.contains(&titled(card))
    });
}
pub(crate) fn metadata_scope(
    app: App,
    lease: &ResourceLease,
    profile: i64,
    provider: &str,
    addon: Option<i64>,
) -> Result<(App, String, u64), ApiError> {
    metadata_scope_inner(app, lease, profile, provider, addon, None)
}
pub(crate) fn metadata_scope_parent(
    app: App,
    parent: &crate::kids::parent_scope::ParentScope,
    provider: &str,
    addon: Option<i64>,
) -> Result<(App, String, u64), ApiError> {
    metadata_scope_inner(
        app,
        parent.lease(),
        parent.profile(),
        provider,
        addon,
        Some(parent),
    )
}
fn metadata_scope_inner(
    mut app: App,
    lease: &ResourceLease,
    profile: i64,
    provider: &str,
    addon: Option<i64>,
    parent: Option<&crate::kids::parent_scope::ParentScope>,
) -> Result<(App, String, u64), ApiError> {
    if !matches!(provider, "simkl" | "stremio")
        || (provider != "stremio" && addon.is_some())
        || addon.is_some_and(|id| id <= 0)
    {
        return Err("Invalid metadata origin".into());
    }
    let (mut configuration, connections) = if let Some(scope) = parent {
        configuration::read_parent(&app, scope)?
    } else {
        configuration::read(&app, lease, profile)?
    };
    configuration.assignments.retain(|assignment| {
        connections
            .iter()
            .any(|connection| connection.id == assignment.connection_id)
    });
    let revision = configuration.scope.revision;
    configuration.browsing_plugin = provider.to_owned();
    let registry = Registry::builtin();
    let metadata = registry
        .connections_for(&configuration, &connections, Capability::Metadata)
        .map_err(|_| auth::forbidden())?;
    if metadata.is_empty() {
        return Err(auth::forbidden());
    }
    let origin = if let Some(id) = addon {
        let owner = format!("addon:{id}");
        let connection = connections
            .iter()
            .find(|connection| {
                connection.id == owner && connection.enabled && connection.plugin_id == "stremio"
            })
            .ok_or_else(auth::forbidden)?;
        if !configuration.assignments.iter().any(|assignment| {
            assignment.connection_id == connection.id
                && assignment
                    .enabled
                    .iter()
                    .any(|cap| matches!(cap, Capability::Search | Capability::Metadata))
        }) {
            return Err(auth::forbidden());
        }
        owner
    } else {
        provider.to_owned()
    };
    let preferred = metadata.iter().find(|connection| connection.id == origin);
    let ids = if let Some(connection) = preferred {
        vec![connection
            .id
            .strip_prefix("addon:")
            .and_then(|id| id.parse().ok())
            .ok_or_else(auth::forbidden)?]
    } else {
        metadata
            .iter()
            .filter_map(|connection| connection.id.strip_prefix("addon:")?.parse().ok())
            .collect()
    };
    app.addons = app.addons.with_engine_ids(ids);
    Ok((app, origin, revision))
}

pub(crate) fn source_origin(
    app: &App,
    value: &Value,
) -> Result<Option<(String, Option<i64>, String)>, ApiError> {
    if let Some(origin) = value.get("metadata_origin") {
        let origin: viptv_engine::origin::MetadataOrigin =
            serde_json::from_value(origin.clone())
                .map_err(|_| ApiError::from("Invalid metadata origin"))?;
        let provider = value
            .get("metadata_provider_id")
            .map(|v| v.as_str().ok_or("Invalid metadata origin"))
            .transpose()?;
        let addon = value
            .get("metadata_addon_id")
            .map(|v| v.as_i64().ok_or("Invalid metadata origin"))
            .transpose()?;
        let (provider, addon) = super::origins::selectors(app, provider, addon, Some(&origin))?;
        return request_origin(app, provider.as_deref(), addon);
    }
    let Some(raw) = value.get("metadata_provider_id") else {
        if value.get("metadata_addon_id").is_some() {
            return Err("Metadata provider is required".into());
        }
        return Ok(None);
    };
    let provider = raw.as_str().ok_or("Invalid metadata origin")?;
    let addon = value
        .get("metadata_addon_id")
        .map(|value| {
            value
                .as_i64()
                .ok_or_else(|| ApiError::from("Invalid metadata origin"))
        })
        .transpose()?;
    request_origin(app, Some(provider), addon)
}

pub(crate) fn request_origin(
    app: &App,
    provider: Option<&str>,
    addon: Option<i64>,
) -> Result<Option<(String, Option<i64>, String)>, ApiError> {
    let Some(provider) = provider else {
        if addon.is_some() {
            return Err("Metadata provider is required".into());
        }
        return Ok(None);
    };
    let lease = app.request_lease();
    let auth::Principal::Account {
        profile_id: Some(profile),
        ..
    } = lease.principal
    else {
        return Err(auth::forbidden());
    };
    let (_, origin, _) = metadata_scope(app.clone(), &lease, profile, provider, addon)?;
    Ok(Some((provider.to_owned(), addon, origin)))
}
#[cfg(test)]
mod tests {
    use super::*;
    use viptv_engine::{identity::MediaIdentity, presentation::CardMetadata};
    fn card(kind: &str, title: &str, year: Option<u32>) -> Card {
        Card {
            metadata_origin: None,
            identity: MediaIdentity {
                id: format!("{kind}:{title}:{year:?}"),
                kind: kind.into(),
                ids: vec![],
                episode: None,
                edition: None,
            },
            provider_id: "addon:1".into(),
            title: title.into(),
            subtitle: None,
            description: None,
            poster: None,
            landscape: None,
            logo: None,
            progress: None,
            badges: vec![],
            parent_identity: None,
            added_at_millis: None,
            category: Some(kind.into()),
            metadata: Some(CardMetadata {
                year,
                ..Default::default()
            }),
        }
    }
    #[test]
    fn year_less_duplicate_display_names_are_dropped() {
        let mut cards = vec![
            card("movie", "Inception", Some(2010)),
            card("movie", "Inception", None),
            card("series", "Inception", None),
        ];
        dedupe_display_names(&mut cards);
        assert_eq!(cards.len(), 2);
        assert!(cards.iter().any(|card| card.identity.kind == "movie"
            && card.metadata.as_ref().and_then(|m| m.year) == Some(2010)));
        assert!(cards.iter().any(|card| card.identity.kind == "series"));
    }
    #[test]
    fn year_less_same_name_is_kept_when_no_same_named_title_has_a_year() {
        let mut cards = vec![
            card("movie", "Inception", None),
            card("movie", "Inception", None),
        ];
        dedupe_display_names(&mut cards);
        assert_eq!(cards.len(), 2);
    }
}
