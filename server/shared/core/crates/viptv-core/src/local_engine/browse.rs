use super::addons::{CatalogRequest, PendingHome};
use super::*;
use serde::Deserialize;
use serde_json::json;
use viptv_engine::{
    capabilities::Capability,
    query::{Choice, ControlKind, QueryControl},
    stremio,
};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Query {
    #[serde(rename = "type")]
    kind: String,
    catalog: Option<String>,
    connection_id: Option<String>,
    #[serde(default)]
    skip: usize,
    search: Option<String>,
    genre: Option<String>,
    #[serde(default)]
    extras: std::collections::HashMap<String, String>,
    #[serde(default)]
    values: Vec<viptv_engine::query::QueryValue>,
}
impl LocalEngine {
    fn browse_connections(
        &self,
        profile: &ProfileConfiguration,
        capability: Capability,
    ) -> Result<Vec<viptv_engine::profile::IntegrationConnection>, CoreError> {
        Ok(self
            .store
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .connections("local")
            .map_err(|_| CoreError::Bridge)?
            .into_iter()
            .filter(|c| {
                c.enabled
                    && c.plugin_id == profile.browsing_plugin
                    && profile
                        .assignments
                        .iter()
                        .any(|a| a.connection_id == c.id && a.enabled.contains(&capability))
            })
            .collect())
    }
}
#[uniffi::export]
impl LocalEngine {
    pub fn discovery_controls(&self, profile: String, kind: String) -> Result<String, CoreError> {
        if !matches!(kind.as_str(), "movie" | "series") {
            return Err(CoreError::InvalidInput);
        }
        let profile: ProfileConfiguration =
            serde_json::from_str(&self.profile(profile)?).map_err(|_| CoreError::Bridge)?;
        let discovery = self.browse_connections(&profile, Capability::Discovery)?;
        let search = self.browse_connections(&profile, Capability::Search)?;
        let mut catalogs = Vec::new();
        for connection in discovery.iter().take(8) {
            let stored = match self.addon(&connection.id) {
                Ok(v) => v,
                Err(_) => continue,
            };
            for catalog in stored.manifest["catalogs"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|c| c["type"] == kind)
                .take(64)
            {
                let Some(id) = catalog["id"]
                    .as_str()
                    .filter(|v| !v.is_empty() && v.len() <= 128)
                else {
                    continue;
                };
                let controls = stremio::catalog_controls(catalog);
                catalogs.push(json!({"id":id,"name":catalog["name"].as_str().unwrap_or(&connection.name),"type":kind,"connection_id":connection.id,"addon_id":null,"connection_name":connection.name,"supports_search":controls.iter().any(|c|c.id=="search"),"supports_skip":stremio::catalog_pageable(catalog),"controls":controls}));
            }
        }
        let controls = vec![QueryControl {
            id: "type".into(),
            label: "Type".into(),
            kind: ControlKind::SingleChoice,
            required: false,
            choices: vec![
                Choice {
                    value: "movie".into(),
                    label: "Movies".into(),
                },
                Choice {
                    value: "series".into(),
                    label: "TV Shows".into(),
                },
            ],
            minimum: None,
            maximum: None,
            maximum_selections: None,
        }];
        let state = |enabled: bool| json!({"availability":if enabled{"Available"}else{"NeedsConfiguration"},"message":if enabled{None}else{Some("Enable this capability for the selected profile")}});
        serde_json::to_string(&json!({"provider_id":profile.browsing_plugin,"media_types":["movie","series"],"controls":controls,"catalogs":catalogs,"discovery":state(!discovery.is_empty()),"search":state(!search.is_empty()),"filters":state(!catalogs.is_empty()),"calendar":{"availability":"Unsupported","message":"Calendar is not available for Stremio addons"}})).map_err(|_|CoreError::Bridge)
    }
    pub fn project_discover(
        &self,
        operation: String,
        raw_results: String,
    ) -> Result<String, CoreError> {
        if raw_results.len() > 24 * 1024 * 1024 {
            return Err(CoreError::InvalidInput);
        }
        let responses: serde_json::Value =
            serde_json::from_str(&raw_results).map_err(|_| CoreError::InvalidInput)?;
        let responses = responses.as_array().ok_or(CoreError::InvalidInput)?;
        let pending = self
            .pending
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .remove(&operation)
            .ok_or(CoreError::InvalidInput)?;
        let (skip, pageable) = pending.discover.ok_or(CoreError::InvalidInput)?;
        let store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        let current = store
            .profile(&pending.scope)
            .map_err(|_| CoreError::Bridge)?
            .ok_or(CoreError::InvalidInput)?;
        if current.scope.revision != pending.scope.revision {
            return Err(CoreError::Bridge);
        }
        let mut result = viptv_engine::presentation::CatalogPage {
            cards: Vec::new(),
            next_cursor: None,
            complete: true,
        };
        let mut seen = std::collections::HashSet::new();
        let mut success = false;
        for request in pending.rows {
            let Some(raw) = responses
                .iter()
                .find(|v| v["id"].as_str() == Some(&request.id))
                .and_then(|v| v["body"].as_str())
            else {
                continue;
            };
            let mut page = stremio::catalog_page(raw, &request.provider_id, skip, pageable)
                .map_err(|_| CoreError::InvalidInput)?;
            store
                .validate_metadata_origin(&pending.scope, &request.origin)
                .map_err(|_| CoreError::InvalidInput)?;
            let origin = request.origin.clone();
            page.cards.retain(origins::stremio_card_allowed);
            for card in &mut page.cards {
                origins::stamp_card(card, &origin);
            }
            success = true;
            result.next_cursor = page.next_cursor;
            result.complete &= page.complete;
            for card in page.cards {
                if result.cards.len() < 200
                    && seen.insert((card.identity.kind.clone(), card.identity.id.clone()))
                {
                    result.cards.push(card);
                }
            }
        }
        if !success {
            return Err(CoreError::Bridge);
        }
        serde_json::to_string(&result).map_err(|_| CoreError::Bridge)
    }
    pub fn plan_discover(&self, profile: String, query_json: String) -> Result<String, CoreError> {
        if query_json.len() > 16 * 1024 {
            return Err(CoreError::InvalidInput);
        }
        let query: Query =
            serde_json::from_str(&query_json).map_err(|_| CoreError::InvalidInput)?;
        if !matches!(query.kind.as_str(), "movie" | "series")
            || query.skip > 10_000
            || query
                .search
                .as_ref()
                .is_some_and(|v| v.chars().count() > 256)
        {
            return Err(CoreError::InvalidInput);
        }
        if !query.values.is_empty() && (query.catalog.is_none() || query.connection_id.is_none()) {
            return Err(CoreError::InvalidInput);
        }
        let profile: ProfileConfiguration =
            serde_json::from_str(&self.profile(profile)?).map_err(|_| CoreError::Bridge)?;
        let cap = if query.search.as_ref().is_some_and(|s| !s.trim().is_empty()) {
            Capability::Search
        } else {
            Capability::Discovery
        };
        let connections = self.browse_connections(&profile, cap)?;
        if query
            .connection_id
            .as_ref()
            .is_some_and(|id| !connections.iter().any(|c| c.id == *id))
        {
            return Err(CoreError::InvalidInput);
        }
        let mut rows = Vec::new();
        let mut pageable = false;
        for connection in connections
            .into_iter()
            .filter(|c| query.connection_id.as_ref().is_none_or(|id| c.id == *id))
            .take(8)
        {
            let stored = self.addon(&connection.id)?;
            let plan = stremio::discovery_plan(
                &stored.manifest_url,
                &stored.manifest,
                &stremio::CatalogQuery {
                    kind: query.kind.clone(),
                    catalog: query.catalog.clone(),
                    addon: query.connection_id.as_ref().map(|_| 1),
                    skip: query.skip,
                    search: query.search.clone(),
                    genre: query.genre.clone(),
                    extras: query.extras.clone(),
                    values: query
                        .values
                        .iter()
                        .map(|v| stremio::ExtraValues {
                            id: v.id.clone(),
                            values: v.values.clone(),
                        })
                        .collect(),
                },
            )
            .map_err(|_| CoreError::InvalidInput)?;
            pageable = plan.pageable;
            for (index, url) in plan.endpoints.into_iter().enumerate().take(12 - rows.len()) {
                rows.push(CatalogRequest {
                    origin: self
                        .store
                        .lock()
                        .map_err(|_| CoreError::Bridge)?
                        .metadata_origin(&profile.scope, &connection.id)
                        .map_err(|_| CoreError::InvalidInput)?,
                    browse: None,
                    id: format!("discover:{}:{index}", connection.id),
                    title: connection.name.clone(),
                    provider_id: connection.id.clone(),
                    url: Some(url),
                });
            }
        }
        if rows.is_empty() {
            return Err(CoreError::InvalidInput);
        }
        let id = uuid::Uuid::new_v4().to_string();
        let store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        if !store
            .profile(&profile.scope)
            .map_err(|_| CoreError::Bridge)?
            .is_some_and(|current| current.scope == profile.scope)
        {
            return Err(CoreError::InvalidInput);
        }
        let mut pending = self.pending.lock().map_err(|_| CoreError::Bridge)?;
        pending.retain(|_, p| p.scope.profile_id != profile.scope.profile_id);
        pending.insert(
            id.clone(),
            PendingHome {
                scope: profile.scope,
                rows: rows.clone(),
                cached: Default::default(),
                cache_home: false,
                discover: Some((query.skip as u32, pageable && rows.len() == 1)),
            },
        );
        serde_json::to_string(&json!({"id":id,"requests":rows})).map_err(|_| CoreError::Bridge)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    #[derive(Default)]
    struct Secrets(Mutex<std::collections::HashMap<String, String>>);
    impl SecretStore for Secrets {
        fn get(&self, key: String) -> Result<Option<String>, CoreError> {
            Ok(self.0.lock().unwrap().get(&key).cloned())
        }
        fn put(&self, key: String, value: String) -> Result<(), CoreError> {
            self.0.lock().unwrap().insert(key, value);
            Ok(())
        }
        fn remove(&self, key: String) -> Result<(), CoreError> {
            self.0.lock().unwrap().remove(&key);
            Ok(())
        }
    }
    #[test]
    fn local_browse_negotiates_manifest_options_scopes_connections_and_keeps_home_cache_separate() {
        let dir = std::env::temp_dir().join(format!("viptv-local-browse-{}", uuid::Uuid::new_v4()));
        let engine = LocalEngine::new(
            dir.join("local.sqlite").to_string_lossy().into(),
            Arc::new(Secrets::default()),
        )
        .unwrap();
        let manifest = json!({"id":"fixture","name":"Provider","version":"1","resources":["catalog"],"catalogs":[{"id":"opaque.catalog","name":"Films","type":"movie","extra":[{"name":"genre","options":["Drama","Sci-Fi / Space"]},{"name":"skip"},{"name":"orderby","isRequired":true,"options":["rating","year"]}]}]});
        let connection: Value = serde_json::from_str(
            &engine
                .install_addon(
                    "default".into(),
                    "https://addon.example/manifest.json".into(),
                    manifest.to_string(),
                )
                .unwrap(),
        )
        .unwrap();
        let controls: Value = serde_json::from_str(
            &engine
                .discovery_controls("default".into(), "movie".into())
                .unwrap(),
        )
        .unwrap();
        let catalog = &controls["catalogs"][0];
        assert_eq!(catalog["connection_id"], connection["id"]);
        assert_eq!(catalog["supports_skip"], true);
        assert!(
            catalog["controls"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["id"] == "orderby" && c["required"] == true)
        );
        let query = json!({"type":"movie","catalog":"opaque.catalog","connection_id":connection["id"],"skip":24,"genre":"Sci-Fi / Space","extras":{"orderby":"rating"}});
        let plan: Value = serde_json::from_str(
            &engine
                .plan_discover("default".into(), query.to_string())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(plan["requests"].as_array().unwrap().len(), 1);
        let url = plan["requests"][0]["url"].as_str().unwrap();
        assert!(url.contains("genre=Sci-Fi"));
        assert!(url.contains("orderby=rating"));
        assert!(url.contains("skip=24"));
        let values:Vec<_>=(0..80).map(|n|json!({"id":format!("opaque:film:{n}"),"type":"movie","name":format!("Actual film {n}")})).collect();
        let results =
            json!([{"id":plan["requests"][0]["id"],"body":json!({"metas":values}).to_string()}]);
        let projected: Value = serde_json::from_str(
            &engine
                .project_discover(plan["id"].as_str().unwrap().into(), results.to_string())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(projected["cards"].as_array().unwrap().len(), 80);
        assert_eq!(projected["cards"][0]["identity"]["id"], "opaque:film:0");
        assert_eq!(projected["next_cursor"], "104");
        assert_eq!(projected["cards"][79]["identity"]["id"], "opaque:film:79");
        let home: Value =
            serde_json::from_str(&engine.plan_home("default".into()).unwrap()).unwrap();
        assert!(
            home["requests"].as_array().unwrap().is_empty(),
            "required custom catalog must never pollute unfiltered Home cache"
        );
        let mut invalid = query.clone();
        invalid["extras"]["orderby"] = json!("unadvertised");
        assert!(
            engine
                .plan_discover("default".into(), invalid.to_string())
                .is_err()
        );
        invalid = query.clone();
        invalid["connection_id"] = json!("unassigned");
        assert!(
            engine
                .plan_discover("default".into(), invalid.to_string())
                .is_err()
        );
        engine
            .set_addon_enabled(
                "default".into(),
                connection["id"].as_str().unwrap().into(),
                false,
            )
            .unwrap();
        assert!(
            engine
                .plan_discover("default".into(), query.to_string())
                .is_err()
        );
        drop(engine);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
#[path = "browse_selection_tests.rs"]
mod selection_tests;
