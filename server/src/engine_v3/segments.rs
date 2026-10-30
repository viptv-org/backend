use super::*;
use serde::Serialize;
use viptv_engine::identity::{ExternalId, MediaIdentity};
use viptv_engine::presentation::Card;

const BASE: &str = "https://api.aniskip.com";
const ENABLED_VARIABLE: &str = "VIPTV_ANISKIP";
const BASE_VARIABLE: &str = "VIPTV_ANISKIP_BASE";
const RELEASE_NAMESPACES: [&str; 4] = ["mal", "kitsu", "anilist", "anidb"];
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_SPACING: Duration = Duration::from_millis(2000);
const MAX_RESPONSE: usize = 256 * 1024;
const MAX_RESULTS: usize = 64;
const MAX_SECONDS: f64 = 86_400.0;
const MAX_CACHED: usize = 4096;
const FRESH: Duration = Duration::from_secs(24 * 60 * 60);
const NO_DATA: Duration = Duration::from_secs(60 * 60);
const BACKOFF: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Segment {
    pub(crate) kind: &'static str,
    pub(crate) start_ms: u64,
    pub(crate) end_ms: u64,
}

#[derive(Clone)]
struct Cached {
    segments: Option<Vec<Segment>>,
    expires: Instant,
}

enum Fetched {
    Segments(Vec<Segment>),
    None,
    Failed,
}

pub(crate) struct Service {
    client: reqwest::Client,
    base: String,
    enabled: bool,
    cache: Mutex<HashMap<(u64, u32), Cached>>,
    next: tokio::sync::Mutex<tokio::time::Instant>,
    inflight: tokio::sync::Semaphore,
}

impl Service {
    pub(crate) fn from_env() -> Result<Self, String> {
        Self::new(
            std::env::var(BASE_VARIABLE).unwrap_or_else(|_| BASE.into()),
            std::env::var(ENABLED_VARIABLE).is_ok_and(|value| value == "1" || value == "true"),
        )
    }

    fn new(base: String, enabled: bool) -> Result<Self, String> {
        Ok(Self {
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(3))
                .timeout(REQUEST_TIMEOUT)
                .user_agent(concat!("VIPTV/", env!("CARGO_PKG_VERSION")))
                .build()
                .map_err(|_| "AniSkip HTTP client unavailable")?,
            base,
            enabled,
            cache: Mutex::new(HashMap::new()),
            next: tokio::sync::Mutex::new(tokio::time::Instant::now()),
            inflight: tokio::sync::Semaphore::new(2),
        })
    }

    #[cfg(test)]
    pub(crate) fn fixture(base: &str) -> Self {
        Self::new(base.into(), true).unwrap()
    }

    pub(crate) async fn lookup(&self, mal: u64, episode: u32) -> Vec<Segment> {
        if !self.enabled || mal == 0 || episode == 0 {
            return Vec::new();
        }
        let key = (mal, episode);
        if let Some(segments) = self.cached(key) {
            return segments;
        }
        let (segments, ttl) = match self.fetch(mal, episode).await {
            Fetched::Segments(segments) => (Some(segments), FRESH),
            Fetched::None => (None, NO_DATA),
            Fetched::Failed => (None, BACKOFF),
        };
        self.remember(key, segments.clone(), ttl);
        segments.unwrap_or_default()
    }

    fn cached(&self, key: (u64, u32)) -> Option<Vec<Segment>> {
        let cache = self.cache.lock().ok()?;
        let entry = cache.get(&key)?;
        (entry.expires > Instant::now()).then(|| entry.segments.clone().unwrap_or_default())
    }

    fn remember(&self, key: (u64, u32), segments: Option<Vec<Segment>>, ttl: Duration) {
        let Ok(mut cache) = self.cache.lock() else {
            return;
        };
        if cache.len() >= MAX_CACHED {
            let now = Instant::now();
            cache.retain(|_, entry| entry.expires > now);
        }
        if cache.len() < MAX_CACHED {
            cache.insert(
                key,
                Cached {
                    segments,
                    expires: Instant::now() + ttl,
                },
            );
        }
    }

    async fn fetch(&self, mal: u64, episode: u32) -> Fetched {
        let Ok(_permit) = self.inflight.try_acquire() else {
            return Fetched::Failed;
        };
        {
            let mut next = self.next.lock().await;
            tokio::time::sleep_until(*next).await;
            *next = tokio::time::Instant::now() + REQUEST_SPACING;
        }
        let url = format!(
            "{}/v2/skip-times/{mal}/{episode}?types[]=op&types[]=ed&types[]=recap&episodeLength=0",
            self.base.trim_end_matches('/')
        );
        let Ok(response) = self.client.get(url).send().await else {
            return Fetched::Failed;
        };
        let status = response.status().as_u16();
        let Ok(Ok(body)) = tokio::time::timeout(REQUEST_TIMEOUT, response.bytes()).await else {
            return Fetched::Failed;
        };
        if body.len() > MAX_RESPONSE {
            return Fetched::Failed;
        }
        match status {
            200 => match serde_json::from_slice::<Value>(&body) {
                Ok(document) => Fetched::Segments(parse(&document)),
                Err(_) => Fetched::Failed,
            },
            404 => Fetched::None,
            _ => Fetched::Failed,
        }
    }
}

