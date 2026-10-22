//! Per-resolution SDK execution. Scope checks and network budgets belong to the host.
use crate::{
    Error,
    identity::{MappingEvidence, MediaIdentity},
    source_integration::VsourcesConfiguration,
};
use async_trait::async_trait;
use std::{sync::Arc, time::Duration};
use url::Url;
use vsources::error::FetchError;
use vsources::traits::{FetchRequest, FetchResponse, Fetcher, ProbeResponse};
use vsources::{MediaId, MediaRef, MediaType};
pub use vsources::Stream as SdkStream;

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

pub use crate::egress::{public_ip, public_url};

/// Provider requests carry a browser identity by default: plenty of sources
/// reject or empty-answer a bare library user agent before any challenge runs.
const EMULATED_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

fn decorate(request: &mut FetchRequest) {
    let has_agent = request
        .headers
        .keys()
        .any(|k| k.eq_ignore_ascii_case("user-agent"));
    if !has_agent {
        request
            .headers
            .insert("User-Agent".into(), EMULATED_USER_AGENT.into());
    }
    if !request.headers.keys().any(|k| k.eq_ignore_ascii_case("accept")) {
        request.headers.insert(
            "Accept".into(),
            "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,*/*;q=0.8".into(),
        );
    }
}

/// The SDK's browser-emulating fetcher cannot express host scoping, so the
/// chain runs here instead: scoped transport first, then challenge detection
/// and a solve-and-replay with the clearance cookies and the solving agent
/// (Cloudflare binds the pair).
pub struct SolvingFetcher {
    transport: ScopedFetcher,
    chain: Option<Arc<vsources_cloudflare::SolverChain>>,
}

impl SolvingFetcher {
    pub fn new(
        authorize: Arc<dyn Fn() -> bool + Send + Sync>,
        chain: Option<Arc<vsources_cloudflare::SolverChain>>,
    ) -> Self {
        Self {
            transport: ScopedFetcher::new(authorize),
            chain,
        }
    }
}

#[async_trait]
impl Fetcher for SolvingFetcher {
    async fn request(&self, request: FetchRequest) -> Result<FetchResponse, FetchError> {
        let mut request = request;
        decorate(&mut request);
        let response = self.transport.request(request.clone()).await?;
        let Some(chain) = &self.chain else {
            return Ok(response);
        };
        let Some(challenge) = vsources_cloudflare::detect(&response) else {
            return Ok(response);
        };
        if !challenge.is_solvable() {
            return Ok(response);
        }
        let Ok(clearance) = chain.solve(&request.url, challenge).await else {
            return Ok(response);
        };
        let mut replay = request;
        replay
            .headers
            .insert("User-Agent".into(), clearance.user_agent.clone());
        replay
            .headers
            .insert("Cookie".into(), clearance.cookie_header());
        self.transport.request(replay).await
    }
    async fn probe(
        &self,
        request: FetchRequest,
        max_bytes: usize,
    ) -> Result<Option<ProbeResponse>, FetchError> {
        self.transport.probe(request, max_bytes).await
    }
}

/// The operator-hosted FlareSolverr daemon dialed over plain local HTTP: its
/// address is operator configuration, so these calls never enter source scope.
struct DirectFetcher(reqwest::Client);

#[async_trait]
impl Fetcher for DirectFetcher {
    async fn request(&self, request: FetchRequest) -> Result<FetchResponse, FetchError> {
        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|_| FetchError::Transport { url: request.url.clone(), message: "bad method".into() })?;
        let mut outgoing = self.0.request(method, request.url.clone());
        for (k, v) in &request.headers {
            outgoing = outgoing.header(k, v);
        }
        if let Some(body) = request.binary_body {
            outgoing = outgoing.body(body);
        } else if let Some(body) = request.body {
            outgoing = outgoing.body(body);
        }
        let response = outgoing.send().await.map_err(|e| FetchError::Transport {
            url: request.url.clone(),
            message: e.to_string(),
        })?;
        let status = response.status().as_u16();
        let url = response.url().clone();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.as_str().to_string(), v.to_string())))
            .collect();
        let body = response.text().await.map_err(|e| FetchError::Transport {
            url: url.clone(),
            message: e.to_string(),
        })?;
        Ok(FetchResponse { url, status, headers, body })
    }
    async fn probe(
        &self,
        _request: FetchRequest,
        _max_bytes: usize,
    ) -> Result<Option<ProbeResponse>, FetchError> {
        Ok(None)
    }
}

