use super::*;

// Discovery negotiation and aggregation now live in the shared `viptv-provider`
// crate; this module keeps the network fetch, cache, and episode-art merging.
pub(super) use viptv_provider::discover::{enrich_episode_art, episode_art_url};

fn needs_episode_art(meta: &Value) -> bool {
    meta["videos"].as_array().is_some_and(|videos| {
        videos.iter().take(2000).any(|video| {
            let image = video["thumbnail"].as_str().unwrap_or("");
            image.is_empty() || image.contains("episodes.metahub.space")
        })
    })
}

impl Addons {
    /// Browse the first applicable enabled catalog in installation order, or
    /// aggregate a single bounded search across at most 32 matching catalogs.
    /// Search never paginates. Browsing advances by the raw upstream page length,
    /// not by the deduplicated/capped (200 item) response length.
    pub async fn discover(
        &self,
        kind: String,
        catalog: Option<String>,
        addon: Option<i64>,
        skip: usize,
        search: Option<String>,
        genre: Option<String>,
    ) -> Result<Value, String> {
        self.discover_with_options(DiscoverOptions {
            kind,
            catalog,
            addon,
            skip,
            search,
            genre,
            extras: HashMap::new(),
            values: vec![],
        })
        .await
    }
    pub async fn discover_with_options(&self, request: DiscoverOptions) -> Result<Value, String> {
        self.discover_with_origin(request)
            .await
            .map(|(value, _)| value)
    }
    pub(crate) async fn discover_with_origin(
        &self,
        request: DiscoverOptions,
    ) -> Result<(Value, Option<i64>), String> {
        let entries = self.async_entries().await?;
        let plan = viptv_provider::discover::plan_discovery(&entries, &request)?;
        let owners = entries
            .iter()
            .filter_map(|entry| {
                let own =
                    viptv_provider::discover::plan_discovery(std::slice::from_ref(entry), &request)
                        .ok()?;
                own.endpoints
                    .iter()
                    .any(|endpoint| plan.endpoints.contains(endpoint))
                    .then_some(entry.0)
            })
            .collect::<std::collections::BTreeSet<_>>();
        let owner = (owners.len() == 1).then(|| *owners.first().expect("one issuer"));
        let results: Vec<Value> = stream::iter(plan.endpoints.iter().cloned().map(|u| {
            let s = self.clone();
            async move { s.fetch(&u, 300).await }
        }))
        .buffered(8)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .flatten()
        .collect();
        viptv_provider::discover::aggregate_discovery(&results, &plan, request.skip)
            .map(|value| (value, owner))
    }
    /// Grouped search retains successes and failures by endpoint until one
    /// absolute deadline. Unlike legacy aggregation, a slow sibling catalog
    /// cannot erase cards that already arrived from this add-on. `catalog`
    /// restricts the search to a single catalog so each searchable catalog of
    /// the add-on becomes its own result row.
    pub(crate) async fn search_results(
        &self,
        addon: i64,
        kinds: Vec<String>,
        catalog: Option<String>,
        query: String,
        budget: Duration,
    ) -> Result<(Vec<Value>, bool), String> {
        let entries = self.async_entries().await?;
        let mut endpoints = Vec::new();
        for kind in kinds {
            let plan = viptv_provider::discover::plan_discovery(
                &entries,
                &DiscoverOptions {
                    kind: kind.clone(),
                    catalog: catalog.clone(),
                    addon: Some(addon),
                    skip: 0,
                    search: Some(query.clone()),
                    genre: None,
                    extras: HashMap::new(),
                    values: vec![],
                },
            )?;
            endpoints.extend(plan.endpoints.into_iter().map(|url| (kind.clone(), url)));
        }
        endpoints.truncate(32);
        if endpoints.is_empty() {
            return Ok((vec![], true));
        }
        let mut requests = stream::iter(endpoints.into_iter().enumerate().map(
            |(index, (kind, url))| {
                let this = self.clone();
                async move { (index, kind, this.fetch(&url, 300).await) }
            },
        ))
        .buffer_unordered(8);
        let deadline = tokio::time::Instant::now() + budget;
        let mut batches = Vec::new();
        let mut failed = false;
        loop {
            match tokio::time::timeout_at(deadline, requests.next()).await {
                Ok(Some((index, kind, Ok(value)))) => {
                    if let Some(metas) = value["metas"].as_array() {
                        let metas = metas
                            .iter()
                            .take(200)
                            .cloned()
                            .filter_map(|mut meta| {
                                if !meta.is_object() {
                                    failed = true;
                                    return None;
                                }
                                if meta["type"].is_null() {
                                    meta["type"] = json!(kind);
                                }
                                Some(meta)
                            })
                            .collect::<Vec<_>>();
                        batches.push((index, metas));
                    } else {
                        failed = true;
                    }
                }
                Ok(Some((_, _, Err(_)))) => failed = true,
                Ok(None) => break,
                Err(_) => {
                    failed = true;
                    break;
                }
            }
        }
        batches.sort_by_key(|(index, _)| *index);
        let mut seen = std::collections::HashSet::new();
        let items = batches
            .into_iter()
            .flat_map(|(_, metas)| metas)
            .filter(|meta| {
                seen.insert((
                    meta["type"].as_str().unwrap_or("").to_owned(),
                    meta["id"].as_str().unwrap_or("").to_owned(),
                ))
            })
            .take(200)
            .collect();
        Ok((items, failed))
    }

