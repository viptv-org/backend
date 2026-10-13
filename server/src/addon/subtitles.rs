use super::*;
use viptv_engine::stremio::{self, SubtitleExtras, SubtitleFile};

pub(crate) struct SubtitleResult {
    pub provider_id: String,
    pub name: String,
    pub files: Option<Vec<SubtitleFile>>,
}

impl Addons {
    pub(crate) async fn subtitles(
        &self,
        kind: &str,
        id: &str,
        extras: &SubtitleExtras,
    ) -> Result<Vec<SubtitleResult>, String> {
        let mut results = Vec::new();
        let mut requests = Vec::new();
        for (addon_id, base, manifest) in self.async_entries().await?.into_iter().take(12) {
            let Some(url) = stremio::subtitle_url(&base, &manifest, kind, id, extras)
                .map_err(|_| "Invalid subtitle request")?
            else {
                continue;
            };
            let index = results.len();
            results.push(SubtitleResult {
                provider_id: format!("addon:{addon_id}"),
                name: manifest["name"]
                    .as_str()
                    .unwrap_or("Stremio addon")
                    .chars()
                    .take(128)
                    .collect(),
                files: None,
            });
            let addons = self.clone();
            requests.push(async move {
                let files = addons
                    .fetch(&url, 60)
                    .await
                    .ok()
                    .and_then(|response| stremio::read_subtitles(&response.to_string()).ok());
                (index, files)
            });
        }
        let pending = stream::iter(requests).buffer_unordered(4);
        tokio::pin!(pending);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while let Ok(Some((index, files))) = tokio::time::timeout_at(deadline, pending.next()).await
        {
            results[index].files = files;
        }
        Ok(results)
    }
}