/// A solver chain that clears challenges through an operator-hosted
/// FlareSolverr daemon (the lazy, server-side answer; device-local WebView
/// solvers plug into the same chain type).
pub fn flaresolverr_chain(url: Url) -> Result<vsources_cloudflare::SolverChain, Error> {
    let daemon = vsources_cloudflare::FlareSolverr::new(url, Arc::new(DirectFetcher(reqwest::Client::new())));
    Ok(vsources_cloudflare::SolverChain::new(vec![Arc::new(
        vsources_cloudflare::FlareSolverrSolver::new(daemon),
    )]))
}

pub struct ScopedFetcher(crate::egress::ScopedHttp);
impl ScopedFetcher {
    pub fn new(authorize: Arc<dyn Fn() -> bool + Send + Sync>) -> Self {
        Self(crate::egress::ScopedHttp::new(authorize))
    }
}
fn http_request(request: FetchRequest) -> crate::egress::HttpRequest {
    crate::egress::HttpRequest {
        url: request.url,
        method: request.method,
        headers: request.headers,
        body: request.body,
        binary_body: request.binary_body,
        timeout: request.timeout,
        max_redirects: request.max_redirects,
    }
}
#[async_trait]
impl Fetcher for ScopedFetcher {
    async fn request(&self, request: FetchRequest) -> Result<FetchResponse, FetchError> {
        let url = request.url.clone();
        let response =
            self.0
                .request(http_request(request))
                .await
                .map_err(|_| FetchError::Transport {
                    url,
                    message: "Scoped source request rejected".into(),
                })?;
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
        let response = self
            .0
            .probe(http_request(request), max_bytes)
            .await
            .map_err(|_| FetchError::Transport {
                url,
                message: "Scoped media probe rejected".into(),
            })?;
        Ok(Some(ProbeResponse {
            url: response.url,
            status: response.status,
            headers: response.headers,
            body: response.body,
            truncated: response.truncated,
        }))
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
    cloudflare: Option<Arc<vsources_cloudflare::SolverChain>>,
    mut publish: impl FnMut(Vec<vsources::Stream>),
) -> Result<(), Error> {
    if !authorize() {
        return Err(Error::Forbidden);
    }
    let engine = engine(
        &config,
        Arc::new(SolvingFetcher::new(authorize.clone(), cloudflare)),
    )?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let resolve = engine.resolve_progressive(&media, tx);
    tokio::pin!(resolve);
    let deadline = tokio::time::sleep(Duration::from_secs(45));
    tokio::pin!(deadline);
    let mut seen = std::collections::HashSet::new();
    let mut guard = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            _=guard.tick()=>{if !authorize(){return Err(Error::Forbidden);}},
            _=&mut deadline=>return Err(Error::Unavailable),
            result=&mut resolve=>{if !authorize(){return Err(Error::Forbidden);} if let Ok(streams)=result {publish_new(streams,&mut seen,&mut publish);return Ok(());} return Err(Error::Unavailable);},
            Some(streams)=rx.recv()=>{if !authorize(){return Err(Error::Forbidden);}publish_new(streams,&mut seen,&mut publish);}
        }
    }
}
/// Project SDK declarations without inferring codecs or containers from labels/URLs.
pub fn presentation(stream: &vsources::Stream) -> crate::stremio::SourcePresentation {
    let meta = &stream.meta;
    crate::stremio::SourcePresentation {
        provider_name: meta.source_label.clone(), source_id: meta.source_id.clone(),
        delivery_format: match stream.format { vsources::types::Format::Hls => Some("hls".into()), vsources::types::Format::Mp4 => Some("progressive".into()), _ => None },
        container: None, quality: meta.quality.clone(), video_codec: meta.codec.clone(),
        audio_codecs: meta.audio.clone(), size_bytes: meta.size, size_label: meta.size_label.clone(),
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
        let key = serde_json::to_string(&(stream.url.as_str(), &stream.meta.request_headers, &stream.meta.audio_selection))
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
    fn distinct_required_audio_selections_are_not_deduplicated_as_same_source() {
        let mut first = vsources::Stream::new(
            Url::parse("https://media.example/master.m3u8").unwrap(),
            vsources::types::Format::Hls,
        );
        first.meta.audio_selection = Some(vsources::types::AudioSelection {
            language: vsources::types::CountryCode::En, audio_index: 1,
        });
        let mut second = first.clone();
        second.meta.audio_selection.as_mut().unwrap().audio_index = 2;
        let mut seen = std::collections::HashSet::new();
        let mut output = Vec::new();
        publish_new(vec![first.clone(), second, first], &mut seen, &mut |batch| output.extend(batch));
        assert_eq!(output.len(), 2);
        assert_eq!(output[0].meta.audio_selection.as_ref().unwrap().audio_index, 1);
        assert_eq!(output[1].meta.audio_selection.as_ref().unwrap().audio_index, 2);
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
