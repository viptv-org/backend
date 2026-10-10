//! Anime ids (kitsu/mal/anilist/anidb) → IMDb through Simkl, verified
//! against Cinemeta. Matching library items are rewritten
//! into IMDb items before mapping, so every mapper check still applies.
//! Several anime entries of one IMDb show (seasons, split cours) merge into one
//! item. An episode that cannot be translated and found in Cinemeta is left
//! for review, never guessed.
use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use flate2::{write::ZlibEncoder, Compression};
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    io::Write,
};

const PREFIXES: [&str; 4] = ["kitsu", "mal", "anilist", "anidb"];
const MAX_TITLES: usize = 500;
const MAX_RESPONSE: usize = 4_000_000;
const MAX_EPISODES: i64 = 2000;

/// Simkl (`SIMKL_CLIENT_ID`): anime id → cross ids and per-episode TVDB
/// numbering. Lookups are on demand and paced under Simkl's 10 GET/s rule.
pub(crate) struct MetadataService {
    base: String,
    key: String,
    next: tokio::sync::Mutex<tokio::time::Instant>,
}

impl MetadataService {
    pub(crate) fn from_env() -> Option<Self> {
        let key = std::env::var("SIMKL_CLIENT_ID").ok()?;
        Self::new("https://api.simkl.com", &key)
    }

    pub(crate) fn new(base: &str, key: &str) -> Option<Self> {
        let base = base.trim().trim_end_matches('/').to_owned();
        let key = key.trim().to_owned();
        (!base.is_empty() && !key.is_empty()).then(|| Self {
            base,
            key,
            next: tokio::sync::Mutex::new(tokio::time::Instant::now()),
        })
    }

    async fn get(&self, client: &reqwest::Client, path: &str) -> Option<Value> {
        {
            // At most ~8 requests per second across every lookup.
            let mut next = self.next.lock().await;
            tokio::time::sleep_until(*next).await;
            *next = tokio::time::Instant::now() + Duration::from_millis(125);
        }
        let mut response = client
            .get(format!("{}{path}", self.base))
            .header("simkl-api-key", &self.key)
            .header("app-name", "viptv")
            .header("app-version", env!("CARGO_PKG_VERSION"))
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.ok()? {
            if body.len() + chunk.len() > MAX_RESPONSE {
                return None;
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body).ok()
    }
}

pub(super) struct Remapped {
    /// Source items with resolved anime entries replaced by IMDb items.
    pub items: Vec<Value>,
    /// Original anime id → IMDb id, for every replaced item.
    pub renamed: HashMap<String, String>,
    /// Trimmed Cinemeta metadata for the IMDb targets.
    pub meta: HashMap<(String, String), Value>,
    /// Replaced items that lost watched or resume state to verification.
    pub unverified: usize,
}

fn imdb(id: &str) -> bool {
    id.strip_prefix("tt").is_some_and(|digits| {
        (5..=12).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit())
    })
}

fn anime_id(id: &str) -> Option<(&str, &str)> {
    let (source, number) = id.split_once(':')?;
    (PREFIXES.contains(&source)
        && (1..=12).contains(&number.len())
        && number.bytes().all(|b| b.is_ascii_digit()))
    .then_some((source, number))
}

struct Resolution {
    imdb: String,
    /// Anime episode number → (season, episode) in the target's numbering.
    episodes: HashMap<i64, (i64, i64)>,
}

async fn resolve(
    service: &MetadataService,
    client: &reqwest::Client,
    kind: &str,
    source: &str,
    number: &str,
) -> Option<Resolution> {
    // Exactly one Simkl anime may claim the id.
    let found = service.get(client, &format!("/search/id?{source}={number}")).await?;
    let [hit] = found.as_array()?.as_slice() else { return None };
    if hit["type"] != "anime" {
        return None;
    }
    let simkl = hit["ids"]["simkl"].as_i64()?;
    let detail = service.get(client, &format!("/anime/{simkl}?extended=full")).await?;
    if detail["ids"][source].as_str() != Some(number) {
        return None; // the hit does not name this exact work
    }
    let imdb = detail["ids"]["imdb"].as_str().filter(|i| imdb(i))?.to_owned();
    if kind == "movie" {
        return (detail["anime_type"] == "movie").then(|| Resolution { imdb, episodes: HashMap::new() });
    }
    let list = service.get(client, &format!("/anime/episodes/{simkl}?extended=full")).await?;
    let mut episodes = HashMap::new();
    let mut conflicting = HashSet::new();
    for e in list.as_array()?.iter().filter(|e| e["type"] == "episode") {
        let (Some(n), Some(s), Some(t)) =
            (e["episode"].as_i64(), e["tvdb"]["season"].as_i64(), e["tvdb"]["episode"].as_i64())
        else {
            continue;
        };
        if !(1..=MAX_EPISODES).contains(&n) {
            continue;
        }
        if episodes.insert(n, (s, t)).is_some_and(|prior| prior != (s, t)) {
            conflicting.insert(n);
        }
    }
    // Two different targets for one episode: neither is trusted.
    episodes.retain(|n, _| !conflicting.contains(n));
    Some(Resolution { imdb, episodes })
}

