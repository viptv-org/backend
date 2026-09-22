//! Xtream candidate parsing, validation, and ranking shared by the backend's
//! provider service and any fat client that needs to produce playable candidates.
use crate::normalize::{
    canonical_id, extension, imdb_id, normalize, required_string, scalar, stream_id, text,
    timestamp, title_year, tmdb_id, valid_year,
};
use serde_json::Value;
use std::collections::HashSet;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Candidate {
    pub id: String,
    pub provider_id: i64,
    pub stream_id: String,
    pub kind: String,
    pub name: String,
    pub normalized: String,
    pub year: Option<i64>,
    pub imdb_id: Option<String>,
    pub tmdb_id: Option<String>,
    pub extension: String,
    pub poster: Option<String>,
    pub override_id: Option<String>,
}

pub fn candidate_from_json(provider_id: i64, kind: &str, value: &Value) -> Option<Candidate> {
    let stream_id = stream_id(
        value,
        if kind == "series" {
            "series_id"
        } else {
            "stream_id"
        },
    )?;
    let name = value.get("name")?.as_str()?.trim();
    let (title, suffix_year) = title_year(name);
    // Keep the provider title (not the display fallback) as the matching input.
    let name = if name.is_empty() {
        format!("Untitled {kind} #{stream_id}")
    } else {
        name.to_owned()
    };
    let year = ["year", "releaseDate", "release_date"]
        .iter()
        .find_map(|k| value.get(*k).and_then(valid_year))
        .or(suffix_year);
    Some(Candidate {
        id: format!("iptv:{provider_id}:{kind}:{stream_id}"),
        provider_id,
        stream_id,
        kind: kind.into(),
        name,
        normalized: normalize(&title),
        year,
        imdb_id: imdb_id(value.get("imdb_id")).or_else(|| imdb_id(value.get("imdb"))),
        tmdb_id: tmdb_id(value.get("tmdb_id")).or_else(|| tmdb_id(value.get("tmdb"))),
        extension: extension(value.get("container_extension").and_then(Value::as_str)),
        poster: text(value, "stream_icon").or_else(|| text(value, "cover")),
        override_id: None,
    })
}

// Lazy discovery must not use an ID match to erase contradictory year/namespace evidence.
pub fn evidence_conflicts(c: &Candidate, r: &MatchRequest) -> bool {
    c.year.zip(r.year).is_some_and(|(a, b)| a != b)
        || [(&c.imdb_id, "tt"), (&c.tmdb_id, "tmdb:")]
            .iter()
            .any(|(id, prefix)| {
                id.as_ref().is_some_and(|id| {
                    r.ids.iter().any(|v| v.starts_with(prefix)) && !r.ids.contains(id)
                })
            })
}

pub fn validated_details(c: &Candidate, details: &Value) -> Option<Candidate> {
    let info = details.get("info")?.as_object()?;
    if details.get("movie_data").is_some_and(|v| !v.is_object()) {
        return None;
    }
    let mut enriched = c.clone();
    let mut named = false;
    for object in [
        Some(info),
        details.get("movie_data").and_then(Value::as_object),
    ]
    .into_iter()
    .flatten()
    {
        for key in ["stream_id", "vod_id", "series_id"] {
            if let Some(value) = object.get(key) {
                if scalar(value).as_deref() != Some(c.stream_id.as_str()) {
                    return None;
                }
            }
        }
        for key in ["name", "title"] {
            if let Some(value) = object.get(key).filter(|v| !v.is_null()) {
                let name = value.as_str()?.trim();
                if name.is_empty() {
                    continue;
                }
                let (title, year) = title_year(name);
                if normalize(&title) != c.normalized || c.normalized.is_empty() {
                    return None;
                }
                named = true;
                if let Some(year) = year {
                    if enriched.year.is_some_and(|old| old != year) {
                        return None;
                    }
                    enriched.year = Some(year);
                }
            }
        }
        for key in ["year", "releasedate", "releaseDate", "release_date"] {
            if let Some(value) = object
                .get(key)
                .filter(|v| !v.is_null() && v.as_str() != Some(""))
            {
                let raw = scalar(value)?;
                if raw.len() != 4 {
                    let bytes = raw.as_bytes();
                    if bytes.len() != 10
                        || bytes[4] != b'-'
                        || bytes[7] != b'-'
                        || !bytes
                            .iter()
                            .enumerate()
                            .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
                        || !(1..=12).contains(&raw[5..7].parse::<u32>().ok()?)
                        || !(1..=31).contains(&raw[8..10].parse::<u32>().ok()?)
                    {
                        return None;
                    }
                }
                let year = valid_year(value)?;
                if enriched.year.is_some_and(|old| old != year) {
                    return None;
                }
                enriched.year = Some(year);
            }
        }
        for (keys, target, parse) in [
            (
                ["imdb_id", "imdb"],
                &mut enriched.imdb_id,
                imdb_id as fn(Option<&Value>) -> Option<String>,
            ),
            (
                ["tmdb_id", "tmdb"],
                &mut enriched.tmdb_id,
                tmdb_id as fn(Option<&Value>) -> Option<String>,
            ),
        ] {
            for key in keys {
                if let Some(value) = object
                    .get(key)
                    .filter(|v| !v.is_null() && v.as_str() != Some(""))
                {
                    let id = parse(Some(value))?;
                    if target.as_ref().is_some_and(|old| old != &id) {
                        return None;
                    }
                    *target = Some(id);
                }
            }
        }
    }
    named.then_some(enriched)
}

