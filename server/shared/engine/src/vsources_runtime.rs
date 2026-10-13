//! Per-resolution SDK execution. Scope checks and network budgets belong to the host.
use crate::{
    Error,
    identity::{MappingEvidence, MediaIdentity},
    source_integration::VsourcesConfiguration,
};
use async_trait::async_trait;
use futures::StreamExt;
use std::{
    collections::BTreeMap,
    net::IpAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use url::{Host, Url};
use vsources::error::{BlockedReason, FetchError};
use vsources::traits::{FetchRequest, FetchResponse, Fetcher, ProbeResponse};
use vsources::{MediaId, MediaRef, MediaType};

/// Canonical metadata IDs are authoritative. Opaque title IDs and title text are never parsed.
pub fn media_ref(
    identity: &MediaIdentity,
    parent: Option<&MediaIdentity>,
    evidence: &[MappingEvidence],
) -> Result<MediaRef, Error> {
    let episodic = identity.episode.is_some();
    let title = if episodic {
        parent.ok_or(Error::Unresolved)?
    } else {
        identity
    };
    let kind = match title.kind.as_str() {
        "movie" if !episodic => MediaType::Movie,
        "series" => MediaType::Series,
        _ => return Err(Error::Unresolved),
    };
    if kind == MediaType::Series && !episodic {
        return Err(Error::Unresolved);
    }
    let has_tmdb = title.ids.iter().any(|id| id.namespace == "tmdb")
        || evidence.iter().any(|e| {
            e.target.namespace == "tmdb"
                && e.confidence == crate::identity::MappingConfidence::Explicit
        });
    let id = match crate::identity::resolve(title, "tmdb", evidence) {
        Ok(id) => MediaId::Tmdb(
            id.value
                .parse::<u64>()
                .ok()
                .filter(|id| *id > 0)
                .ok_or(Error::Unresolved)?,
        ),
        Err(_) if has_tmdb => return Err(Error::Unresolved),
        Err(_) => {
            let id = crate::identity::resolve(title, "imdb", evidence)?;
            if !id.value.starts_with("tt")
                || id.value.len() < 4
                || !id.value[2..].bytes().all(|b| b.is_ascii_digit())
            {
                return Err(Error::Unresolved);
            }
            MediaId::Imdb(id.value)
        }
    };
    let (season, episode) = if let Some(ep) = &identity.episode {
        if ep.order != "aired" || ep.episode == 0 {
            return Err(Error::Unresolved);
        }
        (Some(ep.season.ok_or(Error::Unresolved)?), Some(ep.episode))
    } else {
        (None, None)
    };
    Ok(MediaRef {
        id,
        kind,
        season,
        episode,
    })
}

pub fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_multicast()
                && !ip.is_broadcast()
                && !ip.is_unspecified()
                && a != 0
                && a < 224
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 192 && b == 0 && (c == 0 || c == 2))
                && !(a == 192 && b == 88 && c == 99)
                && !(a == 198 && (b == 18 || b == 19))
                && !(a == 198 && b == 51 && c == 100)
                && !(a == 203 && b == 0 && c == 113)
        }
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return public_ip(v4.into());
            }
            let w = ip.segments();
            (w[0] & 0xe000) == 0x2000
                && w[0] != 0x2002
                && !(w[0] == 0x2001 && (w[1] < 0x200 || w[1] == 0xdb8))
        }
    }
}
pub fn public_url(url: &Url) -> bool {
    let h = url
        .host_str()
        .unwrap_or_default()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    url.as_str().len() <= 16384
        && matches!(url.scheme(), "http" | "https")
        && !h.is_empty()
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && url.port_or_known_default() != Some(0)
        && h != "localhost"
        && !h.ends_with(".localhost")
        && !h.ends_with(".local")
        && !h.ends_with(".internal")
        && !matches!(url.host(),Some(Host::Ipv4(ip)) if !public_ip(ip.into()))
        && !matches!(url.host(),Some(Host::Ipv6(ip)) if !public_ip(ip.into()))
}
/// Each engine is scoped to a single job; SDK caches cannot cross accounts or revisions.
pub struct ScopedFetcher {
    authorize: Arc<dyn Fn() -> bool + Send + Sync>,
    requests: AtomicUsize,
    permits: tokio::sync::Semaphore,
    deadline: tokio::time::Instant,
}
impl ScopedFetcher {
    pub fn new(authorize: Arc<dyn Fn() -> bool + Send + Sync>) -> Self {
        Self {
            authorize,
            requests: AtomicUsize::new(0),
            permits: tokio::sync::Semaphore::new(4),
            deadline: tokio::time::Instant::now() + Duration::from_secs(45),
        }
    }
    fn check(&self, url: &Url) -> Result<(), FetchError> {
        if !(self.authorize)() || tokio::time::Instant::now() >= self.deadline || !public_url(url) {
            Err(FetchError::Blocked {
                url: url.clone(),
                reason: BlockedReason::Unknown,
            })
        } else {
            Ok(())
        }
    }
    async fn fetch(
        &self,
        mut request: FetchRequest,
        limit: usize,
        prefix: bool,
    ) -> Result<ProbeResponse, FetchError> {
        self.check(&request.url)?;
        let denied = || FetchError::Blocked {
            url: request.url.clone(),
            reason: BlockedReason::Unknown,
        };
        if !matches!(request.method.as_str(), "GET" | "POST" | "HEAD")
            || request
                .binary_body
                .as_ref()
                .map(Vec::len)
                .unwrap_or_else(|| request.body.as_ref().map(String::len).unwrap_or(0))
                > 1024 * 1024
            || request.headers.len() > 32
            || request.headers.iter().any(|(k, v)| {
                k.len() > 128
                    || v.len() > 8192
                    || k.eq_ignore_ascii_case("host")
                    || k.eq_ignore_ascii_case("content-length")
                    || k.eq_ignore_ascii_case("transfer-encoding")
            })
        {
            return Err(denied());
        }
        if self.requests.fetch_add(1, Ordering::Relaxed) >= 96 {
            return Err(denied());
        }
        let _permit = self.permits.acquire().await.map_err(|_| denied())?;
        let redirects = request.max_redirects.unwrap_or(8).min(8);
        for hop in 0..=redirects {
            self.check(&request.url)?;
            let err = || FetchError::Transport {
                url: request.url.clone(),
                message: "Source request unavailable".into(),
            };
            let host = request.url.host_str().ok_or_else(err)?;
            let addresses: Vec<_> = tokio::net::lookup_host((
                host,
                request.url.port_or_known_default().ok_or_else(err)?,
            ))
            .await
            .map_err(|_| err())?
            .take(17)
            .collect();
            if addresses.is_empty()
                || addresses.len() > 16
                || addresses.iter().any(|a| !public_ip(a.ip()))
            {
                return Err(FetchError::Blocked {
                    url: request.url.clone(),
                    reason: BlockedReason::Unknown,
                });
            }
            let client = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .resolve_to_addrs(host, &addresses)
                .connect_timeout(Duration::from_secs(3))
                .timeout(
                    request
                        .timeout
                        .unwrap_or(Duration::from_secs(8))
                        .min(Duration::from_secs(10)),
                )
                .build()
                .map_err(|_| err())?;
            let method =
                reqwest::Method::from_bytes(request.method.as_bytes()).map_err(|_| err())?;
            let mut outgoing = client.request(method, request.url.clone());
            for (k, v) in &request.headers {
                outgoing = outgoing.header(k, v);
            }
            if let Some(body) = &request.binary_body {
                outgoing = outgoing.body(body.clone());
            } else if let Some(body) = &request.body {
                outgoing = outgoing.body(body.clone());
            }
            let response = outgoing.send().await.map_err(|_| err())?;
            self.check(&request.url)?;
            let status = response.status().as_u16();
            let headers: BTreeMap<_, _> = response
                .headers()
                .iter()
                .filter_map(|(k, v)| {
                    v.to_str()
                        .ok()
                        .map(|v| (k.as_str().to_ascii_lowercase(), v.to_string()))
                })
                .collect();
            if matches!(status, 301 | 302 | 303 | 307 | 308) && hop < redirects {
                let next = request
                    .url
                    .join(headers.get("location").ok_or_else(err)?)
                    .map_err(|_| err())?;
                if request.url.scheme() == "https" && next.scheme() != "https" {
                    return Err(err());
                }
                if request.url.origin() != next.origin() {
                    // A redirect recipient never inherits declared credentials or POST data.
                    if request.body.is_some() || request.binary_body.is_some() {
                        return Err(err());
                    }
                    request.headers.clear();
                }
                if status == 303 || ((status == 301 || status == 302) && request.method == "POST") {
                    request.method = "GET".into();
                    request.body = None;
                    request.binary_body = None;
                }
                request.url = next;
                continue;
            }
            let mut bytes = Vec::new();
            let mut truncated = false;
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                self.check(&request.url)?;
                let chunk = chunk.map_err(|_| err())?;
                if bytes.len() + chunk.len() > limit {
                    if !prefix {
                        return Err(err());
                    }
                    bytes.extend_from_slice(&chunk[..limit.saturating_sub(bytes.len())]);
                    truncated = true;
                    break;
                }
                bytes.extend_from_slice(&chunk);
            }
            self.check(&request.url)?;
            return Ok(ProbeResponse {
                url: request.url,
                status,
                headers,
                body: bytes,
                truncated,
            });
        }
        Err(FetchError::Transport {
            url: request.url,
            message: "Redirect budget exceeded".into(),
        })
    }
}
#[async_trait]
impl Fetcher for ScopedFetcher {
    async fn request(&self, request: FetchRequest) -> Result<FetchResponse, FetchError> {
        let url = request.url.clone();
        let response =
            tokio::time::timeout_at(self.deadline, self.fetch(request, 3 * 1024 * 1024, false))
                .await
                .map_err(|_| FetchError::Timeout { url })??;
        Ok(FetchResponse {
            url: response.url,
            status: response.status,
            headers: response.headers,
            body: String::from_utf8_lossy(&response.body).into_owned(),
        })
    }
    async fn probe(
        &self,
        request: FetchRequest,
        max_bytes: usize,
    ) -> Result<Option<ProbeResponse>, FetchError> {
        let url = request.url.clone();
        tokio::time::timeout_at(
            self.deadline,
            self.fetch(request, max_bytes.clamp(1, 8192), true),
        )
        .await
        .map_err(|_| FetchError::Timeout { url })?
        .map(Some)
    }
}
pub fn engine(
    config: &VsourcesConfiguration,
    fetcher: Arc<dyn Fetcher>,
) -> Result<vsources::Engine, Error> {
    config.validate()?;
    let providers: Vec<_> = config.providers.iter().map(String::as_str).collect();
    let engine = vsources::EngineBuilder::new()
        .fetcher(fetcher)
        .tmdb_key(config.tmdb_api_key.trim())
        .providers(&providers)
        .with_default_providers()
        .concurrency(4)
        .stream_limit(80)
        .per_source_timeout(Duration::from_secs(18))
        .build()
        .map_err(|_| Error::Unavailable)?;
    if !providers.is_empty()
        && providers
            .iter()
            .any(|id| !engine.providers().iter().any(|p| p.id == *id))
    {
        return Err(Error::InvalidQuery);
    }
    Ok(engine)
}

