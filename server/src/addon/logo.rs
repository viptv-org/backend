//! Add-on manifest logos, fetched and checked by the server.
//!
//! A manifest's `logo` URL can carry the add-on's private configuration (the
//! same token as its manifest URL), so clients never receive it. Listings
//! carry `/api/addons/:id/logo?v=…` instead; that route re-checks ownership,
//! fetches through the pinned source egress policy and serves only raster
//! images the clients can draw. SVG is refused: no client renders it and it can
//! carry script.
use super::*;
use axum::body::Bytes;
use sha2::{Digest, Sha256};
use std::time::Instant;

/// Largest logo accepted from an add-on.
pub(crate) const LIMIT: usize = 512 * 1024;
const CACHE_BYTES: usize = 16 * 1024 * 1024;
const CACHE_ENTRIES: usize = 128;
const FRESH: Duration = Duration::from_secs(6 * 3600);
const RETRY: Duration = Duration::from_secs(300);

/// The manifest's logo when it is a public HTTP(S) address the egress policy allows.
pub(crate) fn source(manifest: &Value, fixture: bool) -> Option<url::Url> {
    let raw = manifest["logo"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 4096 && !s.chars().any(char::is_control))?;
    let url = url::Url::parse(raw).ok()?;
    crate::source_http::validate(&url, fixture).ok()?;
    Some(url)
}

/// Opaque revision of one add-on's logo: changes when the manifest's logo does.
fn version(id: i64, url: &url::Url) -> String {
    let digest = Sha256::digest(format!("viptv-addon-logo\0{id}\0{url}").as_bytes());
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// The relative path a listing exposes, only when the manifest has a usable logo.
pub(crate) fn path(id: i64, manifest: &Value, fixture: bool) -> Option<String> {
    source(manifest, fixture).map(|url| format!("/api/addons/{id}/logo?v={}", version(id, &url)))
}

#[derive(Clone)]
pub(crate) struct Asset {
    pub(crate) bytes: Bytes,
    pub(crate) content_type: &'static str,
}
struct Entry {
    at: Instant,
    asset: Option<Asset>,
}
type Key = (i64, i64, String);

/// Logos by account, add-on and logo revision. Failures are remembered briefly
/// so a broken logo is not refetched on every settings visit.
pub(crate) struct Cache {
    entries: std::sync::Mutex<HashMap<Key, Entry>>,
    flights: std::sync::Mutex<HashMap<Key, Weak<tokio::sync::Mutex<()>>>>,
    gate: Semaphore,
}
impl Cache {
    pub(crate) fn new() -> Self {
        Self {
            entries: Default::default(),
            flights: Default::default(),
            gate: Semaphore::new(4),
        }
    }
    fn cached(&self, key: &Key) -> Option<Option<Asset>> {
        let entries = self.entries.lock().unwrap();
        let entry = entries.get(key)?;
        let fresh = if entry.asset.is_some() { FRESH } else { RETRY };
        (entry.at.elapsed() < fresh).then(|| entry.asset.clone())
    }
    fn store(&self, key: Key, asset: Option<Asset>) {
        let mut entries = self.entries.lock().unwrap();
        entries.remove(&key);
        let size = asset.as_ref().map_or(0, |a| a.bytes.len());
        let mut total: usize = entries
            .values()
            .filter_map(|e| e.asset.as_ref())
            .map(|a| a.bytes.len())
            .sum();
        while entries.len() >= CACHE_ENTRIES || total + size > CACHE_BYTES {
            let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, e)| e.at)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            if let Some(removed) = entries.remove(&oldest) {
                total -= removed.asset.map_or(0, |a| a.bytes.len());
            }
        }
        entries.insert(
            key,
            Entry {
                at: Instant::now(),
                asset,
            },
        );
    }
    /// The checked logo bytes, or `None` when the add-on's logo is unusable.
    pub(crate) async fn load(
        &self,
        account: i64,
        id: i64,
        url: url::Url,
        fixture: bool,
    ) -> Option<Asset> {
        let key = (account, id, version(id, &url));
        if let Some(hit) = self.cached(&key) {
            return hit;
        }
        let flight = {
            let mut flights = self.flights.lock().unwrap();
            flights.retain(|_, f| f.strong_count() > 0);
            match flights.get(&key).and_then(Weak::upgrade) {
                Some(flight) => flight,
                None => {
                    let flight = Arc::new(tokio::sync::Mutex::new(()));
                    flights.insert(key.clone(), Arc::downgrade(&flight));
                    flight
                }
            }
        };
        let _flight = flight.lock().await;
        if let Some(hit) = self.cached(&key) {
            return hit;
        }
        let _permit = self.gate.acquire().await.ok()?;
        let asset = fetch(url, fixture).await;
        self.store(key, asset.clone());
        asset
    }
}

async fn fetch(url: url::Url, fixture: bool) -> Option<Asset> {
    let response = tokio::time::timeout(
        Duration::from_secs(8),
        crate::source_http::bytes_with_content_type(
            url,
            LIMIT,
            Duration::from_secs(5),
            fixture,
            true,
        ),
    )
    .await
    .ok()?
    .ok()?;
    if let Some(declared) = response.content_type.as_deref() {
        let declared = declared
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let image = declared.starts_with("image/") && !declared.starts_with("image/svg");
        if !image && declared != "application/octet-stream" && declared != "binary/octet-stream" {
            return None;
        }
    }
    let content_type = signature(&response.body)?;
    Some(Asset {
        bytes: Bytes::copy_from_slice(&response.body),
        content_type,
    })
}

/// The served type comes from the bytes, never from the add-on's header.
pub(crate) fn signature(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else {
        None
    }
}