#[derive(Clone)]
pub struct MatchRequest {
    pub ids: HashSet<String>,
    pub normalized: Option<String>,
    pub year: Option<i64>,
    pub season: Option<i64>,
    pub episode: Option<i64>,
}
impl MatchRequest {
    pub fn parse(value: &Value, kind: &str) -> Result<Self, String> {
        let mut id = required_string(value, "id", 256)?;
        let number = |key: &str| -> Result<Option<i64>, String> {
            match value.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(v) => timestamp(v)
                    .map(Some)
                    .ok_or_else(|| format!("Invalid {key}")),
            }
        };
        let mut season = number("season")?;
        let mut episode = number("episode")?;
        if kind == "series" {
            // Strip only two numeric suffixes, preserving namespaced IDs such as tmdb:123.
            let pieces: Vec<&str> = id.rsplitn(3, ':').collect();
            if pieces.len() == 3 {
                if let (Ok(e), Ok(s)) = (pieces[0].parse::<i64>(), pieces[1].parse::<i64>()) {
                    if s < 0 || e < 0 {
                        return Err("Invalid season or episode".into());
                    }
                    if season.is_some_and(|v| v != s) || episode.is_some_and(|v| v != e) {
                        return Err("Episode ID conflicts with season or episode".into());
                    }
                    season = Some(s);
                    episode = Some(e);
                    id = pieces[2].to_owned();
                }
            }
        }
        let mut ids = HashSet::from([canonical_id(&id)]);
        if let Some(id) = imdb_id(value.get("imdb_id")) {
            ids.insert(id);
        }
        if let Some(id) = tmdb_id(value.get("tmdb_id")) {
            ids.insert(id);
        }
        let title = text(value, "name").map(|s| title_year(&s));
        let year = value
            .get("year")
            .and_then(valid_year)
            .or_else(|| title.as_ref().and_then(|t| t.1));
        let normalized = title.map(|t| normalize(&t.0)).filter(|t| !t.is_empty());
        Ok(Self {
            ids,
            normalized,
            year,
            season,
            episode,
        })
    }
}

pub fn select_candidates<'a>(
    candidates: &'a [Candidate],
    request: &MatchRequest,
) -> Vec<&'a Candidate> {
    candidates
        .iter()
        .filter(|c| {
            // A manual mapping is authoritative, including when it rules a candidate out.
            if let Some(id) = &c.override_id {
                return request.ids.contains(id);
            }
            if c.imdb_id
                .as_ref()
                .is_some_and(|id| request.ids.contains(id))
                || c.tmdb_id
                    .as_ref()
                    .is_some_and(|id| request.ids.contains(id))
            {
                return true;
            }
            // Do not override contradictory IDs from the same metadata namespace with a title.
            if c.imdb_id.is_some() && request.ids.iter().any(|id| id.starts_with("tt")) {
                return false;
            }
            if c.tmdb_id.is_some() && request.ids.iter().any(|id| id.starts_with("tmdb:")) {
                return false;
            }
            request.year.is_some()
                && request.year == c.year
                && request
                    .normalized
                    .as_ref()
                    .is_some_and(|n| n == &c.normalized)
        })
        .collect()
}

pub fn episode_rows(info: &Value, season: i64, episode: i64) -> Vec<&Value> {
    let Some(episodes) = info.get("episodes") else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    if let Some(seasons) = episodes.as_object() {
        if let Some(items) = seasons.get(&season.to_string()).and_then(Value::as_array) {
            rows.extend(items.iter().filter(|v| {
                v.get("episode_num").and_then(timestamp) == Some(episode)
                    && v.get("season").is_none_or(|s| timestamp(s) == Some(season))
            }));
        }
    } else if let Some(items) = episodes.as_array() {
        rows.extend(items.iter().filter(|v| {
            v.get("season").and_then(timestamp) == Some(season)
                && v.get("episode_num").and_then(timestamp) == Some(episode)
        }));
    }
    rows
}
