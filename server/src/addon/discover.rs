use super::*;

// Discovery negotiation and aggregation now live in the shared `viptv-provider`
// crate; this module keeps the network fetch, cache, and episode-art merging.
pub(super) use viptv_provider::discover::{enrich_episode_art, episode_art_url};

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
        let entries = self.async_entries().await?;
        let plan = viptv_provider::discover::plan_discovery(&entries, &request)?;
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
