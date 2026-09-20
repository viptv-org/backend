use super::*;

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
        })
        .await
    }
    pub async fn discover_with_options(&self, request: DiscoverOptions) -> Result<Value, String> {
        let DiscoverOptions {
            kind,
            catalog,
            addon,
            skip,
            search,
            genre,
            extras: custom,
        } = request;
        if kind.is_empty()
            || kind.len() > 64
            || !kind
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        {
            return Err("Unsupported catalog type".into());
        }
        if custom.len() > 16
            || custom.iter().any(|(k, v)| {
                extra_name(&json!(k)).is_none()
                    || ["search", "genre", "skip"].contains(&k.as_str())
                    || v.chars().count() > 1024
            })
        {
            return Err("Invalid catalog options".into());
        }
        if !custom.is_empty() && (catalog.is_none() || addon.is_none()) {
            return Err("Choose a catalog for custom options".into());
        }
        let search = search
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let genre = genre
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        if genre
            .as_ref()
            .is_some_and(|value| value.chars().count() > MAX_EXTRA_OPTION)
        {
            return Err("Genre too long".into());
        }
        let aggregated = search.is_some() && catalog.is_none();
        if aggregated && skip > 0 {
            return Err("Aggregated search does not support pagination".into());
        }
        if skip > 10000 {
            return Err("Catalog skip must not exceed 10000".into());
        }
        let mut tasks = vec![];
        let mut pageable = false;
        let mut genre_unsupported = false;
        let mut genre_invalid = false;
        'addons: for (id, u, m) in self.async_entries().await? {
            if addon.is_some_and(|a| a != id) {
                continue;
            }
            for c in m["catalogs"].as_array().into_iter().flatten() {
                if c["type"] != kind {
                    continue;
                }
                let cid = c["id"].as_str().unwrap_or("");
                if cid.is_empty() || catalog.as_deref().is_some_and(|wanted| wanted != cid) {
                    continue;
                }
                let declared_extras = catalog_extras(c);
                if search.is_some() && !declared_extras.iter().any(|extra| extra.name == "search") {
                    continue;
                }
                if let Some(requested) = &genre {
                    let Some(capability) =
                        declared_extras.iter().find(|extra| extra.name == "genre")
                    else {
                        genre_unsupported = true;
                        continue;
                    };
                    if !capability.options.is_empty()
                        && !capability.options.iter().any(|option| option == requested)
                    {
                        genre_invalid = true;
                        continue;
                    }
                }
                if custom.iter().any(|(key, value)| {
                    !declared_extras.iter().any(|e| {
                        e.name == *key && (e.options.is_empty() || e.options.contains(value))
                    })
                }) {
                    return Err("Option is not advertised by the selected catalog".into());
                }
                // Every required extra must be one this request can actually supply.
                if declared_extras.iter().any(|extra| {
                    extra.required
                        && match extra.name.as_str() {
                            "skip" => false,
                            "search" => search.is_none(),
                            "genre" => genre.is_none(),
                            _ => custom.get(&extra.name).is_none_or(|v| v.trim().is_empty()),
                        }
                }) {
                    continue;
                }
                let mut extras = Vec::new();
                let supports_skip = declared_extras.iter().any(|extra| extra.name == "skip");
                if supports_skip {
                    extras.push(format!("skip={}", skip.min(10000)));
                } else if skip > 0 {
                    return Err("Selected catalog does not support pagination".into());
                }
                if let Some(s) = &search {
                    extras.push(format!(
                        "search={}",
                        url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>()
                    ));
                }
                if let Some(value) = &genre {
                    extras.push(format!(
                        "genre={}",
                        url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>()
                    ));
                }
                for (name, value) in &custom {
                    if !value.trim().is_empty() {
                        extras.push(format!(
                            "{}={}",
                            name,
                            url::form_urlencoded::byte_serialize(value.as_bytes())
                                .collect::<String>()
                        ));
                    }
                }
                let endpoint = if extras.is_empty() {
                    Self::endpoint(&u, &["catalog", &kind, &format!("{cid}.json")])?
                } else {
                    Self::extra_endpoint(&u, &kind, cid, &extras.join("&"))?
                };
                tasks.push(endpoint);
                pageable = supports_skip;
                if !aggregated || tasks.len() == 32 {
                    break 'addons;
                }
            }
        }
        if tasks.is_empty() {
            if genre_invalid {
                return Err("Genre is not one of the catalog's advertised options".into());
            }
            if genre_unsupported {
                return Err("Selected catalog does not advertise genre filtering".into());
            }
        }
        let single_catalog = !aggregated && tasks.len() == 1;
        let results = stream::iter(tasks.into_iter().take(32).map(|u| {
            let s = self.clone();
            async move { s.fetch(&u, 300).await }
        }))
        .buffered(8)
        .collect::<Vec<_>>()
        .await;
        let mut metas = vec![];
        let mut seen = HashSet::new();
        let mut success = false;
        let mut raw_count = 0;
        for v in results.into_iter().flatten() {
            let Some(raw) = v["metas"].as_array() else {
                continue;
            };
            success = true;
            raw_count += raw.len();
            for m in raw {
                if metas.len() == 200 {
                    break;
                }
                let key = format!("{}:{}", m["type"], m["id"]);
                if seen.insert(key) {
                    metas.push(m.clone());
                }
            }
        }
        if !success {
            return Err("No catalog source succeeded or matched this request".into());
        }
        // Stremio supplies no total/page-size contract: a nonempty raw page is
        // potentially followed by another page; an empty page terminates traversal.
        let has_more =
            single_catalog && pageable && raw_count > 0 && skip.saturating_add(raw_count) <= 10000;
        let next_skip = has_more.then(|| skip + raw_count);
        Ok(
            json!({"metas":metas,"has_more":has_more,"next_skip":next_skip,
            "aggregated":aggregated,"max_catalogs":if aggregated {32} else {1},"max_results":200}),
        )
    }
    pub async fn meta(&self, kind: &str, id: &str) -> Result<Value, String> {
        let endpoints = self
            .async_entries()
            .await?
            .into_iter()
            .filter(|(_, _, m)| supports(m, "meta", kind, id))
            .take(32)
            .map(|(_, u, _)| Self::endpoint(&u, &["meta", kind, &format!("{id}.json")]))
            .collect::<Result<Vec<_>, _>>()?;
        let mut results = stream::iter(endpoints.into_iter().map(|u| {
            let this = self.clone();
            async move { this.fetch(&u, 3600).await }
        }))
        .buffered(8);
        while let Some(result) = results.next().await {
            if let Ok(v) = result {
                if v["meta"].is_object() {
                    let mut primary = v;
                    if kind == "series" && primary["meta"]["videos"].is_array() {
                        // Alternate addons may have working stills where the preferred
                        // metadata uses broken generated URLs. Keep playback identities.
                        let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
                        while let Ok(Some(next)) =
                            tokio::time::timeout_at(deadline, results.next()).await
                        {
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
                    return Ok(primary);
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
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .take(100)
            .collect())
    }
}

// Only public artwork hosts are sent to the optional resizing service. Never
// forward configured addon/CDN tokens, arbitrary URLs, or local-provider images.
fn public_episode_art(value: &Value) -> Option<String> {
    let mut url = url::Url::parse(value.as_str()?).ok()?;
    if url.host_str() == Some("api.top-posters.com") {
        let fallback = url
            .query_pairs()
            .find(|(key, _)| key == "fallback_url")?
            .1
            .into_owned();
        url = url::Url::parse(&fallback).ok()?;
    }
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || !matches!(
            url.host_str(),
            Some("artworks.thetvdb.com" | "image.tmdb.org" | "episodes.metahub.space")
        )
        || url.query().is_some()
    {
        return None;
    }
    Some(url.to_string())
}
pub(super) fn episode_art_url(value: &Value) -> Option<String> {
    let original = public_episode_art(value)?;
    let mut proxy = url::Url::parse("https://wsrv.nl/").ok()?;
    proxy
        .query_pairs_mut()
        .append_pair("url", &original)
        .append_pair("w", "512")
        .append_pair("h", "288")
        .append_pair("fit", "cover")
        .append_pair("output", "jpg");
    Some(proxy.to_string())
}
pub(super) fn enrich_episode_art(primary: &mut Value, alternate: &Value) {
    let same_series = primary["id"].as_str().is_some() && primary["id"] == alternate["id"];
    if !same_series {
        return;
    }
    let Some(videos) = primary["videos"].as_array_mut() else {
        return;
    };
    let Some(other) = alternate["videos"].as_array() else {
        return;
    };
    for video in videos.iter_mut().take(2000) {
        let current = video["thumbnail"].as_str().unwrap_or("");
        if !current.is_empty() && !current.contains("episodes.metahub.space") {
            continue;
        }
        if let Some(candidate) = other.iter().take(2000).find(|candidate| {
            let dates_agree = video["released"]
                .as_str()
                .zip(candidate["released"].as_str())
                .is_some_and(|(a, b)| a.get(..10).is_some() && a.get(..10) == b.get(..10));
            candidate["id"] == video["id"]
                || (dates_agree
                    && !video["season"].is_null()
                    && !video["episode"].is_null()
                    && candidate["season"] == video["season"]
                    && candidate["episode"] == video["episode"])
        }) {
            if let Some(image) = public_episode_art(&candidate["thumbnail"]) {
                if !image.contains("episodes.metahub.space") {
                    video["thumbnail"] = json!(image);
                }
            }
        }
    }
}
