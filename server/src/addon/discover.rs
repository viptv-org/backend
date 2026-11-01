use super::*;

pub(super) use viptv_provider::discover::{enrich_episode_art, episode_art_url};

fn needs_episode_art(meta: &Value) -> bool {
    meta["videos"].as_array().is_some_and(|videos| {
        videos.iter().take(2000).any(|video| {
            let image = video["thumbnail"].as_str().unwrap_or("");
            image.is_empty() || image.contains("episodes.metahub.space")
        })
    })
}

pub(crate) fn declares_meta(manifest: &Value, kind: &str) -> bool {
    manifest["resources"].as_array().is_some_and(|resources| {
        resources.iter().any(|resource| {
            let name = resource.as_str().or_else(|| resource["name"].as_str());
            name == Some("meta")
                && resource["types"]
                    .as_array()
                    .or_else(|| manifest["types"].as_array())
                    .is_none_or(|types| types.iter().any(|value| value == kind))
        })
    })
}

impl Addons {
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
        drop(cache);
        self.meta_results
            .lock()
            .map_err(|_| "Add-on cache unavailable")?
            .retain(|key, _| key.kind != kind || key.id != id);
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
        self.meta_lookup(kind, id, false).await
    }
    pub(crate) async fn meta_from_selected_addon(
        &self,
        kind: &str,
        id: &str,
    ) -> Result<(Value, i64), String> {
        self.meta_lookup(kind, id, true).await
    }
    async fn meta_lookup(
        &self,
        kind: &str,
        id: &str,
        selected: bool,
    ) -> Result<(Value, i64), String> {
        let revision = self.revision()?;
        let scoped = match &self.engine_ids {
            None => "all".to_owned(),
            Some(ids) => {
                let mut ids: Vec<i64> = ids.iter().copied().collect();
                ids.sort_unstable();
                format!(
                    "{}",
                    ids.iter()
                        .map(|id| id.to_string())
                        .collect::<Vec<_>>()
                        .join(",")
                )
            }
        };
        let result_key = super::MetaLookupKey {
            account: self.account_id,
            revision,
            selected,
            kind: kind.to_owned(),
            id: id.to_owned(),
            scoped,
        };
        if let Some((expiry, value, issuer)) = self.meta_results.lock().unwrap().get(&result_key) {
            if *expiry > now() {
                return Ok((value.clone(), *issuer));
            }
        }
        let endpoints = self
            .async_entries()
            .await?
            .into_iter()
            .filter(|(_, _, m)| {
                if selected {
                    declares_meta(m, kind)
                } else {
                    supports(m, "meta", kind, id)
                }
            })
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
                        crate::gateway::diagnostics::timed("episode_enrich", async {
                            let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
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
                        })
                        .await;
                    }
                    {
                        let mut cache = self.meta_results.lock().unwrap();
                        cache.retain(|_, (expiry, _, _)| *expiry > now());
                        let expiry = now() + 3600;
                        if cache.len() >= 256 {
                            let drop: Vec<_> = cache.keys().take(64).cloned().collect();
                            for key in drop {
                                cache.remove(&key);
                            }
                        }
                        cache.insert(result_key, (expiry, primary.clone(), issuer));
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
