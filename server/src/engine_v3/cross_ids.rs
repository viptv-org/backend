use super::*;
use viptv_engine::identity::ExternalId;

const BASE: &str = concat!(
    "https://raw.githubusercontent.com/Fribb/anime-lists/",
    "c6012439e4d3ac1914e5624a953d01f079841693",
    "/anime-list-full.json"
);
const BASE_VARIABLE: &str = "VIPTV_FRIBB_BASE";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const REFRESH: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_RESPONSE: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CrossIds {
    pub(crate) mal: Option<u64>,
    pub(crate) anidb: Option<u64>,
    pub(crate) tvdb: Option<u64>,
    pub(crate) tmdb: Option<u64>,
    pub(crate) imdb: Option<String>,
    pub(crate) season: Option<u32>,
    pub(crate) episode_offset: Option<u32>,
}

impl CrossIds {
    pub(crate) fn external_ids(&self) -> Vec<ExternalId> {
        let mut ids = Vec::new();
        for (namespace, value) in [
            ("anidb", self.anidb.map(|v| v.to_string())),
            ("tvdb", self.tvdb.map(|v| v.to_string())),
            ("tmdb", self.tmdb.map(|v| v.to_string())),
            ("imdb", self.imdb.clone()),
            ("mal", self.mal.map(|v| v.to_string())),
        ] {
            let Some(value) = value else {
                continue;
            };
            let id = ExternalId {
                namespace: namespace.into(),
                value,
            };
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        ids
    }

    fn add(&mut self, id: &ExternalId) {
        let parsed = match id.namespace.as_str() {
            "anidb" => id.value.parse().ok().map(|value| (1, value, None)),
            "tvdb" => id.value.parse().ok().map(|value| (2, value, None)),
            "tmdb" => id.value.parse().ok().map(|value| (3, value, None)),
            "imdb" => Some((4, 0, Some(id.value.clone()))),
            "mal" => id.value.parse().ok().map(|value| (5, value, None)),
            _ => None,
        };
        let Some((namespace, value, imdb)) = parsed else {
            return;
        };
        match namespace {
            1 => self.anidb = self.anidb.or(Some(value)),
            2 => self.tvdb = self.tvdb.or(Some(value)),
            3 => self.tmdb = self.tmdb.or(Some(value)),
            4 => {
                if self.imdb.is_none() {
                    self.imdb = imdb;
                }
            }
            5 => self.mal = self.mal.or(Some(value)),
            _ => {}
        }
    }

    fn merge(&mut self, entry: &Entry) {
        self.anidb = self.anidb.or(entry.anidb);
        self.tvdb = self.tvdb.or(entry.tvdb);
        self.tmdb = self.tmdb.or(entry.tmdb);
        if self.imdb.is_none() {
            self.imdb = entry.imdb.clone();
        }
        self.mal = self.mal.or(entry.mal);
        self.season = self.season.or(entry.season);
        self.episode_offset = self.episode_offset.or(entry.episode_offset);
    }
}

#[derive(Clone, Debug)]
struct Entry {
    keys: Vec<String>,
    mal: Option<u64>,
    anidb: Option<u64>,
    tvdb: Option<u64>,
    tmdb: Option<u64>,
    imdb: Option<String>,
    season: Option<u32>,
    episode_offset: Option<u32>,
}

impl Entry {
    fn from(raw: RawEntry) -> Option<Self> {
        let tmdb = raw.themoviedb_id.and_then(|value| value.tv);
        let imdb = raw.imdb_id.into_iter().next();
        let mut keys = Vec::new();
        for (namespace, value) in [
            ("anidb", raw.anidb_id),
            ("mal", raw.mal_id),
            ("anilist", raw.anilist_id),
            ("kitsu", raw.kitsu_id),
            ("tvdb", raw.tvdb_id),
            ("tmdb", tmdb),
        ] {
            if let Some(value) = value {
                keys.push(format!("{namespace}:{value}"));
            }
        }
        if let Some(imdb) = &imdb {
            keys.push(format!("imdb:{imdb}"));
        }
        if keys.is_empty() {
            return None;
        }
        Some(Self {
            keys,
            mal: raw.mal_id,
            anidb: raw.anidb_id,
            tvdb: raw.tvdb_id,
            tmdb,
            imdb,
            season: raw.season.and_then(|season| season.tvdb).filter(|season| *season > 0),
            episode_offset: raw
                .episode_offset
                .and_then(|offset| offset.tvdb)
                .filter(|offset| *offset > 0),
        })
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawEntry {
    anidb_id: Option<u64>,
    mal_id: Option<u64>,
    anilist_id: Option<u64>,
    kitsu_id: Option<u64>,
    tvdb_id: Option<u64>,
    themoviedb_id: Option<RawTmdb>,
    imdb_id: Vec<String>,
    season: Option<RawSeason>,
    episode_offset: Option<RawOffset>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawSeason {
    tvdb: Option<u32>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawOffset {
    tvdb: Option<u32>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawTmdb {
    tv: Option<u64>,
}

struct Mapping {
    entries: Vec<Entry>,
    index: HashMap<String, usize>,
}

impl Mapping {
    fn parse(bytes: &[u8]) -> Option<Self> {
        let mut output = None;
        let mut deserializer = serde_json::Deserializer::from_slice(bytes);
        let visitor = MappingVisitor(&mut output);
        if serde::de::Deserializer::deserialize_seq(&mut deserializer, visitor).is_err() {
            return None;
        }
        output
    }

    fn lookup(&self, id: &ExternalId) -> Option<&Entry> {
        self.index
            .get(&format!("{}:{}", id.namespace, id.value))
            .map(|position| &self.entries[*position])
    }
}

struct MappingVisitor<'a>(&'a mut Option<Mapping>);

impl<'de> serde::de::Visitor<'de> for MappingVisitor<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("an array of anime id mappings")
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let mut entries = Vec::new();
        let mut index = HashMap::new();
        while let Some(raw) = seq.next_element::<RawEntry>()? {
            let Some(entry) = Entry::from(raw) else {
                continue;
            };
            let position = entries.len();
            for key in entry.keys.iter().cloned() {
                index.entry(key).or_insert(position);
            }
            entries.push(entry);
        }
        if !entries.is_empty() {
            *self.0 = Some(Mapping { entries, index });
        }
        Ok(())
    }
}

pub(crate) struct Service {
    client: reqwest::Client,
    base: String,
    mapping: Mutex<Option<(Instant, Arc<Mapping>)>>,
}

impl Service {
    pub(crate) fn from_env() -> Result<Self, String> {
        Self::new(std::env::var(BASE_VARIABLE).unwrap_or_else(|_| BASE.into()))
    }

    fn new(base: String) -> Result<Self, String> {
        Ok(Self {
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(3))
                .timeout(REQUEST_TIMEOUT)
                .user_agent(concat!("VIPTV/", env!("CARGO_PKG_VERSION")))
                .build()
                .map_err(|_| "Fribb mapping HTTP client unavailable")?,
            base,
            mapping: Mutex::new(None),
        })
    }

    #[cfg(test)]
    pub(crate) fn fixture(base: &str) -> Self {
        Self::new(base.into()).unwrap()
    }

    pub(crate) async fn resolve(&self, ids: &[ExternalId]) -> CrossIds {
        let mut cross = CrossIds::default();
        for id in ids {
            cross.add(id);
        }
        let Some(mapping) = self.mapping().await else {
            return cross;
        };
        for id in ids {
            if let Some(entry) = mapping.lookup(id) {
                cross.merge(entry);
            }
        }
        cross
    }

    async fn mapping(&self) -> Option<Arc<Mapping>> {
        if let Some(mapping) = self.cached() {
            return Some(mapping);
        }
        self.refresh().await
    }

    fn cached(&self) -> Option<Arc<Mapping>> {
        let guard = self.mapping.lock().ok()?;
        guard
            .as_ref()
            .filter(|(created, _)| created.elapsed() < REFRESH)
            .map(|(_, mapping)| mapping.clone())
    }

    async fn refresh(&self) -> Option<Arc<Mapping>> {
        let stale = self.stale();
        match self.fetch().await {
            Some(mapping) => {
                if let Ok(mut guard) = self.mapping.lock() {
                    *guard = Some((Instant::now(), mapping.clone()));
                }
                Some(mapping)
            }
            None => stale,
        }
    }

    fn stale(&self) -> Option<Arc<Mapping>> {
        self.mapping
            .lock()
            .ok()?
            .as_ref()
            .map(|(_, mapping)| mapping.clone())
    }

    async fn fetch(&self) -> Option<Arc<Mapping>> {
        let response = self.client.get(&self.base).send().await.ok()?;
        if response.status().as_u16() != 200 {
            return None;
        }
        let body = tokio::time::timeout(REQUEST_TIMEOUT, response.bytes())
            .await
            .ok()?
            .ok()?;
        if body.len() > MAX_RESPONSE {
            return None;
        }
        Mapping::parse(&body).map(Arc::new)
    }
}