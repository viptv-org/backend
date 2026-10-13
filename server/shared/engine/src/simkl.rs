//! Simkl identities are provider-qualified; title IDs never become episode IDs.
mod mapping;
use crate::{
    Error,
    identity::{EpisodeCoordinates, ExternalId, MediaIdentity},
    presentation::Card,
};
pub use mapping::source_identity;
use serde_json::Value;

pub fn category(value: &str) -> Result<&'static str, Error> {
    match value {
        "movie" => Ok("movie"),
        "tv" => Ok("tv"),
        "anime" => Ok("anime"),
        _ => Err(Error::InvalidQuery),
    }
}

pub fn id(value: &Value) -> Option<u64> {
    let primary = value["ids"]["simkl"].as_u64();
    let alternate = value["ids"]["simkl_id"].as_u64();
    if primary.zip(alternate).is_some_and(|(a, b)| a != b) {
        return None;
    }
    primary.or(alternate).filter(|id| *id > 0)
}

fn text(value: &Value, limit: usize) -> Option<String> {
    value
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.chars().filter(|c| !c.is_control()).take(limit).collect())
}
fn image(value: &Value, folder: &str, suffix: &str) -> Option<String> {
    let path = value.as_str()?;
    if path.is_empty()
        || path.len() > 128
        || !path.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'/')
    {
        return None;
    }
    Some(format!(
        "https://wsrv.nl/?url=https://simkl.in/{folder}/{path}_{suffix}.webp"
    ))
}

pub fn card(value: &Value, category: &str) -> Result<Card, Error> {
    let category = self::category(category)?;
    if value["endpoint_type"]
        .as_str()
        .is_some_and(|kind| kind != category && !(category == "movie" && kind == "movies"))
    {
        return Err(Error::Unresolved);
    }
    if value["type"].as_str().is_some_and(|kind| !match category {
        "movie" => kind == "movie",
        "tv" => matches!(kind, "show" | "tv"),
        "anime" => matches!(
            kind,
            "anime" | "tv" | "movie" | "ova" | "ona" | "special" | "music"
        ),
        _ => false,
    }) {
        return Err(Error::Unresolved);
    }
    let id = id(value).ok_or(Error::Unresolved)?;
    let title = text(&value["title"], 512).ok_or(Error::Unresolved)?;
    let movie = category == "movie"
        || category == "anime"
            && (value["anime_type"].as_str() == Some("movie")
                || value["type"].as_str() == Some("movie"));
    let mut ids = vec![ExternalId {
        namespace: "simkl".into(),
        value: id.to_string(),
    }];
    if let Some(imdb) = value["ids"]["imdb"].as_str().filter(|s| {
        s.len() <= 16
            && s.strip_prefix("tt")
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    }) {
        ids.push(ExternalId {
            namespace: "imdb".into(),
            value: imdb.into(),
        });
    }
    Ok(Card {
        identity: MediaIdentity {
            id: format!("simkl:{category}:{id}"),
            kind: if movie { "movie" } else { "series" }.into(),
            ids,
            episode: None,
            edition: None,
        },
        category: Some(if category == "tv" { "series" } else { category }.into()),
        metadata: Some(metadata(value)),
        provider_id: "simkl".into(),
        title,
        subtitle: year(value).map(|year| year.to_string()),
        description: text(&value["overview"], 16_384),
        poster: image(&value["poster"], "posters", "m"),
        landscape: image(&value["fanart"], "fanart", "w"),
        logo: None,
        progress: None,
        badges: vec!["Simkl".into()],
        parent_identity: None,
        added_at_millis: None,
    })
}

/// TV coordinates come from the provider's episode list; missing or duplicated coordinates fail closed.
pub fn episodes(value: &Value, parent: &Card) -> Result<Vec<Card>, Error> {
    let anime = parent.identity.id.starts_with("simkl:anime:");
    if !parent.identity.id.starts_with("simkl:tv:") && !anime {
        return Err(Error::Unresolved);
    }
    let values = value.as_array().ok_or(Error::Unresolved)?;
    if values.len() > 10_000 {
        return Err(Error::InvalidQuery);
    }
    let mut seen = std::collections::HashSet::new();
    let mut cards = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for value in values {
        if anime && (value["type"].as_str() != Some("episode") || value["aired"] == false) {
            continue;
        }
        let coordinates = if anime { &value["tvdb"] } else { value };
        let Some((season, episode)) = coordinates["season"]
            .as_u64()
            .zip(coordinates["episode"].as_u64())
        else {
            continue;
        };
        let season = u32::try_from(season).map_err(|_| Error::Unresolved)?;
        let episode = u32::try_from(episode).map_err(|_| Error::Unresolved)?;
        if episode == 0 || !seen.insert((season, episode)) {
            return Err(Error::Unresolved);
        }
        let episode_id = id(value).ok_or(Error::Unresolved)?;
        if !ids.insert(episode_id) {
            return Err(Error::Unresolved);
        }
        let mut card = parent.clone();
        card.parent_identity = Some(parent.identity.clone());
        card.identity.id = format!("{}:{season}:{episode}", parent.identity.id);
        card.identity.ids = vec![ExternalId {
            namespace: "simkl_episode".into(),
            value: episode_id.to_string(),
        }];
        card.identity.episode = Some(EpisodeCoordinates {
            order: "aired".into(),
            season: Some(season),
            episode,
        });
        card.title = text(&value["title"], 512).unwrap_or_else(|| format!("Episode {episode}"));
        card.subtitle = Some(format!("S{season} E{episode}"));
        card.description =
            text(&value["description"], 16_384).or_else(|| text(&value["overview"], 16_384));
        card.landscape = image(&value["img"], "episodes", "w");
        cards.push(card);
    }
    cards.sort_by_key(|card| {
        let e = card.identity.episode.as_ref().unwrap();
        (e.season, e.episode)
    });
    Ok(cards)
}

