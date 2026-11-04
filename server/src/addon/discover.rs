use super::*;

pub(super) use viptv_provider::discover::{enrich_episode_art, episode_art_url};

const PAGE_DEADLINE: Duration = Duration::from_secs(6);

pub(crate) struct AddonFailure {
    addon_id: i64,
    addon_name: String,
    error_code: &'static str,
}

pub(crate) fn addon_failures_value(failures: &[AddonFailure]) -> Value {
    Value::Array(
        failures
            .iter()
            .map(|failure| {
                json!({
                    "addon_id": failure.addon_id,
                    "addon_name": failure.addon_name,
                    "error_code": failure.error_code,
                })
            })
            .collect(),
    )
}

fn addon_failure_code(raw: &str) -> &'static str {
    crate::service_errors::addon(raw).unwrap_or("addon_unavailable")
}

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
        let mut endpoint_owners = std::collections::HashMap::new();
        for entry in &entries {
            let Ok(own) =
                viptv_provider::discover::plan_discovery(std::slice::from_ref(entry), &request)
            else {
                continue;
            };
            let name = entry.2["name"].as_str().unwrap_or("Stremio addon").to_owned();
            for endpoint in own.endpoints {
                endpoint_owners.insert(endpoint, (entry.0, name.clone()));
            }
        }
        let total = plan.endpoints.len();
        let deadline = tokio::time::Instant::now() + PAGE_DEADLINE;
        let mut requests = stream::iter(plan.endpoints.iter().cloned().enumerate().map(
            |(index, u)| {
                let this_owner = endpoint_owners.get(&u).cloned();
                let s = self.clone();
                async move { (index, this_owner, s.fetch(&u, 300).await) }
            },
        ))
        .buffer_unordered(8);
        let mut responses = Vec::new();
        let mut failures: Vec<AddonFailure> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut completed = vec![false; total];
        loop {
            let next = match tokio::time::timeout_at(deadline, requests.next()).await {
                Ok(Some(next)) => next,
                Ok(None) => break,
                Err(_) => {
                    for (index, u) in plan.endpoints.iter().enumerate() {
                        if !completed[index] {
                            if let Some((addon_id, addon_name)) = endpoint_owners.get(u) {
                                if seen.insert(*addon_id) {
                                    failures.push(AddonFailure {
                                        addon_id: *addon_id,
                                        addon_name: addon_name.clone(),
                                        error_code: "addon_timeout",
                                    });
                                }
                            }
                        }
                    }
                    break;
                }
            };
            let (index, this_owner, result) = next;
            completed[index] = true;
            match result {
                Ok(value) => responses.push((index, value)),
                Err(error) => {
                    if let Some((addon_id, addon_name)) = this_owner {
                        if seen.insert(addon_id) {
                            failures.push(AddonFailure {
                                addon_id,
                                addon_name,
                                error_code: addon_failure_code(&error),
                            });
                        }
                    }
                }
            }
        }
        responses.sort_by_key(|(index, _)| *index);
        let results = responses
            .into_iter()
            .map(|(_, value)| value)
            .collect::<Vec<_>>();
        let mut value = viptv_provider::discover::aggregate_discovery(&results, &plan, request.skip)?;
        if !failures.is_empty() {
            value["failures"] = addon_failures_value(&failures);
        }
        Ok((value, owner))
    }
    pub(crate) async fn search_results(
        &self,
        addon: i64,
        kinds: Vec<String>,
        catalog: Option<String>,
        query: String,
        budget: Duration,
        refill: bool,
    ) -> Result<(Vec<Value>, bool, bool), String> {
        let entries = std::sync::Arc::new(self.async_entries().await?);
        let mut tasks = Vec::new();
        for kind in kinds {
            let plan = viptv_provider::discover::plan_discovery(
                entries.as_ref(),
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
            let pageable = refill && plan.pageable && plan.single_catalog;
            for url in plan.endpoints {
                tasks.push((kind.clone(), url, pageable));
            }
        }
        tasks.truncate(32);
        if tasks.is_empty() {
            return Ok((vec![], true, false));
        }
        let deadline = tokio::time::Instant::now() + budget;
        let mut requests = stream::iter(tasks.into_iter().enumerate().map(
            |(index, (kind, first_url, pageable))| {
                let this = self.clone();
                let entries = entries.clone();
                let query = query.clone();
                let catalog = catalog.clone();
                async move {
                    let mut metas: Vec<Value> = Vec::new();
                    let mut seen = std::collections::HashSet::new();
                    let mut skip = 0usize;
                    let mut url = first_url;
                    let mut failed = false;
                    let mut answered = false;
                    let mut denied = false;
                    loop {
                        if metas.len() >= 200 || tokio::time::Instant::now() >= deadline {
                            if metas.is_empty() {
                                failed = true;
                            }
                            break;
                        }
                        let value = match this.fetch(&url, 300).await {
                            Ok(value) => value,
                            Err(error) => {
                                failed = true;
                                if crate::service_errors::addon(&error)
                                    == Some("addon_access_denied")
                                {
                                    denied = true;
                                }
                                break;
                            }
                        };
                        let Some(raw) = value["metas"].as_array() else {
                            failed = true;
                            break;
                        };
                        answered = true;
                        let raw_count = raw.len();
                        for meta in raw {
                            if metas.len() >= 200 {
                                break;
                            }
                            let mut meta = meta.clone();
                            if !meta.is_object() {
                                failed = true;
                                continue;
                            }
                            if meta["type"].is_null() {
                                meta["type"] = json!(kind);
                            }
                            if seen.insert((
                                meta["type"].as_str().unwrap_or("").to_owned(),
                                meta["id"].as_str().unwrap_or("").to_owned(),
                            )) {
                                metas.push(meta);
                            }
                        }
                        if !pageable {
                            break;
                        }
                        let has_more =
                            raw_count > 0 && skip.saturating_add(raw_count) <= 10000;
                        if !has_more {
                            break;
                        }
                        skip += raw_count;
                        let next = viptv_provider::discover::plan_discovery(
                            entries.as_ref(),
                            &DiscoverOptions {
                                kind: kind.clone(),
                                catalog: catalog.clone(),
                                addon: Some(addon),
                                skip,
                                search: Some(query.clone()),
                                genre: None,
                                extras: HashMap::new(),
                                values: vec![],
                            },
                        )
                        .ok()
                        .and_then(|plan| plan.endpoints.into_iter().next());
                        match next {
                            Some(next_url) => url = next_url,
                            None => break,
                        }
                    }
                    Ok::<_, String>((index, metas, answered, failed, denied))
                }
            },
        ))
        .buffer_unordered(8);
        let mut batches = Vec::new();
        let mut failed = false;
        let mut answered = false;
        let mut access_denied = false;
        loop {
            match tokio::time::timeout_at(deadline, requests.next()).await {
                Ok(Some(Ok((index, metas, a, f, d)))) => {
                    answered |= a;
                    failed |= f;
                    access_denied |= d;
                    batches.push((index, metas));
                }
                Ok(Some(Err(_))) => {
                    failed = true;
                }
                Ok(None) => break,
                Err(_) => {
                    failed = true;
                    break;
                }
            }
        }
        if answered {
            self.record_search_success(addon);
        } else if access_denied {
            self.record_search_auth_failure(addon);
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
        Ok((items, failed, access_denied))
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
        self.meta_lookup(kind, id, false, true)
            .await
            .map(|(value, issuer, _)| (value, issuer))
    }
    pub(crate) async fn meta_with_failures(
        &self,
        kind: &str,
        id: &str,
        selected: bool,
    ) -> Result<(Value, i64, Vec<AddonFailure>), String> {
        self.meta_lookup(kind, id, selected, true).await
    }
    pub(crate) async fn meta_identity(&self, kind: &str, id: &str) -> Result<Value, String> {
        self.meta_lookup(kind, id, false, false)
            .await
            .map(|(value, _, _)| value)
    }
    async fn meta_lookup(
        &self,
        kind: &str,
        id: &str,
        selected: bool,
        enrich: bool,
    ) -> Result<(Value, i64, Vec<AddonFailure>), String> {
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
                if value.is_null() {
                    return Err("Metadata not found".into());
                }
                return Ok((value.clone(), *issuer, Vec::new()));
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
            .map(|(addon, u, m)| {
                let name = m["name"].as_str().unwrap_or("Stremio addon").to_owned();
                Self::endpoint(&u, &["meta", kind, &format!("{id}.json")]).map(|url| (addon, name, url))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let total = endpoints.len();
        let owners: Vec<(i64, String)> = endpoints
            .iter()
            .map(|(addon, name, _)| (*addon, name.clone()))
            .collect();
        let mut requests = stream::iter(endpoints.into_iter().enumerate().map(
            |(index, (addon, name, u))| {
                let this = self.clone();
                async move { (index, addon, name, this.fetch(&u, 3600).await) }
            },
        ))
        .buffer_unordered(8);
        let deadline = tokio::time::Instant::now() + PAGE_DEADLINE;
        let mut resolved = vec![false; total];
        let mut valid: Vec<(usize, i64, Value)> = Vec::new();
        let mut failures: Vec<AddonFailure> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        loop {
            let next = match tokio::time::timeout_at(deadline, requests.next()).await {
                Ok(Some(next)) => next,
                Ok(None) => break,
                Err(_) => {
                    for (index, (addon, name)) in owners.iter().enumerate() {
                        if !resolved[index] {
                            if seen.insert(*addon) {
                                failures.push(AddonFailure {
                                    addon_id: *addon,
                                    addon_name: name.clone(),
                                    error_code: "addon_timeout",
                                });
                            }
                        }
                    }
                    break;
                }
            };
            let (index, issuer, name, result) = next;
            resolved[index] = true;
            match result {
                Ok(v) if v["meta"].is_object() => valid.push((index, issuer, v)),
                Ok(_) => {}
                Err(error) => {
                    if seen.insert(issuer) {
                        failures.push(AddonFailure {
                            addon_id: issuer,
                            addon_name: name,
                            error_code: addon_failure_code(&error),
                        });
                    }
                }
            }
            let best = valid.iter().min_by_key(|(index, _, _)| *index);
            if best.is_some_and(|(index, _, _)| resolved[..*index].iter().all(|&done| done)) {
                break;
            }
        }
        let primary_index = match valid.iter().map(|(index, _, _)| *index).min() {
            Some(index) => index,
            None => {
                if enrich {
                    let mut cache = self.meta_results.lock().unwrap();
                    cache.retain(|_, (expiry, _, _)| *expiry > now());
                    let expiry = now() + META_NOT_FOUND_TTL;
                    if cache.len() >= 256 {
                        let drop: Vec<_> = cache.keys().take(64).cloned().collect();
                        for key in drop {
                            cache.remove(&key);
                        }
                    }
                    cache.insert(result_key, (expiry, Value::Null, 0));
                }
                return Err("Metadata not found".into());
            }
        };
        let at = valid
            .iter()
            .position(|(index, _, _)| *index == primary_index)
            .expect("selected meta present");
        let (_, issuer, mut primary) = valid.swap_remove(at);
        if enrich && kind == "series" && primary["meta"]["videos"].is_array() {
            crate::gateway::diagnostics::timed("episode_enrich", async {
                let enrich_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
                for (_, _, other) in valid.iter() {
                    if !needs_episode_art(&primary["meta"]) {
                        break;
                    }
                    enrich_episode_art(&mut primary["meta"], &other["meta"]);
                }
                while needs_episode_art(&primary["meta"]) {
                    let Ok(Some((_, _, _, result))) =
                        tokio::time::timeout_at(enrich_deadline, requests.next()).await
                    else {
                        break;
                    };
                    if let Ok(other) = result {
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
        if enrich {
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
        Ok((primary, issuer, failures))
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
