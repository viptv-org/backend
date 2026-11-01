use super::*;
use viptv_engine::identity::{EpisodeCoordinates, ExternalId, MediaIdentity};
use viptv_engine::presentation::{Card, DetailSnapshot};

const ANIME_ADDONS: [&str; 4] = ["mal:", "kitsu:", "anilist:", "anidb:"];
const ANIME_SOURCES: [&str; 4] = ["mal", "kitsu", "anilist", "anidb"];
const MAX_EPISODES: u32 = 10_000;
const MAX_ROWS: usize = 10_000;

#[derive(Clone, Debug)]
pub(crate) struct AnimeShow {
    identity: MediaIdentity,
    layout: Layout,
    document: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Layout {
    Aired,
    Segments {
        release_season: Option<u32>,
        segments: Vec<Segment>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Segment {
    season: u32,
    first: u32,
    last: u32,
    offset: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TvdbEpisode {
    pub(crate) season: u32,
    pub(crate) episode: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReleaseEpisode {
    pub(crate) season: Option<u32>,
    pub(crate) episode: u32,
}

impl AnimeShow {
    pub(crate) fn identity(&self) -> &MediaIdentity {
        &self.identity
    }

    pub(crate) fn tvdb_episode(&self, release: &EpisodeCoordinates) -> Option<TvdbEpisode> {
        if release.episode == 0 {
            return None;
        }
        match &self.layout {
            Layout::Aired => Some(TvdbEpisode {
                season: release.season?,
                episode: release.episode,
            }),
            Layout::Segments { segments, .. } => {
                let segment = segments
                    .iter()
                    .find(|segment| (segment.first..=segment.last).contains(&release.episode))?;
                let episode = i64::from(release.episode) + segment.offset;
                (1..=i64::from(MAX_EPISODES))
                    .contains(&episode)
                    .then(|| TvdbEpisode {
                        season: segment.season,
                        episode: episode as u32,
                    })
            }
        }
    }

    pub(crate) fn lists(&self, tvdb: TvdbEpisode) -> bool {
        self.document
            .as_ref()
            .is_some_and(|document| lists_episode(document, tvdb.season, tvdb.episode))
    }

    pub(crate) fn canonical_episode(&self, release: &EpisodeCoordinates) -> Option<TvdbEpisode> {
        if release.episode == 0 {
            return None;
        }
        let season = release.season?;
        if self
            .document
            .as_ref()
            .is_some_and(|document| lists_episode(document, season, release.episode))
        {
            return Some(TvdbEpisode {
                season,
                episode: release.episode,
            });
        }
        self.tvdb_episode(release)
    }

    pub(crate) fn release_episode(&self, tvdb: TvdbEpisode) -> Option<ReleaseEpisode> {
        if tvdb.episode == 0 {
            return None;
        }
        let (season, episode) = match &self.layout {
            Layout::Aired => (Some(tvdb.season), tvdb.episode),
            Layout::Segments {
                release_season,
                segments,
            } => {
                let episode = segments
                    .iter()
                    .filter(|segment| segment.season == tvdb.season)
                    .find_map(|segment| {
                        let episode = i64::from(tvdb.episode) - segment.offset;
                        (i64::from(segment.first)..=i64::from(segment.last))
                            .contains(&episode)
                            .then_some(episode as u32)
                    })?;
                (*release_season, episode)
            }
        };
        Some(ReleaseEpisode { season, episode })
    }

    fn details_document(&self, release: &Value) -> Value {
        if let Some(document) = &self.document {
            let mut document = document.clone();
            document["id"] = release["id"].clone();
            document["type"] = release["type"].clone();
            let ids = release["videos"]
                .as_array()
                .into_iter()
                .flatten()
                .take(MAX_ROWS)
                .filter_map(|video| {
                    let tvdb = self.tvdb_episode(&video_coordinates(video)?)?;
                    Some(((tvdb.season, tvdb.episode), video["id"].as_str()?.to_owned()))
                })
                .collect();
            keep_release_ids(&mut document, ids);
            return document;
        }
        let mut release = release.clone();
        if let Some(videos) = release["videos"].as_array_mut() {
            for video in videos.iter_mut().take(MAX_ROWS) {
                let Some(release_episode) = video_coordinates(video) else {
                    continue;
                };
                let Some(tvdb) = self.tvdb_episode(&release_episode) else {
                    continue;
                };
                video["season"] = json!(tvdb.season);
                video["episode"] = json!(tvdb.episode);
            }
        }
        release
    }
}

pub(crate) async fn tvdb_document(
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    release: &Value,
) -> Option<Value> {
    let release_identity = MediaIdentity {
        id: release["id"].as_str()?.to_owned(),
        kind: release["type"].as_str()?.to_owned(),
        ids: provider_identity::ids(release),
        episode: None,
        edition: None,
    };
    let addons = metadata_addons(app, lease, profile)?;
    let show = resolve_with(&addons, &release_identity, Some(release.clone())).await?;
    Some(show.details_document(release))
}

pub(crate) async fn detail(
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    item: &Card,
    provider: &str,
    offset: usize,
    limit: usize,
) -> Option<DetailSnapshot> {
    let addons = metadata_addons(app, lease, profile)?;
    let release = MediaIdentity {
        id: item.identity.id.clone(),
        kind: item.identity.kind.clone(),
        ids: item.identity.ids.clone(),
        episode: None,
        edition: None,
    };
    let show = resolve(&addons, &release).await?;
    let mut parent = show.document.as_ref()?.clone();
    parent["id"] = json!(item.identity.id);
    parent["type"] = json!(item.identity.kind);
    let ids = simkl_episode_coordinates(app, &item.identity.id)
        .await
        .into_iter()
        .map(|(season, episode)| {
            (
                (season, episode),
                format!("{}:{season}:{episode}", item.identity.id),
            )
        })
        .collect();
    keep_release_ids(&mut parent, ids);
    let videos = {
        let db = app.db.lock().ok()?;
        kids::filter_items(
            &db,
            &app.identity(),
            parent["videos"].as_array().cloned().unwrap_or_default(),
        )
        .ok()?
    };
    parent["videos"] = json!(videos);
    let mut page = viptv_engine::stremio::detail_page(
        &json!({"meta":parent}).to_string(),
        provider,
        &item.identity.kind,
        &item.identity.id,
        offset,
        limit,
    )
    .ok()?;
    for id in show.identity().ids.iter().chain(item.identity.ids.iter()) {
        if page.item.identity.ids.len() == 32 {
            break;
        }
        if !page.item.identity.ids.contains(id) {
            page.item.identity.ids.push(id.clone());
        }
    }
    for episode in &mut page.episodes {
        episode.parent_identity = Some(page.item.identity.clone());
    }
    Some(page)
}

fn metadata_addons(app: &App, lease: &ResourceLease, profile: i64) -> Option<App> {
    super::search::metadata_scope(app.clone(), lease, profile, "stremio", None)
        .ok()
        .map(|(app, _, _)| app)
}

fn keep_release_ids(document: &mut Value, ids: std::collections::BTreeMap<(u32, u32), String>) {
    if ids.is_empty() {
        return;
    }
    let Some(videos) = document["videos"].as_array_mut() else {
        return;
    };
    for video in videos.iter_mut().take(MAX_ROWS) {
        let Some(coordinates) = video_coordinates(video) else {
            continue;
        };
        let Some(season) = coordinates.season else {
            continue;
        };
        if let Some(id) = ids.get(&(season, coordinates.episode)) {
            video["id"] = json!(id);
        }
    }
}

async fn simkl_episode_coordinates(app: &App, title: &str) -> Vec<(u32, u32)> {
    let Some(id) = simkl_anime(title) else {
        return Vec::new();
    };
    let Some(rows) = simkl_episodes(app, id).await else {
        return Vec::new();
    };
    rows.iter()
        .take(MAX_ROWS)
        .filter_map(|row| {
            Some((
                u32::try_from(row["tvdb"]["season"].as_u64()?).ok()?,
                u32::try_from(row["tvdb"]["episode"].as_u64()?).ok()?,
            ))
        })
        .collect()
}

fn video_coordinates(video: &Value) -> Option<EpisodeCoordinates> {
    let episode = video["episode"].as_u64().or_else(|| video["number"].as_u64()).or_else(|| {
        let id = video["id"].as_str()?;
        let parts = id.split(':').collect::<Vec<_>>();
        (parts.len() == 3)
            .then_some(parts[2])
            .and_then(|part| part.parse().ok())
    })?;
    Some(EpisodeCoordinates {
        order: "aired".into(),
        season: video["season"]
            .as_u64()
            .and_then(|season| u32::try_from(season).ok()),
        episode: u32::try_from(episode).ok()?,
    })
}

pub(crate) fn anime_release(id: &str) -> bool {
    simkl_anime(id).is_some()
        || ANIME_ADDONS
            .iter()
            .any(|prefix| id.starts_with(*prefix))
}

fn cross_ids_of(identity: &MediaIdentity) -> Vec<ExternalId> {
    let mut ids = identity.ids.clone();
    let (namespace, value) = match identity.id.split_once(':') {
        Some((namespace, value)) if ANIME_SOURCES.contains(&namespace) => (namespace, value),
        _ => return ids,
    };
    let id = ExternalId {
        namespace: namespace.into(),
        value: value.into(),
    };
    if !ids.contains(&id) {
        ids.push(id);
    }
    ids
}

pub(crate) fn addon_serves_meta(app: &App, addon: i64, kind: &str) -> bool {
    app.addons.entries().ok().is_some_and(|entries| {
        entries
            .into_iter()
            .any(|(candidate, _, manifest)| candidate == addon && crate::addon::declares_meta(&manifest, kind))
    })
}

pub(crate) struct CanonicalDocument {
    pub(crate) document: Value,
    pub(crate) focus_season: Option<u32>,
    pub(crate) focus_episode: Option<u32>,
}

pub(crate) async fn release_document(app: &App, id: &str) -> Option<CanonicalDocument> {
    if !anime_release(id) {
        return None;
    }
    let release = MediaIdentity {
        id: id.to_owned(),
        kind: "series".into(),
        ids: Vec::new(),
        episode: None,
        edition: None,
    };
    let cross = app.cross_ids.resolve(&cross_ids_of(&release)).await;
    let release = MediaIdentity {
        id: release.id,
        kind: release.kind,
        ids: cross.external_ids(),
        episode: None,
        edition: None,
    };
    let show = resolve(app, &release).await?;
    let mut document = show.document.as_ref()?.clone();
    document["id"] = json!(id);
    document["type"] = json!("series");
    document["ids"] = identity_ids(show.identity());
    Some(CanonicalDocument {
        document,
        focus_season: cross.season,
        focus_episode: cross.episode_offset,
    })
}

pub(crate) async fn title_document(
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    id: &str,
    title: &str,
    year: Option<u32>,
    season: Option<u32>,
) -> Option<CanonicalDocument> {
    let cards = crate::engine_v3::search::canonical_search(app, lease, profile, title, "series")
        .await
        .ok()?;
    let candidate = title_match(&cards, title, year)?;
    let mut identity = candidate.identity.clone();
    ensure_canonical_id(&mut identity);
    let mut identity = show_identity(identity)?;
    let mut document = show_document(app, &mut identity).await?;
    document["id"] = json!(id);
    document["type"] = json!("series");
    document["ids"] = identity_ids(&identity);
    let focus_season = season.filter(|season| season_exists(&document, *season));
    Some(CanonicalDocument {
        document,
        focus_season,
        focus_episode: None,
    })
}

fn title_match<'a>(
    cards: &'a [Card],
    title: &str,
    year: Option<u32>,
) -> Option<&'a Card> {
    let needle = normalized_title(title);
    let mut found = None;
    for card in cards {
        if card.identity.kind != "series" || !canonical_identity(&card.identity) {
            continue;
        }
        if let (Some(needle_year), Some(candidate_year)) =
            (year, card.metadata.as_ref().and_then(|metadata| metadata.year))
        {
            if needle_year.abs_diff(candidate_year) > 1 {
                continue;
            }
        }
        let matches = normalized_title(&card.title) == needle
            || card.metadata.as_ref().is_some_and(|metadata| {
                metadata
                    .alternate_titles
                    .iter()
                    .any(|alternate| normalized_title(alternate) == needle)
            });
        if matches {
            if found.is_some() {
                return None;
            }
            found = Some(card);
        }
    }
    found
}

fn canonical_identity(identity: &MediaIdentity) -> bool {
    ["imdb", "tvdb", "tmdb"]
        .into_iter()
        .any(|namespace| id_of(identity, namespace).is_some())
        || identity.id.starts_with("tvdb:")
        || identity.id.starts_with("tmdb:")
        || identity.id.strip_prefix("tt").is_some_and(|digits| {
            !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn ensure_canonical_id(identity: &mut MediaIdentity) {
    let (namespace, value) = if identity
        .id
        .strip_prefix("tt")
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()))
    {
        ("imdb", identity.id.as_str())
    } else if let Some(value) = identity.id.strip_prefix("tvdb:") {
        ("tvdb", value)
    } else if let Some(value) = identity.id.strip_prefix("tmdb:") {
        ("tmdb", value)
    } else {
        return;
    };
    let id = ExternalId {
        namespace: namespace.into(),
        value: value.to_owned(),
    };
    if !identity.ids.contains(&id) {
        identity.ids.push(id);
    }
}

fn season_exists(document: &Value, season: u32) -> bool {
    document["videos"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|video| video["season"].as_u64() == Some(u64::from(season)))
}

pub(crate) fn split_title_season(title: &str) -> (String, Option<u32>) {
    let words: Vec<String> = title.split_whitespace().map(str::to_owned).collect();
    if words.is_empty() {
        return (title.to_owned(), None);
    }
    for index in 0..words.len() {
        if words[index].eq_ignore_ascii_case("season")
            || words[index].eq_ignore_ascii_case("part")
        {
            if let Some(season) = words.get(index + 1).and_then(|word| parse_season(word)) {
                return (join_without(&words, index, index + 1), Some(season));
            }
        }
        if let Some(season) = ordinal_number(&words[index]) {
            if words
                .get(index + 1)
                .is_some_and(|word| word.eq_ignore_ascii_case("season"))
            {
                return (join_without(&words, index, index + 1), Some(season));
            }
        }
        if let Some(season) = s_number(&words[index]) {
            return (join_without(&words, index, index), Some(season));
        }
    }
    let last = words.len() - 1;
    if let Some(season) = roman_number(&words[last]) {
        return (join_without(&words, last, last), Some(season));
    }
    if let Some(season) = small_season_number(&words[last]) {
        return (join_without(&words, last, last), Some(season));
    }
    (title.to_owned(), None)
}

fn parse_season(word: &str) -> Option<u32> {
    word.parse::<u32>().ok().filter(|season| (1..=999).contains(season))
}

fn ordinal_number(word: &str) -> Option<u32> {
    let lower = word.to_lowercase();
    ["st", "nd", "rd", "th"].iter().find_map(|suffix| {
        lower
            .strip_suffix(suffix)
            .and_then(|prefix| prefix.parse::<u32>().ok())
            .filter(|season| (1..=999).contains(season))
    })
}

fn roman_number(word: &str) -> Option<u32> {
    let value = match word.to_uppercase().as_str() {
        "II" => 2,
        "III" => 3,
        "IV" => 4,
        "V" => 5,
        "VI" => 6,
        "VII" => 7,
        "VIII" => 8,
        "IX" => 9,
        "X" => 10,
        _ => return None,
    };
    Some(value)
}

fn s_number(word: &str) -> Option<u32> {
    let rest = word.strip_prefix('S').or_else(|| word.strip_prefix('s'))?;
    if rest.is_empty() || !rest.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    rest.parse::<u32>().ok().filter(|season| (1..=999).contains(season))
}

fn small_season_number(word: &str) -> Option<u32> {
    word.parse::<u32>().ok().filter(|season| (2..=13).contains(season))
}

fn join_without(words: &[String], from: usize, to: usize) -> String {
    words
        .iter()
        .enumerate()
        .filter(|(index, _)| *index < from || *index > to)
        .map(|(_, word)| word.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalized_title(title: &str) -> String {
    title
        .chars()
        .flat_map(char::to_lowercase)
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn identity_ids(identity: &MediaIdentity) -> Value {
    let mut ids = serde_json::Map::new();
    for id in &identity.ids {
        ids.entry(id.namespace.clone())
            .or_insert_with(|| json!(id.value));
    }
    Value::Object(ids)
}

fn lists_episode(document: &Value, season: u32, episode: u32) -> bool {
    document["videos"]
        .as_array()
        .into_iter()
        .flatten()
        .take(MAX_ROWS)
        .any(|video| {
            video["season"].as_u64() == Some(u64::from(season))
                && video["episode"]
                    .as_u64()
                    .or_else(|| video["number"].as_u64())
                    == Some(u64::from(episode))
        })
}

pub(crate) async fn canonical_episode(
    app: &App,
    release: &MediaIdentity,
    coordinates: &EpisodeCoordinates,
) -> Option<(MediaIdentity, TvdbEpisode)> {
    let show = resolve(app, release).await?;
    let tvdb = show.canonical_episode(coordinates)?;
    Some((show.identity().clone(), tvdb))
}

pub(crate) async fn canonical_show(
    app: &App,
    release: &MediaIdentity,
) -> Option<MediaIdentity> {
    resolve(app, release)
        .await
        .map(|show| show.identity().clone())
}

pub(crate) async fn canonical_id(app: &App, kind: &str, id: &str) -> Option<String> {
    if kind != "series" {
        return None;
    }
    let release = MediaIdentity {
        id: id.to_owned(),
        kind: kind.to_owned(),
        ids: Vec::new(),
        episode: None,
        edition: None,
    };
    canonical_show(app, &release).await.map(|show| show.id)
}

pub(crate) async fn canonical_progress(app: &App, value: &Value) -> Option<Value> {
    let coordinates = episode_coordinates(value)?;
    let release = release_identity(value)?;
    let id = value["id"].as_str()?;
    let show = resolve(app, &release).await?;
    let prefix = format!("{}:", show.identity().id);
    let tvdb = if id.starts_with(&prefix) {
        TvdbEpisode {
            season: coordinates.season?,
            episode: coordinates.episode,
        }
    } else if let Some(episode) = episode_suffix(id, &release.id) {
        show.tvdb_episode(&EpisodeCoordinates {
            order: coordinates.order,
            season: coordinates.season,
            episode,
        })?
    } else {
        show.canonical_episode(&coordinates)?
    };
    let id = format!("{0}:{1}:{2}", show.identity().id, tvdb.season, tvdb.episode);
    let mut value = value.clone();
    value["id"] = json!(id);
    value["series_id"] = json!(show.identity().id);
    value["season"] = json!(tvdb.season);
    value["episode"] = json!(tvdb.episode);
    value["engine_identity"] = episode_identity(&id, &show.identity().ids, tvdb);
    value["engine_parent_identity"] = parent_identity(show.identity());
    Some(value)
}

pub(crate) async fn show_episode(
    app: &App,
    parent: &MediaIdentity,
    episode: &str,
    coordinates: &EpisodeCoordinates,
) -> Option<(MediaIdentity, TvdbEpisode)> {
    if parent.kind != "series" || parent.episode.is_some() || coordinates.episode == 0 {
        return None;
    }
    let show = match resolve(app, parent).await {
        Some(show) => show,
        None => resolve(app, &anime_source(parent)?).await?,
    };
    let tvdb = TvdbEpisode {
        season: coordinates.season?,
        episode: coordinates.episode,
    };
    if !episode.starts_with(&format!("{}:", show.identity().id)) || !show.lists(tvdb) {
        return None;
    }
    Some((show.identity().clone(), tvdb))
}

fn anime_source(identity: &MediaIdentity) -> Option<MediaIdentity> {
    let (namespace, value) = identity.ids.iter().find_map(|id| {
        ANIME_SOURCES
            .contains(&id.namespace.as_str())
            .then_some((id.namespace.as_str(), id.value.as_str()))
    })?;
    Some(MediaIdentity {
        id: format!("{namespace}:{value}"),
        kind: "series".into(),
        ids: identity.ids.clone(),
        episode: None,
        edition: None,
    })
}

fn episode_coordinates(value: &Value) -> Option<EpisodeCoordinates> {
    if let Some(episode) = value["engine_identity"]["episode"].as_object() {
        return Some(EpisodeCoordinates {
            order: episode["order"].as_str().unwrap_or("aired").to_owned(),
            season: episode["season"]
                .as_u64()
                .and_then(|season| u32::try_from(season).ok()),
            episode: episode["episode"]
                .as_u64()
                .and_then(|episode| u32::try_from(episode).ok())?,
        });
    }
    Some(EpisodeCoordinates {
        order: "aired".into(),
        season: value["season"]
            .as_u64()
            .and_then(|season| u32::try_from(season).ok()),
        episode: value["episode"]
            .as_u64()
            .and_then(|episode| u32::try_from(episode).ok())?,
    })
}

pub(crate) fn release_identity(value: &Value) -> Option<MediaIdentity> {
    if let Ok(parent) = serde_json::from_value::<MediaIdentity>(value["engine_parent_identity"].clone())
    {
        if parent.kind == "series" && parent.episode.is_none() && !parent.id.is_empty() {
            return Some(parent);
        }
    }
    let id = value["series_id"].as_str()?;
    Some(MediaIdentity {
        id: id.to_owned(),
        kind: "series".into(),
        ids: Vec::new(),
        episode: None,
        edition: None,
    })
}

fn episode_identity(id: &str, ids: &[ExternalId], tvdb: TvdbEpisode) -> Value {
    let mut identity = parent_identity(&MediaIdentity {
        id: id.to_owned(),
        kind: "series".into(),
        ids: ids.to_vec(),
        episode: None,
        edition: None,
    });
    identity["episode"] = json!({
        "order": "aired",
        "season": tvdb.season,
        "episode": tvdb.episode,
    });
    identity
}

fn parent_identity(show: &MediaIdentity) -> Value {
    json!({
        "id": show.id,
        "kind": "series",
        "ids": show.ids.clone(),
        "episode": null,
        "edition": null,
    })
}

pub(crate) async fn resolve(app: &App, release: &MediaIdentity) -> Option<AnimeShow> {
    resolve_with(app, release, None).await
}

async fn resolve_with(
    app: &App,
    release: &MediaIdentity,
    mut release_document: Option<Value>,
) -> Option<AnimeShow> {
    let simkl_identity = simkl_anime(&release.id);
    if release.kind != "series" || release.episode.is_some() || !anime_release(&release.id) {
        return None;
    }
    let mut identity = MediaIdentity {
        id: release.id.clone(),
        kind: "series".into(),
        ids: release.ids.clone(),
        episode: None,
        edition: None,
    };
    let mut videos = Vec::new();
    let mut simkl = simkl_identity;
    if let Some(id) = simkl {
        match simkl_record(app, id).await {
            Some(record) => provider_identity::merge(&mut identity, &record),
            None => simkl = None,
        }
    } else {
        if release_document.is_none() {
            release_document = app
                .addons
                .meta(&release.kind, &release.id)
                .await
                .ok()
                .map(meta_of);
        }
        if let Some(meta) = release_document.as_ref() {
            provider_identity::merge(&mut identity, meta);
            videos = meta["videos"].as_array().cloned().unwrap_or_default();
        }
    }
    if simkl.is_none() && simkl_identity.is_none() {
        let found = simkl_lookup(app, &identity).await;
        if let Some((id, record)) = found {
            provider_identity::merge(&mut identity, &record);
            simkl = Some(id);
        }
    }
    let mut identity = show_identity(identity)?;
    let document = show_document(app, &mut identity).await;
    if !mapped(&identity) {
        return None;
    }
    let layout = if simkl_identity.is_some() {
        Layout::Aired
    } else {
        let mut segments = match simkl {
            Some(id) if !spans_seasons(&videos) => simkl_episodes(app, id)
                .await
                .map(|rows| segments(&rows))
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        if segments.is_empty() && simkl.is_none() {
            segments = release_segments(&release.id, &videos);
        }
        Layout::Segments {
            release_season: single_season(&videos),
            segments,
        }
    };
    Some(AnimeShow {
        identity,
        layout,
        document,
    })
}

fn single_season(videos: &[Value]) -> Option<u32> {
    let seasons = season_numbers(videos);
    match seasons.len() {
        1 => seasons
            .first()
            .copied()
            .and_then(|season| u32::try_from(season).ok()),
        _ => None,
    }
}

fn spans_seasons(videos: &[Value]) -> bool {
    season_numbers(videos).len() > 1
}

fn season_numbers(videos: &[Value]) -> std::collections::BTreeSet<u64> {
    videos
        .iter()
        .filter_map(|video| video["season"].as_u64())
        .collect()
}

fn segments(rows: &[Value]) -> Vec<Segment> {
    segments_from(rows.iter().take(MAX_ROWS).filter_map(|row| {
        if row["type"].as_str() != Some("episode") || row["aired"] == false {
            return None;
        }
        let number = u32::try_from(row["episode"].as_u64()?).ok()?;
        let season = u32::try_from(row["tvdb"]["season"].as_u64()?).ok()?;
        let episode = u32::try_from(row["tvdb"]["episode"].as_u64()?).ok()?;
        Some((number, season, episode))
    }))
}

fn release_segments(release: &str, videos: &[Value]) -> Vec<Segment> {
    segments_from(videos.iter().take(MAX_ROWS).filter_map(|video| {
        let number = episode_suffix(video["id"].as_str()?, release)?;
        let (season, episode) = show_coordinates(video)?;
        Some((number, season, episode))
    }))
}

fn episode_suffix(id: &str, release: &str) -> Option<u32> {
    let suffix = id.strip_prefix(release)?.strip_prefix(':')?;
    if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    suffix.parse().ok()
}

fn show_coordinates(video: &Value) -> Option<(u32, u32)> {
    let season = video["imdbSeason"]
        .as_u64()
        .or_else(|| video["season"].as_u64())?;
    let episode = video["imdbEpisode"]
        .as_u64()
        .or_else(|| video["episode"].as_u64())?;
    Some((u32::try_from(season).ok()?, u32::try_from(episode).ok()?))
}

fn segments_from(items: impl Iterator<Item = (u32, u32, u32)>) -> Vec<Segment> {
    let mut pairs = std::collections::BTreeMap::new();
    let mut conflicting = HashSet::new();
    for (number, season, episode) in items {
        if !(1..=MAX_EPISODES).contains(&number) || episode == 0 {
            continue;
        }
        if pairs
            .insert(number, (season, episode))
            .is_some_and(|prior| prior != (season, episode))
        {
            conflicting.insert(number);
        }
    }
    pairs.retain(|number, _| !conflicting.contains(number));
    let mut segments: Vec<Segment> = Vec::new();
    for (number, (season, episode)) in pairs {
        let offset = i64::from(episode) - i64::from(number);
        match segments.last_mut() {
            Some(last)
                if last.season == season && last.offset == offset && last.last + 1 == number =>
            {
                last.last = number;
            }
            _ => segments.push(Segment {
                season,
                first: number,
                last: number,
                offset,
            }),
        }
    }
    segments
}

async fn show_document(app: &App, identity: &mut MediaIdentity) -> Option<Value> {
    let id = ["imdb", "tvdb", "tmdb"]
        .into_iter()
        .find_map(|namespace| {
            let value = id_of(identity, namespace)?;
            Some(if namespace == "imdb" {
                value.to_owned()
            } else {
                format!("{namespace}:{value}")
            })
        })?;
    let meta = meta_of(app.addons.meta("series", &id).await.ok()?);
    if !provider_identity::same_title(&meta, &id)
        || meta["videos"].as_array().is_none_or(Vec::is_empty)
    {
        return None;
    }
    provider_identity::merge(identity, &meta);
    Some(meta)
}

async fn simkl_lookup(app: &App, identity: &MediaIdentity) -> Option<(u64, Arc<Value>)> {
    for namespace in ANIME_SOURCES {
        let Some(value) = id_of(identity, namespace) else {
            continue;
        };
        let Ok(document) =
            crate::simkl::public_document(app, &format!("/search/id?{namespace}={value}")).await
        else {
            continue;
        };
        let Some([hit]) = document.as_array().map(Vec::as_slice) else {
            continue;
        };
        let Some(id) = viptv_engine::simkl::id(hit) else {
            continue;
        };
        let Some(record) = simkl_record(app, id).await else {
            continue;
        };
        if provider_identity::ids(&record)
            .iter()
            .any(|id| id.namespace == namespace && id.value == value)
        {
            return Some((id, record));
        }
    }
    None
}

async fn simkl_record(app: &App, id: u64) -> Option<Arc<Value>> {
    let document = crate::simkl::public_document(app, &format!("/anime/{id}?extended=full"))
        .await
        .ok()?;
    (viptv_engine::simkl::id(&document) == Some(id)).then_some(document)
}

async fn simkl_episodes(app: &App, id: u64) -> Option<Vec<Value>> {
    let document = crate::simkl::public_document(app, &format!("/anime/episodes/{id}"))
        .await
        .ok()?;
    Some(document.as_array()?.clone())
}

fn simkl_anime(id: &str) -> Option<u64> {
    match id.split(':').collect::<Vec<_>>().as_slice() {
        ["simkl", "anime", number] => number.parse().ok(),
        _ => None,
    }
}

fn meta_of(mut document: Value) -> Value {
    if document["meta"].is_object() {
        document["meta"].take()
    } else {
        document
    }
}

fn id_of<'a>(identity: &'a MediaIdentity, namespace: &str) -> Option<&'a str> {
    identity
        .ids
        .iter()
        .find(|id| id.namespace == namespace)
        .map(|id| id.value.as_str())
}

fn mapped(identity: &MediaIdentity) -> bool {
    ["tvdb", "tmdb"]
        .into_iter()
        .any(|namespace| id_of(identity, namespace).is_some())
}

fn show_identity(mut identity: MediaIdentity) -> Option<MediaIdentity> {
    let id = ["imdb", "tvdb", "tmdb"].into_iter().find_map(|namespace| {
        let value = id_of(&identity, namespace)?;
        Some(match namespace {
            "imdb" => value.to_owned(),
            _ => format!("{namespace}:{value}"),
        })
    })?;
    identity.id = id;
    Some(identity)
}

#[cfg(test)]
mod season_tests {
    use super::*;
    use crate::*;
    use viptv_engine::presentation::Card;

    #[test]
    fn season_markers_split_off_the_base_title() {
        assert_eq!(
            super::split_title_season("Mob Psycho 100 III"),
            ("Mob Psycho 100".to_owned(), Some(3))
        );
        assert_eq!(
            super::split_title_season("Attack on Titan Season 2"),
            ("Attack on Titan".to_owned(), Some(2))
        );
        assert_eq!(
            super::split_title_season("One Punch Man 2"),
            ("One Punch Man".to_owned(), Some(2))
        );
        assert_eq!(
            super::split_title_season("Bleach S3"),
            ("Bleach".to_owned(), Some(3))
        );
        assert_eq!(
            super::split_title_season("Naruto Part 2"),
            ("Naruto".to_owned(), Some(2))
        );
        assert_eq!(
            super::split_title_season("Tower of God"),
            ("Tower of God".to_owned(), None)
        );
    }

    fn card(id: &str, title: &str, alt: &[&str], year: Option<u32>) -> Card {
        serde_json::from_value(json!({
            "identity": {"id": id, "kind": "series", "ids": [{"namespace": "imdb", "value": id}], "episode": null, "edition": null},
            "provider_id": "stremio",
            "title": title,
            "badges": [],
            "metadata": {"alternate_titles": alt, "year": year},
        }))
        .unwrap()
    }

    #[test]
    fn title_match_accepts_one_exact_or_alternate_match_with_a_close_year() {
        let hero = card("tt1111111", "Hero Show", &["Brave Story"], Some(2020));
        assert_eq!(
            super::title_match(&[hero.clone()], "Hero Show", Some(2020))
                .map(|card| card.identity.id.as_str()),
            Some("tt1111111")
        );
        assert_eq!(
            super::title_match(&[hero.clone()], "Brave Story", Some(2020))
                .map(|card| card.identity.id.as_str()),
            Some("tt1111111")
        );
        assert!(super::title_match(&[hero.clone()], "Brave Story", Some(2022)).is_none());
        let twin = card("tt2222222", "Hero Show", &[], Some(2020));
        assert!(super::title_match(&[hero, twin], "Hero Show", Some(2020)).is_none());
    }
}
