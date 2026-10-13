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
        let mut cached = tokio::time::timeout(Duration::from_secs(4), entry.lock())
            .await
            .map_err(|_| {
                ApiError(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Image request exceeded its deadline".into(),
                )
            })?;
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
            let _permit = tokio::time::timeout(Duration::from_secs(1), self.limit.acquire())
                .await
                .map_err(|_| ())?
                .map_err(|_| ())?;
            let target = fixture
                .clone()
                .unwrap_or_else(|| url::Url::parse(&image.direct()).unwrap());
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
            .map_err(|_| ())?
            .map_err(|_| ())?;
            let content_type = signature(&bytes).ok_or(())?;
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
mod tests {
    use super::*;
    #[tokio::test]
    async fn fetches_are_bounded_coalesced_cached_and_never_forward_inbound_credentials() {
        use axum::routing::get;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let reads = Arc::new(AtomicUsize::new(0));
        let count = reads.clone();
        let router = axum::Router::new()
            .route(
                "/image",
                get(move |headers: axum::http::HeaderMap| {
                    let count = count.clone();
                    async move {
                        assert!(!headers.contains_key(header::AUTHORIZATION));
                        assert!(!headers.contains_key(header::COOKIE));
                        count.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        (
                            [(header::CONTENT_TYPE, "text/html")],
                            b"RIFF\x10\0\0\0WEBPtest".to_vec(),
                        )
                    }
                }),
            )
            .route("/large", get(|| async { vec![0u8; 2 * 1024 * 1024 + 1] }))
            .route(
                "/html",
                get(|| async { b"<html>access denied</html>".to_vec() }),
            )
            .route(
                "/redirect",
                get(|| async {
                    (
                        StatusCode::FOUND,
                        [(header::LOCATION, "http://127.0.0.1/private")],
                    )
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let image = descriptor::Image::parts("episodes", "12/abcdef_w.webp").unwrap();
        let service = Service::new();
        let target = url::Url::parse(&format!("http://{address}/image")).unwrap();
        let (a, b) = tokio::join!(
            service.fetch(image.clone(), Some(target.clone())),
            service.fetch(image.clone(), Some(target))
        );
        assert_eq!(a.unwrap().content_type, "image/webp");
        assert_eq!(b.unwrap().bytes.len(), 16);
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        // An active saturated cache cannot exceed its strict byte budget.
        let full = Service::new();
        full.records.lock().await.bytes = 32 * 1024 * 1024;
        let bounded = full
            .fetch(
                descriptor::Image::parts("episodes", "budget_w.webp").unwrap(),
                Some(url::Url::parse(&format!("http://{address}/image")).unwrap()),
            )
            .await
            .unwrap();
        assert_eq!(bounded.bytes.len(), 16);
        assert_eq!(full.records.lock().await.bytes, 32 * 1024 * 1024);
        assert!(
            full.records
                .lock()
                .await
                .entries
                .values()
                .next()
                .unwrap()
                .lock()
                .await
                .value
                .is_none()
        );
        for path in ["html", "redirect", "large"] {
            let image = descriptor::Image::parts("episodes", &format!("{path}_w.webp")).unwrap();
            assert!(
                service
                    .fetch(
                        image,
                        Some(url::Url::parse(&format!("http://{address}/{path}")).unwrap())
                    )
                    .await
                    .is_err()
            );
        }
        task.abort();
    }
}
