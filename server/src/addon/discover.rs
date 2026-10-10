use super::*;

// Discovery negotiation and aggregation now live in the shared `viptv-provider`
// crate; this module keeps the network fetch, cache, and episode-art merging.
#[cfg(test)]
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
    pub async fn discover_with_options(&self, request: DiscoverOptions) -> Result<Value,String> { self.simkl.as_ref().ok_or("SIMKL unavailable")?.discover(request,self.profile_id).await }
    pub async fn meta(&self,kind:&str,id:&str)->Result<Value,String>{self.simkl.as_ref().ok_or("SIMKL unavailable")?.meta(kind,id,self.profile_id).await}
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