    /// Drops this title's cached `meta` responses, so the next `meta` asks the
    /// add-ons again (a known air date passed; the cache may predate it).
    pub(crate) async fn forget_meta(&self, kind: &str, id: &str) -> Result<(), String> {
        let entries = self.async_entries().await?;
        let mut cache = self.cache.lock().map_err(|_| "Add-on cache unavailable")?;
        for (_, base, manifest) in entries {
            if !supports(&manifest, "meta", kind, id) {
                continue;
            }
            if let Ok(key) = Self::endpoint(&base, &["meta", kind, &format!("{id}.json")])
                .and_then(|url| self.cache_key(&url))
            {
                cache.remove(&key);
            }
        }
        Ok(())
    }
    pub async fn meta(&self, kind: &str, id: &str) -> Result<Value, String> {
        self.meta_with_origin(kind, id)
            .await
            .map(|(value, _)| value)
    }
    pub(crate) async fn meta_with_origin(
        &self,
        kind: &str,
        id: &str,
    ) -> Result<(Value, i64), String> {
        let endpoints = self
            .async_entries()
            .await?
            .into_iter()
            .filter(|(_, _, m)| supports(m, "meta", kind, id))
            .take(32)
            .map(|(addon, u, _)| {
                Self::endpoint(&u, &["meta", kind, &format!("{id}.json")]).map(|url| (addon, url))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut results = stream::iter(endpoints.into_iter().map(|(addon, u)| {
            let this = self.clone();
            async move { (addon, this.fetch(&u, 3600).await) }
        }))
        .buffered(8);
        while let Some((issuer, result)) = results.next().await {
            if let Ok(v) = result {
                if v["meta"].is_object() {
                    let mut primary = v;
                    if kind == "series" && primary["meta"]["videos"].is_array() {
                        // Alternate addons may have working stills where the preferred
                        // metadata uses broken generated URLs. Keep playback identities.
                        let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
                        while needs_episode_art(&primary["meta"]) {
                            let Ok(Some((_, next))) =
                                tokio::time::timeout_at(deadline, results.next()).await
                            else {
                                break;
                            };
                            if let Ok(other) = next {
                                enrich_episode_art(&mut primary["meta"], &other["meta"]);
                            }
                        }
                        if let Some(videos) = primary["meta"]["videos"].as_array_mut() {
                            for video in videos.iter_mut().take(2000) {
                                if let Some(image) = episode_art_url(&video["thumbnail"]) {
                                    video["thumbnail"] = json!(image);
                                }
                            }
                        }
                    }
                    return Ok((primary, issuer));
                }
            }
        }
        Err("Metadata not found".into())
    }
    pub async fn streams(&self, u: &str, kind: &str, id: &str) -> Result<Vec<Value>, String> {
        let endpoint = Self::endpoint(u, &["stream", kind, &format!("{id}.json")])?;
        let v = self.fetch(&endpoint, 60).await?;
        Ok(v["streams"]
            .as_array()
            .ok_or("Invalid addon stream response")?
            .iter()
            .take(100)
            .cloned()
            .collect())
    }
}
