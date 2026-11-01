use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_secs() as i64)
}

/// A catalog row can page: the row maps to a browsable catalog and the first page
/// was full enough that another page may exist. Rows that are already complete
/// locally (Continue Watching, favourites) have no browse action and stay `None`.
fn row_cursor(request: &CatalogRequest, cards: &[viptv_engine::presentation::Card]) -> Option<String> {
    request.browse.as_ref()?;
    (!cards.is_empty()).then(|| cards.len().to_string())
}

#[uniffi::export]
impl LocalEngine {
    pub fn plan_home(&self, profile_id: String) -> Result<String, CoreError> {
        let scope = local_scope(&profile_id);
        let store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        let profile = store
            .profile(&scope)
            .map_err(|_| CoreError::Bridge)?
            .ok_or(CoreError::InvalidInput)?;
        let connections = store.connections("local").map_err(|_| CoreError::Bridge)?;
        let mut rows = Vec::new();
        for connection in connections
            .iter()
            .filter(|c| {
                c.enabled
                    && c.plugin_id == profile.browsing_plugin
                    && profile.assignments.iter().any(|a| {
                        a.connection_id == c.id && a.enabled.contains(&Capability::Discovery)
                    })
            })
            .take(8)
        {
            let secret = match self.addon(&connection.id) {
                Ok(s) => s,
                Err(_) => {
                    rows.push(CatalogRequest {
                        origin: store
                            .metadata_origin(&profile.scope, &connection.id)
                            .map_err(|_| CoreError::InvalidInput)?,
                        browse: None,
                        id: format!("{}:unavailable", connection.id),
                        title: connection.name.clone(),
                        provider_id: connection.id.clone(),
                        url: None,
                    });
                    continue;
                }
            };
            let parsed = stremio::parse_manifest(&secret.manifest.to_string())
                .map_err(|_| CoreError::Bridge)?;
            for catalog in parsed
                .catalogs
                .into_iter()
                .filter(|c| !c.required_extras)
                .take(4)
            {
                if rows.len() >= 12 {
                    break;
                }
                rows.push(CatalogRequest {
                    origin: store
                        .metadata_origin(&profile.scope, &connection.id)
                        .map_err(|_| CoreError::InvalidInput)?,
                    browse: Some(viptv_engine::home_browse::HomeBrowseAction::discover(
                        &catalog.kind,
                        &catalog.id,
                        Some(("connection_id", connection.id.clone())),
                    )),
                    id: format!("{}:{}:{}", connection.id, catalog.kind, catalog.id),
                    title: format!("{} · {}", connection.name, catalog.name),
                    provider_id: connection.id.clone(),
                    url: stremio::catalog_url(&secret.manifest_url, &catalog).ok(),
                });
            }
        }
        let id = uuid::Uuid::new_v4().to_string();
        let cached = rows
            .iter()
            .filter_map(|row| {
                store
                    .cached_catalog(&profile.scope, &row.id, now())
                    .ok()
                    .flatten()
                    .filter(|cards| {
                        cards.iter().all(|card| {
                            card.metadata_origin.as_ref().is_none_or(|origin| {
                                store
                                    .validate_metadata_origin(&profile.scope, origin)
                                    .is_ok()
                            })
                        })
                    })
                    .map(|cards| (row.id.clone(), cards))
            })
            .collect::<std::collections::HashMap<_, _>>();
        let requests: Vec<_> = rows
            .iter()
            .filter(|row| !cached.contains_key(&row.id))
            .cloned()
            .collect();
        let pending = PendingHome {
            scope: profile.scope,
            rows: rows.clone(),
            cached,
            cache_home: true,
            discover: None,
        };
        let mut operations = self.pending.lock().map_err(|_| CoreError::Bridge)?;
        operations.retain(|_, p| p.scope.profile_id != profile_id);
        operations.insert(id.clone(), pending);
        serde_json::to_string(&json!({"id":id,"requests":requests})).map_err(|_| CoreError::Bridge)
    }

