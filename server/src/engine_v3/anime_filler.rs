use super::*;
use std::collections::HashSet;
use serde::Serialize;
use unicode_normalization::UnicodeNormalization;

const BASE: &str = "https://www.animefillerlist.com";
const BASE_VARIABLE: &str = "VIPTV_ANIME_FILLER_BASE";
const SPACING: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const BUDGET: Duration = Duration::from_secs(8);
const FRESH: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const RETRY: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_RESPONSE: usize = 4 * 1024 * 1024;
const MAX_TITLES: usize = 16;
const MAX_TITLE: usize = 256;
const MAX_EPISODE: u32 = 100_000;
const INDEX: &str = "";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Kind {
    Filler,
    Mixed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Episode {
    pub(crate) number: u32,
    pub(crate) kind: Kind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Show {
    pub(crate) slug: String,
    pub(crate) name: String,
}

enum Document {
    Index(Arc<HashMap<String, Show>>),
    Episodes(Arc<Vec<Episode>>),
}

struct Cached {
    document: Option<Document>,
    checked: Instant,
    fresh: bool,
}

pub(crate) struct Service {
    client: reqwest::Client,
    base: String,
    spacing: Duration,
    cache: Mutex<HashMap<String, Cached>>,
    inflight: Mutex<HashSet<String>>,
    stored: tokio::sync::Notify,
    next: tokio::sync::Mutex<tokio::time::Instant>,
}

impl Service {
    pub(crate) fn from_env() -> Result<Self, String> {
        Self::new(std::env::var(BASE_VARIABLE).unwrap_or_else(|_| BASE.into()), SPACING)
    }

    fn new(base: String, spacing: Duration) -> Result<Self, String> {
        Ok(Self {
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(5))
                .timeout(REQUEST_TIMEOUT)
                .user_agent(concat!("VIPTV/", env!("CARGO_PKG_VERSION")))
                .build()
                .map_err(|_| "Anime filler HTTP client unavailable")?,
            base: base.trim_end_matches('/').into(),
            spacing,
            cache: Mutex::new(HashMap::new()),
            inflight: Mutex::new(HashSet::new()),
            stored: tokio::sync::Notify::new(),
            next: tokio::sync::Mutex::new(tokio::time::Instant::now()),
        })
    }

    #[cfg(test)]
    pub(crate) fn fixture(base: &str, spacing: Duration) -> Self {
        Self::new(base.into(), spacing).unwrap()
    }

    pub(crate) async fn lookup(self: &Arc<Self>, titles: &[String], budget: Duration) -> Option<(Show, Vec<Episode>)> {
        let deadline = tokio::time::Instant::now() + budget;
        let Some(Document::Index(index)) = self.document(INDEX, deadline).await else {
            return None;
        };
        let show = titles.iter().find_map(|title| index.get(&normalize(title)))?.clone();
        let Some(Document::Episodes(episodes)) = self.document(&show.slug, deadline).await else {
            return None;
        };
        Some((show, episodes.as_ref().clone()))
    }

    async fn document(self: &Arc<Self>, key: &str, deadline: tokio::time::Instant) -> Option<Document> {
        loop {
            let notified = self.stored.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let (document, settled) = self.cached(key);
            if !settled {
                self.refresh(key);
            }
            if settled || document.is_some() {
                return document;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return None;
            }
        }
    }

    fn cached(&self, key: &str) -> (Option<Document>, bool) {
        let Ok(cache) = self.cache.lock() else {
            return (None, true);
        };
        let Some(entry) = cache.get(key) else {
            return (None, false);
        };
        let window = if entry.fresh { FRESH } else { RETRY };
        let document = entry.document.as_ref().map(|document| match document {
            Document::Index(index) => Document::Index(index.clone()),
            Document::Episodes(episodes) => Document::Episodes(episodes.clone()),
        });
        (document, entry.checked.elapsed() < window)
    }

    fn refresh(self: &Arc<Self>, key: &str) {
        if !self.inflight.lock().is_ok_and(|mut inflight| inflight.insert(key.to_owned())) {
            return;
        }
        let service = self.clone();
        let key = key.to_owned();
        tokio::spawn(async move {
            let document = service.fetch(&key).await;
            if let Ok(mut cache) = service.cache.lock() {
                let fresh = document.is_some();
                let previous = cache.remove(&key).and_then(|entry| entry.document);
                cache.insert(key.clone(), Cached {
                    document: document.or(previous),
                    checked: Instant::now(),
                    fresh,
                });
            }
            if let Ok(mut inflight) = service.inflight.lock() {
                inflight.remove(&key);
            }
            service.stored.notify_waiters();
        });
    }

    async fn fetch(&self, key: &str) -> Option<Document> {
        let url = if key == INDEX {
            format!("{}/shows", self.base)
        } else {
            format!("{}/shows/{key}", self.base)
        };
        let body = self.get(&url).await?;
        if key == INDEX {
            let index = parse_index(&body);
            (!index.is_empty()).then(|| Document::Index(Arc::new(index)))
        } else {
            parse_show(&body).map(|episodes| Document::Episodes(Arc::new(episodes)))
        }
    }

    async fn get(&self, url: &str) -> Option<String> {
        let mut next = self.next.lock().await;
        tokio::time::sleep_until(*next).await;
        let response = self.client.get(url).send().await;
        *next = tokio::time::Instant::now() + self.spacing;
        drop(next);
        let response = response.ok()?;
        if response.status().as_u16() != 200 {
            return None;
        }
        if response.content_length().is_some_and(|length| length > MAX_RESPONSE as u64) {
            return None;
        }
        let body = tokio::time::timeout(REQUEST_TIMEOUT, response.bytes()).await.ok()?.ok()?;
        (body.len() <= MAX_RESPONSE).then(|| String::from_utf8_lossy(&body).into_owned())
    }
}

pub(crate) fn normalize(title: &str) -> String {
    title
        .nfkd()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn decode(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&#039;", "'")
        .replace("&#39;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

fn aliases(name: &str) -> Vec<String> {
    let mut aliases = Vec::new();
    if let (Some(open), true) = (name.find(" ("), name.ends_with(')')) {
        aliases.push(name[..open].to_owned());
        aliases.push(name[open + 2..name.len() - 1].to_owned());
    }
    aliases
}

fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 200
        && slug
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'%'))
}

pub(crate) fn parse_index(html: &str) -> HashMap<String, Show> {
    let mut shows = Vec::new();
    for group in html.split("<div class=\"Group\">").skip(1) {
        let group = group.split("</div>").next().unwrap_or_default();
        for link in group.split("<a href=\"/shows/").skip(1) {
            let Some((slug, rest)) = link.split_once("\">") else {
                continue;
            };
            let Some((name, _)) = rest.split_once("</a>") else {
                continue;
            };
            let name = decode(name.trim());
            if valid_slug(slug) && !name.is_empty() {
                shows.push(Show {
                    slug: slug.to_owned(),
                    name,
                });
            }
        }
    }
    let mut index = HashMap::new();
    let mut ambiguous = HashSet::new();
    for show in &shows {
        let key = normalize(&show.name);
        if !key.is_empty() && index.insert(key.clone(), show.clone()).is_some_and(|other: Show| other.slug != show.slug) {
            ambiguous.insert(key);
        }
    }
    let names: HashSet<String> = index.keys().cloned().collect();
    let mut derived: HashMap<String, Show> = HashMap::new();
    for show in &shows {
        for alias in aliases(&show.name) {
            let key = normalize(&alias);
            if key.is_empty() || names.contains(&key) {
                continue;
            }
            if derived.insert(key.clone(), show.clone()).is_some_and(|other| other.slug != show.slug) {
                ambiguous.insert(key);
            }
        }
    }
    index.extend(derived);
    index.retain(|key, _| !ambiguous.contains(key));
    index
}

pub(crate) fn parse_show(html: &str) -> Option<Vec<Episode>> {
    let table = html.split("<table class=\"EpisodeList\"").nth(1)?;
    let table = table.split("</table>").next().unwrap_or_default();
    let mut episodes = Vec::new();
    for row in table.split("<tr class=\"").skip(1) {
        let Some((class, cells)) = row.split_once('"') else {
            continue;
        };
        let kind = match class.split_whitespace().next() {
            Some("filler") => Kind::Filler,
            Some("mixed_canon/filler") => Kind::Mixed,
            _ => continue,
        };
        let Some(number) = cells
            .split("<td class=\"Number\">")
            .nth(1)
            .and_then(|cell| cell.split('<').next())
            .and_then(|number| number.trim().parse::<u32>().ok())
            .filter(|number| (1..=MAX_EPISODE).contains(number))
        else {
            continue;
        };
        if !episodes.iter().any(|episode: &Episode| episode.number == number) {
            episodes.push(Episode { number, kind });
        }
    }
    episodes.sort_by_key(|episode| episode.number);
    Some(episodes)
}

pub(crate) fn titles(query: &str) -> Vec<String> {
    let mut titles: Vec<String> = Vec::new();
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        let value = value.trim();
        if key == "title" && !value.is_empty() && value.len() <= MAX_TITLE && !titles.iter().any(|title| title == value) {
            titles.push(value.to_owned());
        }
        if titles.len() == MAX_TITLES {
            break;
        }
    }
    titles
}

pub(crate) async fn episodes(
    State(app): State<App>,
    Extension(_lease): Extension<ResourceLease>,
    axum::extract::RawQuery(query): axum::extract::RawQuery,
) -> axum::Json<Value> {
    let titles = titles(query.as_deref().unwrap_or_default());
    axum::Json(match app.anime_filler.lookup(&titles, BUDGET).await {
        Some((show, episodes)) => json!({"show": show.slug, "episodes": episodes}),
        None => json!({"show": null, "episodes": []}),
    })
}
