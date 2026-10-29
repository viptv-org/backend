use super::*;
use viptv_engine::identity::{EpisodeCoordinates, MediaIdentity};

const ANIME_ADDONS: [&str; 4] = ["mal:", "kitsu:", "anilist:", "anidb:"];
const ANIME_SOURCES: [&str; 4] = ["mal", "kitsu", "anilist", "anidb"];
const MAX_EPISODES: u32 = 10_000;
const MAX_ROWS: usize = 10_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AnimeShow {
    identity: MediaIdentity,
    layout: Layout,
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
}

pub(crate) async fn resolve(app: &App, release: &MediaIdentity) -> Option<AnimeShow> {
    let simkl_identity = simkl_anime(&release.id);
    let anime_addon = ANIME_ADDONS
        .iter()
        .any(|prefix| release.id.starts_with(*prefix));
    if release.kind != "series" || release.episode.is_some() || simkl_identity.is_none() && !anime_addon
    {
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
    } else if let Ok(document) = app.addons.meta(&release.kind, &release.id).await {
        let meta = meta_of(document);
        provider_identity::merge(&mut identity, &meta);
        videos = meta["videos"].as_array().cloned().unwrap_or_default();
    }
    if !mapped(&identity) {
        if let Some(imdb) = id_of(&identity, "imdb").map(str::to_owned) {
            if let Ok(document) = app.addons.meta("series", &imdb).await {
                provider_identity::merge(&mut identity, &meta_of(document));
            }
        }
    }
    if simkl.is_none() && simkl_identity.is_none() {
        let found = simkl_lookup(app, &identity).await;
        if let Some((id, record)) = found {
            provider_identity::merge(&mut identity, &record);
            simkl = Some(id);
        }
    }
    if !mapped(&identity) {
        return None;
    }
    let layout = if simkl_identity.is_some() {
        Layout::Aired
    } else {
        let segments = if spans_seasons(&videos) {
            Vec::new()
        } else {
            match simkl {
                Some(id) => simkl_episodes(app, id)
                    .await
                    .map(|rows| segments(&rows))
                    .unwrap_or_default(),
                None => Vec::new(),
            }
        };
        Layout::Segments {
            release_season: single_season(&videos),
            segments,
        }
    };
    Some(AnimeShow {
        identity: show_identity(identity)?,
        layout,
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

/// Release episode numbers count the way Simkl's `episode` numbers do inside one anime title.
fn segments(rows: &[Value]) -> Vec<Segment> {
    let mut pairs = std::collections::BTreeMap::new();
    let mut conflicting = HashSet::new();
    for row in rows.iter().take(MAX_ROWS) {
        if row["type"].as_str() != Some("episode") || row["aired"] == false {
            continue;
        }
        let (Some(number), Some(season), Some(episode)) = (
            row["episode"]
                .as_u64()
                .and_then(|number| u32::try_from(number).ok()),
            row["tvdb"]["season"]
                .as_u64()
                .and_then(|season| u32::try_from(season).ok()),
            row["tvdb"]["episode"]
                .as_u64()
                .and_then(|episode| u32::try_from(episode).ok()),
        ) else {
            continue;
        };
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