pub(crate) fn parse(document: &Value) -> Vec<Segment> {
    let mut segments: Vec<Segment> = Vec::new();
    for result in document["results"]
        .as_array()
        .into_iter()
        .flatten()
        .take(MAX_RESULTS)
    {
        let kind = match result["skipType"].as_str() {
            Some("op") => "intro",
            Some("ed") => "credits",
            Some("recap") => "recap",
            _ => continue,
        };
        let (Some(start_ms), Some(end_ms)) = (
            milliseconds(&result["interval"]["startTime"]),
            milliseconds(&result["interval"]["endTime"]),
        ) else {
            continue;
        };
        if end_ms <= start_ms || segments.iter().any(|segment| segment.kind == kind) {
            continue;
        }
        segments.push(Segment {
            kind,
            start_ms,
            end_ms,
        });
    }
    segments.sort_by_key(|segment| segment.start_ms);
    segments
}

fn milliseconds(value: &Value) -> Option<u64> {
    let seconds = value.as_f64()?;
    (seconds.is_finite() && (0.0..=MAX_SECONDS).contains(&seconds))
        .then(|| (seconds * 1000.0).round() as u64)
}

fn release(ids: &[ExternalId]) -> Option<MediaIdentity> {
    let id = RELEASE_NAMESPACES.iter().find_map(|namespace| {
        ids.iter()
            .find(|id| id.namespace == *namespace)
            .map(|id| format!("{namespace}:{}", id.value))
    })?;
    Some(MediaIdentity {
        id,
        kind: "series".into(),
        ids: ids.to_vec(),
        episode: None,
        edition: None,
    })
}

fn mal_id(ids: &[ExternalId]) -> Option<u64> {
    ids.iter()
        .find(|id| id.namespace == "mal")
        .and_then(|id| id.value.parse().ok())
}

pub(crate) async fn target(
    app: &App,
    lease: &ResourceLease,
    card: &Card,
) -> Option<(u64, u32)> {
    let coordinates = card.identity.episode.as_ref()?;
    if coordinates.episode == 0 {
        return None;
    }
    let ids = if card.identity.ids.is_empty() {
        card.parent_identity.as_ref().map_or(&[][..], |parent| parent.ids.as_slice())
    } else {
        card.identity.ids.as_slice()
    };
    let show = super::anime_show::resolve(&app.clone().with_lease(lease.clone()), &release(ids)?).await?;
    let release_episode = show.release_episode(super::anime_show::TvdbEpisode {
        season: coordinates.season?,
        episode: coordinates.episode,
    })?;
    let mal = mal_id(ids).or_else(|| mal_id(&show.identity().ids))?;
    (release_episode.episode > 0).then_some((mal, release_episode.episode))
}

pub(crate) fn empty() -> Value {
    json!({"segments":[]})
}

pub(crate) async fn answer(app: &App, lease: &ResourceLease, card: Option<&Card>) -> Value {
    let Some(card) = card else {
        return empty();
    };
    let Some((mal, episode)) = target(app, lease, card).await else {
        return empty();
    };
    json!({"segments": app.segments.lookup(mal, episode).await})
}
