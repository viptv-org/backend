use super::*;
use serde::Serialize;
use viptv_engine::identity::{ExternalId, MediaIdentity};
use viptv_engine::presentation::Card;

const ANISKIP_BASE: &str = "https://api.aniskip.com";
const ANIMESKIP_BASE: &str = "https://api.anime-skip.com/graphql";
pub(crate) const ANIMESKIP_CLIENT_ID: &str = "ZGfO0sMF3eCwLYf8yMSCJjlynwNGRXWE";
const THEINTRODB_BASE: &str = "https://api.theintrodb.org/v4";
const SKIPDB_BASE: &str = "https://skipdb.tv";
const ANIMESKIP_SHOWS_QUERY: &str = "query Shows($serviceId: String!) { findShowsByExternalId(service: ANILIST, serviceId: $serviceId) { id deletedAt } }";
const ANIMESKIP_EPISODES_QUERY: &str = "query Episodes($showId: ID!) { findEpisodesByShowId(showId: $showId) { season number absoluteNumber deletedAt timestamps { at type { name } } } }";
const RELEASE_NAMESPACES: [&str; 4] = ["mal", "kitsu", "anilist", "anidb"];
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const ANISKIP_SPACING: Duration = Duration::from_millis(2000);
const ANIMESKIP_SPACING: Duration = Duration::from_millis(1500);
const THEINTRODB_SPACING: Duration = Duration::from_millis(1000);
const SKIPDB_SPACING: Duration = Duration::from_millis(600);
const MAX_RESPONSE: usize = 256 * 1024;
const MAX_GRAPHQL_RESPONSE: usize = 1024 * 1024;
const MAX_RESULTS: usize = 64;
const MAX_SECONDS: f64 = 86_400.0;
const MAX_MILLISECONDS: f64 = 86_400_000.0;
const MAX_INFERRED_MS: u64 = 600_000;
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
    Document(Value),
    None,
    Failed,
}

pub(crate) struct Target {
    pub(crate) season: Option<u32>,
    pub(crate) episode: u32,
    pub(crate) mal: Option<u64>,
    pub(crate) anilist: Option<u64>,
    pub(crate) release_episode: Option<u32>,
    pub(crate) tvdb: Option<u64>,
    pub(crate) tmdb: Option<u64>,
    pub(crate) imdb: Option<String>,
}

struct Adapter {
    enabled: bool,
    base: String,
    client_id: String,
    cache: Mutex<HashMap<String, Cached>>,
    next: tokio::sync::Mutex<tokio::time::Instant>,
    inflight: tokio::sync::Semaphore,
    spacing: Duration,
}

impl Adapter {
    fn new(enabled: bool, base: String, client_id: String, spacing: Duration) -> Self {
        Self {
            enabled,
            base,
            client_id,
            cache: Mutex::new(HashMap::new()),
            next: tokio::sync::Mutex::new(tokio::time::Instant::now()),
            inflight: tokio::sync::Semaphore::new(2),
            spacing,
        }
    }

    fn cached(&self, key: &str) -> Option<Vec<Segment>> {
        let cache = self.cache.lock().ok()?;
        let entry = cache.get(key)?;
        (entry.expires > Instant::now()).then(|| entry.segments.clone().unwrap_or_default())
    }

