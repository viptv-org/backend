use super::*;
use viptv_engine::{capabilities::Capability, profile::IntegrationConnection};

impl Addons {
    pub(crate) fn with_engine_ids(mut self, ids: Vec<i64>) -> Self {
        self.engine_ids = Some(Arc::new(ids.into_iter().collect()));
        self
    }
    pub(crate) fn engine_connections(
        &self,
        account_id: i64,
    ) -> Result<Vec<IntegrationConnection>, String> {
        let mut descriptors=self.clone();
        descriptors.engine_ids=None;
        descriptors.entries()?
            .into_iter()
            .map(|(id, _, manifest)| {
                let names: Vec<_> = manifest["resources"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str().or_else(|| v["name"].as_str()))
                    .collect();
                let mut capabilities = vec![];
                if names.contains(&"meta") {
                    capabilities.push(Capability::Metadata);
                }
                if names.contains(&"stream") {
                    capabilities.push(Capability::Sources);
                }
                if names.contains(&"subtitles") {
                    capabilities.push(Capability::Subtitles);
                }
                if manifest["catalogs"]
                    .as_array()
                    .is_some_and(|v| !v.is_empty())
                {
                    capabilities.push(Capability::Discovery);
                    if manifest["catalogs"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|c| catalog_extras(c).iter().any(|e| e.name == "search"))
                    {
                        capabilities.push(Capability::Search);
                    }
                }
                Ok(IntegrationConnection {
                    id: format!("addon:{id}"),
                    plugin_id: "stremio".into(),
                    account_id: account_id.to_string(),
                    private_profile_id: None,
                    name: manifest["name"]
                        .as_str()
                        .unwrap_or("Stremio addon")
                        .chars()
                        .take(256)
                        .collect(),
                    enabled: true,
                    revision: 0,
                    capabilities,
                })
            })
            .collect()
    }
}