fn metadata(value: &Value) -> crate::presentation::CardMetadata {
    let strings = |v: &Value, max: usize| {
        v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| text(v, 512))
            .take(max)
            .collect::<Vec<_>>()
    };
    let mut alternate_titles = strings(&value["all_titles"], 32);
    for key in ["en_title", "title_en", "title_romaji"] {
        if let Some(s) = text(&value[key], 512) {
            alternate_titles.push(s);
        }
    }
    for value in value["alt_titles"]
        .as_array()
        .into_iter()
        .flatten()
        .take(32)
    {
        if let Some(s) = text(&value["name"], 512) {
            alternate_titles.push(s);
        }
    }
    alternate_titles.sort();
    alternate_titles.dedup();
    alternate_titles.truncate(32);
    let ratings = ["imdb", "simkl", "mal"].into_iter().find_map(|key| {
        value["ratings"][key]["rating"]
            .as_f64()
            .filter(|n| n.is_finite() && (0.0..=10.0).contains(n))
            .map(|rating| (rating, value["ratings"][key]["votes"].as_u64()))
    });
    crate::presentation::CardMetadata {
        year: year(value),
        runtime_minutes: runtime_minutes(&value["runtime"]),
        genres: strings(&value["genres"], 16),
        rating: ratings.map(|r| r.0),
        rating_votes: ratings.and_then(|r| r.1),
        certification: text(&value["certification"], 128),
        alternate_titles,
    }
}

/// Recommendations are already included in the authoritative full detail record.
pub fn recommendations(value: &Value, parent_category: &str, parent_id: &str) -> Vec<Card> {
    let mut seen = std::collections::HashSet::new();
    ["users_recommendations", "similar", "relations"]
        .into_iter()
        .flat_map(|key| value[key].as_array().into_iter().flatten())
        .take(200)
        .filter_map(|v| {
            let category = match v["endpoint_type"].as_str().or_else(|| v["type"].as_str()) {
                Some("movie" | "movies") => "movie",
                Some("tv" | "show") => "tv",
                Some("anime") => "anime",
                None if parent_category == "anime" => "anime",
                _ => return None,
            };
            let category = if parent_category == "anime" && v["anime_type"].is_string() {
                "anime"
            } else {
                category
            };
            let c = card(v, category).ok()?;
            (c.identity.id != parent_id && seen.insert(c.identity.id.clone())).then_some(c)
        })
        .take(24)
        .collect()
}

fn year(value: &Value) -> Option<u32> {
    if let Some(year) = value["year"]
        .as_u64()
        .or_else(|| value["year"].as_str().and_then(|s| s.parse().ok()))
        .filter(|n| *n > 0 && *n <= 9999)
    {
        return Some(year as u32);
    }
    for key in ["release_date", "released", "first_aired"] {
        let Some(date) = value[key].as_str() else {
            continue;
        };
        if !date.is_ascii(){continue;}
        let parts = if date.len() == 10 && date.as_bytes()[2] == b'/' && date.as_bytes()[5] == b'/'
        {
            Some((&date[6..10], &date[0..2], &date[3..5]))
        } else if date.len() >= 10 && date.as_bytes()[4] == b'-' && date.as_bytes()[7] == b'-' {
            Some((&date[0..4], &date[5..7], &date[8..10]))
        } else {
            None
        };
        let Some((year, month, day)) = parts else {
            continue;
        };
        let (Ok(year), Ok(month), Ok(day)) = (
            year.parse::<u32>(),
            month.parse::<u32>(),
            day.parse::<u32>(),
        ) else {
            continue;
        };
        let max = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 => {
                if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
                    29
                } else {
                    28
                }
            }
            _ => 0,
        };
        if year > 0 && year <= 9999 && day > 0 && day <= max {
            return Some(year);
        }
    }
    None
}
fn runtime_minutes(value: &Value) -> Option<u32> {
    if let Some(n) = value.as_u64().filter(|n| *n > 0 && *n <= 10000) {
        return Some(n as u32);
    }
    let s = value.as_str()?.trim();
    if s.len() > 64 {
        return None;
    }
    if let Ok(n) = s.parse::<u32>() {
        return (n > 0 && n <= 10000).then_some(n);
    }
    let mut hours = None;
    let mut minutes = None;
    for word in s.split_whitespace() {
        if let Some(n) = word.strip_suffix('h') {
            if hours.is_some() {
                return None;
            }
            hours = Some(n.parse::<u32>().ok()?);
        } else if let Some(n) = word.strip_suffix('m') {
            if minutes.is_some() {
                return None;
            }
            minutes = Some(n.parse::<u32>().ok()?);
        } else {
            return None;
        }
    }
    let total = hours
        .unwrap_or(0)
        .checked_mul(60)?
        .checked_add(minutes.unwrap_or(0))?;
    (total > 0 && total <= 10000).then_some(total)
}