/// Merged SDK snapshots become a bounded sequence of new grants, never URL-bearing public events.
pub async fn resolve_progressive(
    config: VsourcesConfiguration,
    media: MediaRef,
    authorize: Arc<dyn Fn() -> bool + Send + Sync>,
    mut publish: impl FnMut(Vec<vsources::Stream>),
) -> Result<(), Error> {
    if !authorize() {
        return Err(Error::Forbidden);
    }
    let engine = engine(&config, Arc::new(ScopedFetcher::new(authorize.clone())))?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let resolve = engine.resolve_progressive(&media, tx);
    tokio::pin!(resolve);
    let deadline = tokio::time::sleep(Duration::from_secs(45));
    tokio::pin!(deadline);
    let mut seen = std::collections::HashSet::new();
    let mut guard=tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            _=guard.tick()=>{if !authorize(){return Err(Error::Forbidden);}},
            _=&mut deadline=>return Err(Error::Unavailable),
            result=&mut resolve=>{if !authorize(){return Err(Error::Forbidden);} if let Ok(streams)=result {publish_new(streams,&mut seen,&mut publish);return Ok(());} return Err(Error::Unavailable);},
            Some(streams)=rx.recv()=>{if !authorize(){return Err(Error::Forbidden);}publish_new(streams,&mut seen,&mut publish);}
        }
    }
}
fn publish_new(
    streams: Vec<vsources::Stream>,
    seen: &mut std::collections::HashSet<String>,
    publish: &mut impl FnMut(Vec<vsources::Stream>),
) {
    let mut batch = Vec::new();
    for stream in streams {
        if seen.len() >= 80 {
            break;
        }
        if stream.is_external
            || !public_url(&stream.url)
            || !stream.behavior_hints.is_empty()
            || !matches!(
                stream.format,
                vsources::types::Format::Hls | vsources::types::Format::Mp4
            )
            || stream.ttl.is_zero()
        {
            continue;
        }
        let key = serde_json::to_string(&(stream.url.as_str(), &stream.meta.request_headers))
            .unwrap_or_default();
        if seen.insert(key) {
            batch.push(stream);
        }
    }
    if !batch.is_empty() {
        publish(batch);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{EpisodeCoordinates, ExternalId};
    fn identity(kind: &str, ids: Vec<ExternalId>) -> MediaIdentity {
        MediaIdentity {
            id: "opaque".into(),
            kind: kind.into(),
            ids,
            episode: None,
            edition: None,
        }
    }
    #[test]
    fn canonical_mapping_requires_exact_parent_episode_and_rejects_private_addresses() {
        let parent = identity(
            "series",
            vec![ExternalId {
                namespace: "tmdb".into(),
                value: "1399".into(),
            }],
        );
        let mut episode = identity("series", vec![]);
        episode.episode = Some(EpisodeCoordinates {
            order: "aired".into(),
            season: Some(2),
            episode: 3,
        });
        let mapped = media_ref(&episode, Some(&parent), &[]).unwrap();
        assert_eq!(mapped.season, Some(2));
        assert_eq!(mapped.episode, Some(3));
        assert!(media_ref(&episode, None, &[]).is_err());
        episode.episode.as_mut().unwrap().order = "absolute".into();
        assert!(media_ref(&episode, Some(&parent), &[]).is_err());
        for target in [
            "http://127.0.0.1/",
            "https://[::ffff:127.0.0.1]/",
            "http://169.254.169.254/",
            "https://host.internal/",
        ] {
            assert!(!public_url(&Url::parse(target).unwrap()));
        }
    }
    #[tokio::test]
    async fn revoked_authority_never_performs_even_public_dns_lookup() {
        let fetcher = ScopedFetcher::new(Arc::new(|| false));
        assert!(
            fetcher
                .request(FetchRequest::get(
                    Url::parse("https://example.com/").unwrap()
                ))
                .await
                .is_err()
        );
    }
}

#[cfg(test)]
mod real_qualification {
    use super::*;
    #[tokio::test]
    #[ignore = "requires an explicitly injected owned TMDB credential; reads real public providers"]
    async fn owned_sdk_sources_use_scoped_fetcher_and_publish_real_streams() {
        let key =
            std::env::var("VIPTV_QA_TMDB_KEY").expect("Owned qualification credential required");
        let config = VsourcesConfiguration {
            tmdb_api_key: key,
            providers: vec![],
        };
        let media = MediaRef {
            id: MediaId::Tmdb(27205),
            kind: MediaType::Movie,
            season: None,
            episode: None,
        };
        let mut count = 0;
        let started = std::time::Instant::now();
        let result = resolve_progressive(config, media, Arc::new(|| true), |streams| {
            count += streams.len();
        })
        .await;
        // Late providers may hit the total deadline after earlier valid sources arrive.
        println!(
            "SDK qualification: sources={count}, elapsed_seconds={}, completed={}",
            started.elapsed().as_secs(),
            result.is_ok()
        );
        assert!(count > 0, "No real playable SDK source qualified");
    }
}