fn episode_number(video: &str, title: &str) -> Option<i64> {
    video
        .strip_prefix(title)?
        .strip_prefix(':')?
        .parse()
        .ok()
        .filter(|n| (1..=MAX_EPISODES).contains(n))
}

fn translate(n: i64, resolution: &Resolution, known: &HashSet<String>) -> Option<String> {
    let (s, e) = resolution.episodes.get(&n)?;
    let video = format!("{}:{s}:{e}", resolution.imdb);
    known.contains(&video).then_some(video)
}

/// Anime-side watched episodes, decoded against the anime's own sequential
/// episode list (`<id>:1`, `<id>:2`, … as Stremio's anime add-ons number them).
fn anime_watched(field: &str, title: &str, resolution: &Resolution) -> Option<Vec<i64>> {
    let (prefix, _) = field.rsplit_once(':')?;
    let (anchor, _) = prefix.rsplit_once(':')?;
    let last = resolution
        .episodes
        .keys()
        .copied()
        .chain(episode_number(anchor, title))
        .max()?;
    let videos: Vec<Value> = (1..=last)
        .map(|n| json!({"id":format!("{title}:{n}"),"season":1,"episode":n}))
        .collect();
    let decoded = mapper::verified_episodes(field, title, &json!({"videos":videos}))?;
    Some(decoded.into_iter().map(|(_, _, n)| n).collect())
}

/// Stremio's anchored bitfield over `meta`'s ordered videos.
fn encode(watched: &HashSet<String>, title: &str, meta: &Value) -> Option<String> {
    let ordered: Vec<&str> = meta["videos"].as_array()?.iter().filter_map(|v| v["id"].as_str()).collect();
    let anchor = ordered.iter().rposition(|id| watched.contains(*id))?;
    let count = anchor + 1;
    let mut bytes = vec![0u8; count.div_ceil(8)];
    for (index, id) in ordered[..count].iter().enumerate() {
        if watched.contains(*id) {
            bytes[index / 8] |= 1 << (index % 8);
        }
    }
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&bytes).ok()?;
    let field = format!("{}:{count}:{}", ordered[anchor], STANDARD.encode(encoder.finish().ok()?));
    // Round trip through the mapper's own verification before trusting it.
    let decoded: HashSet<String> = mapper::verified_episodes(&field, title, meta)?
        .into_iter()
        .map(|(id, _, _)| id)
        .collect();
    (&decoded == watched).then_some(field)
}

fn last_watched(item: &Value) -> i64 {
    item["state"]["lastWatched"]
        .as_str()
        .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
        .map_or(0, |d| d.timestamp_millis())
}

/// One IMDb item from every source entry that names it.
fn merge(
    kind: &str,
    target: &str,
    members: &[(&Value, Option<&Resolution>)],
    meta: &Value,
    unverified: &mut usize,
) -> Value {
    let known: HashSet<String> = meta["videos"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v["id"].as_str().map(str::to_owned))
        .collect();
    let mut watched = HashSet::new();
    let mut lost = false;
    let mut videos: Vec<Option<String>> = Vec::with_capacity(members.len());
    for (item, resolution) in members {
        let title = item["_id"].as_str().unwrap_or("");
        let state = &item["state"];
        let field = state["watched"].as_str().filter(|s| !s.is_empty());
        match resolution {
            Some(resolution) => {
                if let Some(field) = field {
                    match anime_watched(field, title, resolution) {
                        Some(numbers) => {
                            for n in numbers {
                                match translate(n, resolution, &known) {
                                    Some(video) => {
                                        watched.insert(video);
                                    }
                                    None => lost = true,
                                }
                            }
                        }
                        None => lost = true,
                    }
                }
                videos.push(
                    state["video_id"]
                        .as_str()
                        .and_then(|v| episode_number(v, title))
                        .and_then(|n| translate(n, resolution, &known)),
                );
            }
            None => {
                if let Some(field) = field {
                    match mapper::verified_episodes(field, target, meta) {
                        Some(list) => watched.extend(list.into_iter().map(|(id, _, _)| id)),
                        None => lost = true,
                    }
                }
                videos.push(state["video_id"].as_str().map(str::to_owned));
            }
        }
    }
    let latest = (0..members.len()).max_by_key(|&i| last_watched(members[i].0)).unwrap_or(0);
    let mut merged = members[latest].0.clone();
    merged["_id"] = json!(target);
    if let Some((native, _)) = members.iter().find(|(_, r)| r.is_none()) {
        merged["name"] = native["name"].clone();
    }
    merged["removed"] = json!(members.iter().all(|(i, _)| i["removed"].as_bool().unwrap_or(false)));
    merged["temp"] = json!(members.iter().all(|(i, _)| i["temp"].as_bool().unwrap_or(false)));
    if !merged["state"].is_object() {
        merged["state"] = json!({});
    }
    let state = &mut merged["state"];
    state["timesWatched"] = json!(members
        .iter()
        .filter_map(|(i, _)| i["state"]["timesWatched"].as_u64())
        .max()
        .unwrap_or(0));
    if kind == "series" {
        match videos[latest].take() {
            Some(video) => state["video_id"] = json!(video),
            None => {
                if state["timeOffset"].as_u64().unwrap_or(0) > 0 {
                    lost = true;
                }
                state["timeOffset"] = json!(0);
                state["video_id"] = Value::Null;
            }
        }
        match encode(&watched, target, meta) {
            Some(field) => state["watched"] = json!(field),
            None => {
                lost |= !watched.is_empty();
                state["watched"] = Value::Null;
            }
        }
    }
    if lost {
        *unverified += 1;
    }
    merged
}