    pub fn prepared_home(&self, operation: String) -> Result<String, CoreError> {
        let plan = self
            .pending
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .get(&operation)
            .cloned()
            .ok_or(CoreError::InvalidInput)?;
        let current: ProfileConfiguration =
            serde_json::from_str(&self.profile(plan.scope.profile_id.clone())?)
                .map_err(|_| CoreError::Bridge)?;
        if current.scope.revision != plan.scope.revision {
            return Err(CoreError::Bridge);
        }
        let mut rows = Vec::new();
        if plan.cache_home {
            rows.extend(self.continue_row(&plan.scope)?);
            rows.extend(self.favorites_row(&plan.scope)?);
        }
        rows.extend(plan.rows.iter().map(|request| {
            HomeRow {
                browse: request.browse.clone(),
                id: request.id.clone(),
                title: request.title.clone(),
                provider_id: request.provider_id.clone(),
                state: if let Some(cards) = plan.cached.get(&request.id) {
                    if cards.is_empty() {
                        RowState::Empty
                    } else {
                        RowState::Ready
                    }
                } else if request.url.is_some() {
                    RowState::Pending
                } else {
                    RowState::Failed
                },
                next_cursor: row_cursor(request, plan.cached.get(&request.id).map(Vec::as_slice).unwrap_or(&[])),
                cards: plan.cached.get(&request.id).cloned().unwrap_or_default(),
                message: request
                    .url
                    .is_none()
                    .then(|| "This addon is unavailable. Try again.".into()),
            }
        }));
        for row in &mut rows {
            self.merge_cached_presentation(&plan.scope, &mut row.cards)?;
        }
        let featured = viptv_engine::home::select_featured(&rows);
        let hero = featured.first().cloned();
        let complete = rows.iter().all(|row| row.state != RowState::Pending);
        serde_json::to_string(&HomeSnapshot {
            scope: plan.scope.clone(),
            revision: self.home_revision(&plan.scope)?,
            hero,
            featured,
            rows,
            complete,
        })
        .map_err(|_| CoreError::Bridge)
    }

    pub fn project_home(&self, id: String, raw_results: String) -> Result<String, CoreError> {
        if raw_results.len() > 12 * 2 * 1024 * 1024 {
            return Err(CoreError::InvalidInput);
        }
        let values: Value =
            serde_json::from_str(&raw_results).map_err(|_| CoreError::InvalidInput)?;
        let responses = values.as_array().ok_or(CoreError::InvalidInput)?;
        let pending = self
            .pending
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .remove(&id)
            .ok_or(CoreError::InvalidInput)?;
        let mut store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        let current = store
            .profile(&pending.scope)
            .map_err(|_| CoreError::Bridge)?
            .ok_or(CoreError::Bridge)?;
        if current.scope.revision != pending.scope.revision {
            return Err(CoreError::Bridge);
        }
        let mut rows = pending
            .rows
            .iter()
            .map(|request| {
                let result = responses
                    .iter()
                    .find(|v| v["id"].as_str() == Some(&request.id));
                let mut cards = pending.cached.get(&request.id).cloned().or_else(|| {
                    result
                        .and_then(|v| v["body"].as_str())
                        .and_then(|body| stremio::cards(body, &request.provider_id).ok())
                });
                if !pending.cached.contains_key(&request.id) {
                    if store
                        .validate_metadata_origin(&pending.scope, &request.origin)
                        .is_err()
                    {
                        cards = None;
                    } else if let Some(cards) = cards.as_mut() {
                        cards.retain(origins::stremio_card_allowed);
                        for card in cards {
                            origins::stamp_card(card, &request.origin);
                        }
                    }
                } else if cards.as_ref().is_some_and(|cards| {
                    cards.iter().any(|card| {
                        card.metadata_origin.as_ref().is_some_and(|origin| {
                            store
                                .validate_metadata_origin(&pending.scope, origin)
                                .is_err()
                        })
                    })
                }) {
                    cards = None;
                }
                if pending.cache_home && !pending.cached.contains_key(&request.id) {
                    if let Some(cards) = &cards {
                        // Caching is optional: a full/read-only disk must not discard a live result.
                        let _ = store.cache_catalog(&pending.scope, &request.id, cards, now());
                    }
                }
                let state = if cards.is_none() {
                    RowState::Failed
                } else if cards.as_ref().is_some_and(Vec::is_empty) {
                    RowState::Empty
                } else {
                    RowState::Ready
                };
                let cursor = row_cursor(request, cards.as_deref().unwrap_or(&[]));
                HomeRow {
                    browse: request.browse.clone(),
                    id: request.id.clone(),
                    title: request.title.clone(),
                    provider_id: request.provider_id.clone(),
                    state: state.clone(),
                    cards: cards.unwrap_or_default(),
                    next_cursor: cursor,
                    message: if state == RowState::Failed {
                        Some("This addon is unavailable. Try again.".into())
                    } else {
                        None
                    },
                }
            })
            .collect::<Vec<_>>();
        drop(store);
        for row in &mut rows {
            self.merge_cached_presentation(&pending.scope, &mut row.cards)?;
        }
        let featured = viptv_engine::home::select_featured(&rows);
        let hero = featured.first().cloned();
        if pending.cache_home {
            if let Some(favorites) = self.favorites_row(&pending.scope)? {
                rows.insert(0, favorites);
            }
            if let Some(queue) = self.continue_row(&pending.scope)? {
                rows.insert(0, queue);
            }
        }
        serde_json::to_string(&HomeSnapshot {
            scope: pending.scope.clone(),
            revision: self.home_revision(&pending.scope)?,
            hero,
            featured,
            rows,
            complete: true,
        })
        .map_err(|_| CoreError::Bridge)
    }
}