    fn remember(&self, key: String, segments: Option<Vec<Segment>>, ttl: Duration) {
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

    async fn fetch(
        &self,
        client: &reqwest::Client,
        key: &str,
        url: String,
        parse: fn(&Value) -> Vec<Segment>,
    ) -> Vec<Segment> {
        if !self.enabled {
            return Vec::new();
        }
        if let Some(segments) = self.cached(key) {
            return segments;
        }
        let (segments, ttl) = match self.exchange(client.get(url), MAX_RESPONSE).await {
            Fetched::Document(document) => (Some(parse(&document)), FRESH),
            Fetched::None => (None, NO_DATA),
            Fetched::Failed => (None, BACKOFF),
        };
        self.remember(key.to_owned(), segments.clone(), ttl);
        segments.unwrap_or_default()
    }

    async fn graphql(&self, client: &reqwest::Client, query: &str, variables: Value) -> Fetched {
        if !self.enabled {
            return Fetched::Failed;
        }
        let request = client
            .post(self.base.trim_end_matches('/'))
            .header("X-Client-ID", self.client_id.as_str())
            .json(&json!({"query": query, "variables": variables}));
        self.exchange(request, MAX_GRAPHQL_RESPONSE).await
    }

    async fn exchange(&self, request: reqwest::RequestBuilder, max_response: usize) -> Fetched {
        let Ok(_permit) = self.inflight.try_acquire() else {
            return Fetched::Failed;
        };
        {
            let mut next = self.next.lock().await;
            tokio::time::sleep_until(*next).await;
            *next = tokio::time::Instant::now() + self.spacing;
        }
        let Ok(response) = request.send().await else {
            return Fetched::Failed;
        };
        let status = response.status().as_u16();
        let Ok(Ok(body)) = tokio::time::timeout(REQUEST_TIMEOUT, response.bytes()).await else {
            return Fetched::Failed;
        };
        if body.len() > max_response {
            return Fetched::Failed;
        }
        match status {
            200 => match serde_json::from_slice::<Value>(&body) {
                Ok(document) => Fetched::Document(document),
                Err(_) => Fetched::Failed,
            },
            404 => Fetched::None,
            _ => Fetched::Failed,
        }
    }
}

pub(crate) struct Service {
    client: reqwest::Client,
    aniskip: Adapter,
    animeskip: Adapter,
    theintrodb: Adapter,
    skipdb: Adapter,
}

impl Service {
    pub(crate) fn from_env() -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(REQUEST_TIMEOUT)
            .user_agent(concat!("VIPTV/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| "Skip segment HTTP client unavailable")?;
        let animeskip_client_id = std::env::var("VIPTV_ANIMESKIP_CLIENT_ID")
            .unwrap_or_else(|_| ANIMESKIP_CLIENT_ID.into());
        let animeskip_client_id = animeskip_client_id.trim().to_owned();
        Ok(Self {
            client,
            aniskip: Adapter::new(flag("VIPTV_ANISKIP"), var_or("VIPTV_ANISKIP_BASE", ANISKIP_BASE), String::new(), ANISKIP_SPACING),
            animeskip: Adapter::new(
                flag("VIPTV_ANIMESKIP") && !animeskip_client_id.is_empty(),
                var_or("VIPTV_ANIMESKIP_BASE", ANIMESKIP_BASE),
                animeskip_client_id,
                ANIMESKIP_SPACING,
            ),
            theintrodb: Adapter::new(flag("VIPTV_THEINTRODB"), var_or("VIPTV_THEINTRODB_BASE", THEINTRODB_BASE), String::new(), THEINTRODB_SPACING),
            skipdb: Adapter::new(flag("VIPTV_SKIPDB"), var_or("VIPTV_SKIPDB_BASE", SKIPDB_BASE), String::new(), SKIPDB_SPACING),
        })
    }

    #[cfg(test)]
    pub(crate) fn fixture(aniskip: Option<&str>, animeskip: Option<&str>, theintrodb: Option<&str>, skipdb: Option<&str>) -> Self {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(REQUEST_TIMEOUT)
            .user_agent(concat!("VIPTV/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap();
        Self {
            client,
            aniskip: Adapter::new(aniskip.is_some(), aniskip.unwrap_or_default().into(), String::new(), ANISKIP_SPACING),
            animeskip: Adapter::new(animeskip.is_some(), animeskip.unwrap_or_default().into(), ANIMESKIP_CLIENT_ID.into(), ANIMESKIP_SPACING),
            theintrodb: Adapter::new(theintrodb.is_some(), theintrodb.unwrap_or_default().into(), String::new(), THEINTRODB_SPACING),
            skipdb: Adapter::new(skipdb.is_some(), skipdb.unwrap_or_default().into(), String::new(), SKIPDB_SPACING),
        }
    }

    pub(crate) async fn lookup(&self, target: &Target) -> Vec<Segment> {
        let (aniskip, animeskip, theintrodb, skipdb) = tokio::join!(
            self.aniskip_lookup(target),
            self.animeskip_lookup(target),
            self.theintrodb_lookup(target),
            self.skipdb_lookup(target)
        );
        let mut merged: Vec<Segment> = Vec::new();
        self.merge(&mut merged, aniskip);
        self.merge(&mut merged, animeskip);
        self.merge(&mut merged, theintrodb);
        self.merge(&mut merged, skipdb);
        merged.sort_by_key(|segment| segment.start_ms);
        merged
    }

    fn merge(&self, merged: &mut Vec<Segment>, incoming: Vec<Segment>) {
        for segment in incoming {
            if !merged.iter().any(|existing| existing.kind == segment.kind) {
                merged.push(segment);
            }
        }
    }

    async fn aniskip_lookup(&self, target: &Target) -> Vec<Segment> {
        let (Some(mal), Some(episode)) = (target.mal, target.release_episode) else {
            return Vec::new();
        };
        if mal == 0 || episode == 0 {
            return Vec::new();
        }
        let url = format!(
            "{}/v2/skip-times/{mal}/{episode}?types[]=op&types[]=ed&types[]=recap&episodeLength=0",
            self.aniskip.base.trim_end_matches('/')
        );
        self.aniskip
            .fetch(&self.client, &format!("{mal}/{episode}"), url, parse)
            .await
    }

    async fn animeskip_lookup(&self, target: &Target) -> Vec<Segment> {
        if !self.animeskip.enabled {
            return Vec::new();
        }
        let Some(anilist) = target.anilist else {
            return Vec::new();
        };
        let key = format!(
            "animeskip:{anilist}:{}:{}:{}",
            target.season.map(|season| season.to_string()).unwrap_or_default(),
            target.episode,
            target.release_episode.map(|episode| episode.to_string()).unwrap_or_default()
        );
        if let Some(segments) = self.animeskip.cached(&key) {
            return segments;
        }
        let (segments, ttl) = self.animeskip_episode(anilist, target).await;
        self.animeskip.remember(key, segments.clone(), ttl);
        segments.unwrap_or_default()
    }

    async fn animeskip_episode(&self, anilist: u64, target: &Target) -> (Option<Vec<Segment>>, Duration) {
        let shows = match self
            .animeskip
            .graphql(
                &self.client,
                ANIMESKIP_SHOWS_QUERY,
                json!({"serviceId": anilist.to_string()}),
            )
            .await
        {
            Fetched::Document(document) => document,
            Fetched::None => return (None, NO_DATA),
            Fetched::Failed => return (None, BACKOFF),
        };
        let Some(show) = animeskip_show(&shows) else {
            return (None, NO_DATA);
        };
        let episodes = match self
            .animeskip
            .graphql(&self.client, ANIMESKIP_EPISODES_QUERY, json!({"showId": show}))
            .await
        {
            Fetched::Document(document) => document,
            Fetched::None => return (None, NO_DATA),
            Fetched::Failed => return (None, BACKOFF),
        };
        match animeskip_segments(&episodes, target) {
            Some(segments) => (Some(segments), FRESH),
            None => (None, NO_DATA),
        }
    }

    async fn theintrodb_lookup(&self, target: &Target) -> Vec<Segment> {
        let Some(season) = target.season else {
            return Vec::new();
        };
        let (field, value, key) = if let Some(tvdb) = target.tvdb {
            ("tvdb_id", tvdb.to_string(), format!("tvdb:{tvdb}"))
        } else if let Some(tmdb) = target.tmdb {
            ("tmdb_id", tmdb.to_string(), format!("tmdb:{tmdb}"))
        } else if let Some(imdb) = &target.imdb {
            ("imdb_id", imdb.clone(), format!("imdb:{imdb}"))
        } else {
            return Vec::new();
        };
        let url = format!(
            "{}/media?{field}={value}&season={season}&episode={}",
            self.theintrodb.base.trim_end_matches('/'),
            target.episode
        );
        self.theintrodb
            .fetch(
                &self.client,
                &format!("theintrodb:{key}:{season}:{}", target.episode),
                url,
                parse_theintrodb,
            )
            .await
    }

    async fn skipdb_lookup(&self, target: &Target) -> Vec<Segment> {
        let (Some(season), Some(imdb)) = (target.season, target.imdb.as_ref()) else {
            return Vec::new();
        };
        let url = format!(
            "{}/api/segments?imdb_id={imdb}&season={season}&episode={}",
            self.skipdb.base.trim_end_matches('/'),
            target.episode
        );
        self.skipdb
            .fetch(
                &self.client,
                &format!("skipdb:{imdb}:{season}:{}", target.episode),
                url,
                parse_skipdb,
            )
            .await
    }
}

fn flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| value == "1" || value == "true")
}

fn var_or(name: &str, fallback: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| fallback.into())
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
            seconds_ms(&result["interval"]["startTime"]),
            seconds_ms(&result["interval"]["endTime"]),
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

pub(crate) fn parse_theintrodb(document: &Value) -> Vec<Segment> {
    let mut segments: Vec<Segment> = Vec::new();
    for (field, kind) in [("intro", "intro"), ("recap", "recap"), ("credits", "credits")] {
        let Some(segment) = introdb_segment(&document[field], kind) else {
            continue;
        };
        segments.push(segment);
    }
    segments.sort_by_key(|segment| segment.start_ms);
    segments
}

fn introdb_segment(list: &Value, kind: &'static str) -> Option<Segment> {
    for row in list.as_array().into_iter().flatten().take(MAX_RESULTS) {
        let start_ms = match &row["start_ms"] {
            Value::Null => 0,
            value => milliseconds_ms(value)?,
        };
        let end_ms = milliseconds_ms(&row["end_ms"])?;
        if end_ms <= start_ms {
            continue;
        }
        return Some(Segment {
            kind,
            start_ms,
            end_ms,
        });
    }
    None
}

pub(crate) fn parse_skipdb(document: &Value) -> Vec<Segment> {
    let mut segments: Vec<Segment> = Vec::new();
    for (field, kind) in [("intro", "intro"), ("recap", "recap"), ("outro", "credits")] {
        let Some(row) = document["segments"][field].as_object() else {
            continue;
        };
        let (Some(start_ms), Some(end_ms)) = (
            milliseconds_ms(&row["start_ms"]),
            milliseconds_ms(&row["end_ms"]),
        ) else {
            continue;
        };
        if end_ms <= start_ms {
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

pub(crate) fn animeskip_show(document: &Value) -> Option<String> {
    document["data"]["findShowsByExternalId"]
        .as_array()?
        .iter()
        .find(|show| show["deletedAt"].is_null())
        .and_then(|show| show["id"].as_str())
        .map(str::to_owned)
}

pub(crate) fn animeskip_segments(document: &Value, target: &Target) -> Option<Vec<Segment>> {
    let episodes = document["data"]["findEpisodesByShowId"].as_array()?;
    let timestamps = animeskip_episode(episodes, target)?.get("timestamps")?;
    let segments = animeskip_timestamps(timestamps);
    (!segments.is_empty()).then_some(segments)
}

fn animeskip_episode<'a>(episodes: &'a [Value], target: &Target) -> Option<&'a Value> {
    let live = |episode: &&Value| episode["deletedAt"].is_null();
    if let Some(release) = target.release_episode.map(|episode| episode.to_string()) {
        if let Some(episode) = episodes
            .iter()
            .filter(live)
            .find(|episode| episode["absoluteNumber"].as_str() == Some(release.as_str()))
        {
            return Some(episode);
        }
        if let Some(episode) = episodes
            .iter()
            .filter(live)
            .find(|episode| episode["number"].as_str() == Some(release.as_str()))
        {
            return Some(episode);
        }
    }
    let season = target.season?.to_string();
    let number = target.episode.to_string();
    episodes.iter().filter(live).find(|episode| {
        episode["season"].as_str() == Some(season.as_str())
            && episode["number"].as_str() == Some(number.as_str())
    })
}

fn animeskip_timestamps(timestamps: &Value) -> Vec<Segment> {
    let mut points: Vec<(u64, Option<&'static str>)> = Vec::new();
    for timestamp in timestamps
        .as_array()
        .into_iter()
        .flatten()
        .take(MAX_RESULTS)
    {
        let Some(at) = seconds_ms(&timestamp["at"]) else {
            continue;
        };
        let kind = match timestamp["type"]["name"].as_str() {
            Some("Intro") | Some("New Intro") => Some("intro"),
            Some("Recap") => Some("recap"),
            Some("Credits") | Some("New Credits") => Some("credits"),
            _ => None,
        };
        points.push((at, kind));
    }
    points.sort_by_key(|point| point.0);
    let mut segments: Vec<Segment> = Vec::new();
    for (position, (start_ms, kind)) in points.iter().enumerate() {
        let Some(kind) = kind else {
            continue;
        };
        let Some((end_ms, _)) = points.get(position + 1) else {
            continue;
        };
        if *end_ms <= *start_ms || *end_ms - *start_ms > MAX_INFERRED_MS {
            continue;
        }
        if segments.iter().any(|segment| segment.kind == *kind) {
            continue;
        }
        segments.push(Segment {
            kind,
            start_ms: *start_ms,
            end_ms: *end_ms,
        });
    }
    segments
}

fn seconds_ms(value: &Value) -> Option<u64> {
    let seconds = value.as_f64()?;
    (seconds.is_finite() && (0.0..=MAX_SECONDS).contains(&seconds))
        .then(|| (seconds * 1000.0).round() as u64)
}

fn milliseconds_ms(value: &Value) -> Option<u64> {
    let Some(millis) = value.as_u64().or_else(|| value.as_f64().map(|s| s.round() as u64)) else {
        return None;
    };
    (millis as f64 <= MAX_MILLISECONDS).then_some(millis)
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

fn ids_of(card: &Card) -> Vec<ExternalId> {
    let parent = card
        .parent_identity
        .as_ref()
        .map(|parent| parent.ids.as_slice())
        .unwrap_or(&[]);
    let mut combined: Vec<ExternalId> = Vec::new();
    for id in card.identity.ids.iter().chain(parent.iter()) {
        if !combined
            .iter()
            .any(|existing| existing.namespace == id.namespace && existing.value == id.value)
        {
            combined.push(id.clone());
        }
    }
    combined
}

pub(crate) async fn target(app: &App, lease: &ResourceLease, card: &Card) -> Option<Target> {
    let coordinates = card.identity.episode.as_ref()?;
    if coordinates.episode == 0 {
        return None;
    }
    let own = card.identity.ids.as_slice();
    let parent = card
        .parent_identity
        .as_ref()
        .map(|parent| parent.ids.as_slice())
        .unwrap_or(&[]);
    let release = [own, parent]
        .into_iter()
        .find_map(|ids| release(ids));
    let ids = ids_of(card);
    let cross = app.cross_ids.resolve(&ids).await;

    let mut release_episode = None;
    let mut tvdb = cross.tvdb;
    let mut tmdb = cross.tmdb;
    let mut imdb = cross.imdb;
    if let Some(release) = &release {
        if let Some(show) =
            super::anime_show::resolve(&app.clone().with_lease(lease.clone()), release).await
        {
            if let Some(season) = coordinates.season {
                release_episode = show
                    .release_episode(super::anime_show::TvdbEpisode {
                        season,
                        episode: coordinates.episode,
                    })
                    .filter(|release_episode| release_episode.episode > 0)
                    .map(|release_episode| release_episode.episode);
            }
            tvdb = tvdb.or_else(|| id_u64(&show.identity().ids, "tvdb"));
            tmdb = tmdb.or_else(|| id_u64(&show.identity().ids, "tmdb"));
            imdb = imdb.or_else(|| id_str(&show.identity().ids, "imdb"));
        }
    }
    Some(Target {
        season: coordinates.season,
        episode: coordinates.episode,
        mal: cross.mal.or_else(|| id_u64(&ids, "mal")),
        anilist: cross.anilist.or_else(|| id_u64(&ids, "anilist")),
        release_episode,
        tvdb,
        tmdb,
        imdb,
    })
}

fn id_u64(ids: &[ExternalId], namespace: &str) -> Option<u64> {
    ids.iter()
        .find(|id| id.namespace == namespace)
        .and_then(|id| id.value.parse().ok())
}

fn id_str(ids: &[ExternalId], namespace: &str) -> Option<String> {
    ids.iter()
        .find(|id| id.namespace == namespace)
        .map(|id| id.value.clone())
}

pub(crate) fn empty() -> Value {
    json!({"segments":[]})
}

pub(crate) async fn answer(app: &App, lease: &ResourceLease, card: Option<&Card>) -> Value {
    let Some(card) = card else {
        return empty();
    };
    let Some(target) = target(app, lease, card).await else {
        return empty();
    };
    json!({"segments": app.segments.lookup(&target).await})
}