pub(super) async fn remap(app: &App, items: &[Value]) -> Remapped {
    use futures::{stream, StreamExt};
    let mut out = Remapped {
        items: items.to_vec(),
        renamed: HashMap::new(),
        meta: HashMap::new(),
        unverified: 0,
    };
    let Some(service) = app.stremio_import.metadata_service.as_ref() else {
        return out;
    };
    let client = &app.stremio_import.client;
    let mut seen = HashSet::new();
    let wanted: Vec<(String, String)> = items
        .iter()
        .filter_map(|i| Some((i["type"].as_str()?, i["_id"].as_str()?)))
        .filter(|(kind, id)| matches!(*kind, "movie" | "series") && anime_id(id).is_some())
        .filter(|identity| seen.insert(*identity))
        .take(MAX_TITLES)
        .map(|(k, i)| (k.to_owned(), i.to_owned()))
        .collect();
    if wanted.is_empty() {
        return out;
    }
    // Shares the review request budget with add-on verification, which runs
    // alongside; unfinished titles stay review-only.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(25);
    let mut resolved: HashMap<(String, String), Resolution> = HashMap::new();
    let mut pending = stream::iter(wanted)
        .map(|(kind, id)| async move {
            let (source, number) = anime_id(&id)?;
            let r = resolve(service, client, &kind, source, number).await?;
            Some(((kind, id), r))
        })
        .buffer_unordered(8);
    while let Ok(Some(result)) = tokio::time::timeout_at(deadline, pending.next()).await {
        if let Some((identity, r)) = result {
            resolved.insert(identity, r);
        }
    }
    drop(pending);
    let targets: HashSet<(String, String)> =
        resolved.iter().map(|((kind, _), r)| (kind.clone(), r.imdb.clone())).collect();
    let mut fetching = stream::iter(targets)
        .map(|(kind, id)| async move {
            let meta = addon_review::cinemeta(app, &kind, &id).await?;
            Some(((kind, id), meta))
        })
        .buffer_unordered(8);
    while let Ok(Some(result)) = tokio::time::timeout_at(deadline, fetching.next()).await {
        if let Some((identity, meta)) = result {
            out.meta.insert(identity, meta);
        }
    }
    drop(fetching);

    // Group every entry (anime or native IMDb) by its IMDb target.
    let mut groups: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    for (index, item) in items.iter().enumerate() {
        let (Some(kind), Some(id)) = (item["type"].as_str(), item["_id"].as_str()) else { continue };
        if let Some(r) = resolved.get(&(kind.to_owned(), id.to_owned())) {
            if out.meta.contains_key(&(kind.to_owned(), r.imdb.clone())) {
                groups.entry((kind.to_owned(), r.imdb.clone())).or_default().push(index);
            }
        }
    }
    for (index, item) in items.iter().enumerate() {
        let (Some(kind), Some(id)) = (item["type"].as_str(), item["_id"].as_str()) else { continue };
        if let Some(members) = groups.get_mut(&(kind.to_owned(), id.to_owned())) {
            members.push(index);
        }
    }
    let mut consumed = HashSet::new();
    let mut merged = VecDeque::new();
    for ((kind, target), indices) in &groups {
        let members: Vec<(&Value, Option<&Resolution>)> = indices
            .iter()
            .map(|&i| {
                let item = &items[i];
                let id = item["_id"].as_str().unwrap_or("");
                (item, resolved.get(&(kind.clone(), id.to_owned())))
            })
            .collect();
        merged.push_back(merge(kind, target, &members, &out.meta[&(kind.clone(), target.clone())], &mut out.unverified));
        for &i in indices {
            consumed.insert(i);
            if let Some(id) = items[i]["_id"].as_str().filter(|id| anime_id(id).is_some()) {
                out.renamed.insert(id.to_owned(), target.clone());
            }
        }
    }
    out.items = items
        .iter()
        .enumerate()
        .filter(|(i, _)| !consumed.contains(i))
        .map(|(_, item)| item.clone())
        .chain(merged)
        .collect();
    out
}
