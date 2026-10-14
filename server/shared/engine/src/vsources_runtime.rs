//! Per-resolution SDK execution. Scope checks and network budgets belong to the host.
use crate::{
    Error,
    identity::{MappingEvidence, MediaIdentity},
    source_integration::VsourcesConfiguration,
};
use async_trait::async_trait;
use std::{sync::Arc, time::Duration};
#[cfg(test)]
use url::Url;
use vsources::error::FetchError;
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

pub use crate::egress::{public_ip, public_url};
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
