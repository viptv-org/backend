use super::addons::{CatalogRequest, PendingHome};
use super::*;
use serde_json::json;
use viptv_engine::capabilities::Capability;
use viptv_engine::stremio;

#[uniffi::export]
impl LocalEngine {
    pub fn plan_search(&self, profile_id: String, term: String) -> Result<String, CoreError> {
        let term = term.trim();
        if term.chars().count() > 256 {
            return Err(CoreError::InvalidInput);
        }
        let store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        let profile = store
            .profile(&local_scope(&profile_id))
            .map_err(|_| CoreError::Bridge)?
            .ok_or(CoreError::InvalidInput)?;
        let connections = store.connections("local").map_err(|_| CoreError::Bridge)?;
        let mut rows = Vec::new();
        if !term.is_empty() {
            let registry = viptv_engine::registry::Registry::builtin();
            let eligible = registry
                .connections_for(&profile, &connections, Capability::Search)
                .map_err(|_| CoreError::InvalidInput)?;
            for connection in eligible.into_iter().take(8) {
                let stored = match self.addon(&connection.id) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                for kind in ["movie", "series"] {
                    let endpoints =
                        stremio::search_urls(&stored.manifest_url, &stored.manifest, kind, term)
                            .unwrap_or_default();
                    for (index, url) in endpoints.into_iter().enumerate() {
                        if rows.len() >= 12 {
                            break;
                        }
                        rows.push(CatalogRequest {
                            origin: store
                                .metadata_origin(&profile.scope, &connection.id)
                                .map_err(|_| CoreError::InvalidInput)?,
                            browse: None,
                            id: format!("search:{}:{kind}:{index}", connection.id),
                            // One carousel per add-on catalog: the title names the
                            // kind so the movie and series rows read distinctly.
                            title: format!("{} · {}", connection.name, if kind == "movie" { "Movies" } else { "Series" }),
                            provider_id: connection.id.clone(),
                            url: Some(url),
                        });
                    }
                }
            }
        }
        let id = uuid::Uuid::new_v4().to_string();
        let mut operations = self.pending.lock().map_err(|_| CoreError::Bridge)?;
        operations.retain(|_, p| p.scope.profile_id != profile_id);
        operations.insert(
            id.clone(),
            PendingHome {
                scope: profile.scope,
                rows: rows.clone(),
                cached: Default::default(),
                cache_home: false,
                discover: None,
            },
        );
        serde_json::to_string(&json!({"id":id,"requests":rows})).map_err(|_| CoreError::Bridge)
    }
}
