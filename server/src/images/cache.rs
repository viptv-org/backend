use super::*;
#[derive(Clone)]
pub(super) struct Asset {
    pub bytes: Arc<[u8]>,
    pub content_type: &'static str,
}
#[derive(Default)]
struct Cached {
    at: Option<Instant>,
    value: Option<Result<Asset, ()>>,
}
#[derive(Default)]
struct Records {
    entries: HashMap<descriptor::Image, Arc<tokio::sync::Mutex<Cached>>>,
    bytes: usize,
}
pub(crate) struct Service {
    records: tokio::sync::Mutex<Records>,
    limit: tokio::sync::Semaphore,
}
impl Service {
    pub(crate) fn new() -> Self {
        Self {
            records: Default::default(),
            limit: tokio::sync::Semaphore::new(6),
        }
    }
    pub(super) async fn load(&self, image: descriptor::Image) -> Result<Asset, ApiError> {
        self.fetch(image, None).await
    }
    async fn fetch(
        &self,
        image: descriptor::Image,
        fixture: Option<url::Url>,
    ) -> Result<Asset, ApiError> {
        tokio::time::timeout(Duration::from_secs(8), self.fetch_inner(image, fixture))
            .await
            .map_err(|_| {
                ApiError(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Image request exceeded its deadline".into(),
                )
            })?
    }
    async fn fetch_inner(
        &self,
        image: descriptor::Image,
        fixture: Option<url::Url>,
    ) -> Result<Asset, ApiError> {
        let entry = {
            let mut records = self.records.lock().await;
            if !records.entries.contains_key(&image) && records.entries.len() >= 128 {
                Self::evict(&mut records, 0);
            }
            if records.entries.len() >= 128 && !records.entries.contains_key(&image) {
                return Err(ApiError(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Images are busy".into(),
                ));
            }
            records.entries.entry(image.clone()).or_default().clone()
        };
        // Both coalescing and permit waiting share the outer deadline. Local
        // backpressure must never become a cached upstream image failure.
        let mut cached = entry.lock().await;
        if let Some(value) = cached.value.as_ref().filter(|value| {
            cached.at.is_some_and(|at| {
                at.elapsed() < Duration::from_secs(if value.is_ok() { 3600 } else { 10 })
            })
        }) {
            return value
                .clone()
                .map_err(|_| ApiError(StatusCode::BAD_GATEWAY, "Image is unavailable".into()));
        }
        let previous = cached
            .value
            .as_ref()
            .and_then(|value| value.as_ref().ok())
            .map_or(0, |asset| asset.bytes.len());
        let result = async {
            let _permit = self.limit.acquire().await.map_err(|_| ())?;
            let target = fixture
                .clone()
                .unwrap_or_else(|| url::Url::parse(&image.direct()).unwrap());
            let started = Instant::now();
            let bytes = tokio::time::timeout(
                Duration::from_secs(3),
                crate::source_http::bytes(
                    target,
                    2 * 1024 * 1024,
                    Duration::from_secs(3),
                    fixture.is_some(),
                    false,
                ),
            )
            .await
            .map_err(|_| {tracing::warn!(image_folder=%image.folder,failure_class="deadline",elapsed_ms=started.elapsed().as_millis() as u64,"Public image fetch failed");()})?
            .map_err(|failure| {tracing::warn!(image_folder=%image.folder,failure_class=failure.provider_code(),elapsed_ms=started.elapsed().as_millis() as u64,"Public image fetch failed");()})?;
            let content_type = signature(&bytes).ok_or_else(|| {tracing::warn!(image_folder=%image.folder,failure_class="image_signature",elapsed_ms=started.elapsed().as_millis() as u64,"Public image fetch failed");()})?;
            Ok(Asset {
                bytes: Arc::from(bytes.as_slice()),
                content_type,
            })
        }
        .await;
        let size = result.as_ref().map_or(0, |asset: &Asset| asset.bytes.len());
        let cacheable = {
            let mut records = self.records.lock().await;
            records.bytes = records.bytes.saturating_sub(previous);
            Self::evict(&mut records, size);
            let fits = records.bytes.saturating_add(size) <= 32 * 1024 * 1024;
            if fits {
                records.bytes = records.bytes.saturating_add(size);
            }
            fits
        };
        cached.at = Some(Instant::now());
        cached.value = cacheable.then(|| result.clone());
        result.map_err(|_| ApiError(StatusCode::BAD_GATEWAY, "Image is unavailable".into()))
    }
    fn evict(records: &mut Records, needed: usize) {
        if records.entries.len() < 128 && records.bytes.saturating_add(needed) <= 32 * 1024 * 1024 {
            return;
        }
        let idle = records
            .entries
            .iter()
            .filter(|(_, entry)| Arc::strong_count(entry) == 1)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in idle {
            if let Some(entry) = records.entries.remove(&key) {
                if let Ok(cached) = entry.try_lock() {
                    records.bytes = records.bytes.saturating_sub(
                        cached
                            .value
                            .as_ref()
                            .and_then(|v| v.as_ref().ok())
                            .map_or(0, |asset| asset.bytes.len()),
                    );
                }
            }
            if records.entries.len() < 128
                && records.bytes.saturating_add(needed) <= 32 * 1024 * 1024
            {
                break;
            }
        }
    }
}
fn signature(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}
#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
