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
        "anime" => kind == "anime",
        _ => false,
    }) {
        return Err(Error::Unresolved);
    }
    let id = id(value).ok_or(Error::Unresolved)?;
    let title = text(&value["title"], 512).ok_or(Error::Unresolved)?;
    let movie =
        category == "movie" || category == "anime" && value["anime_type"].as_str() == Some("movie");
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
        provider_id: "simkl".into(),
        title,
        subtitle: value["year"]
            .as_u64()
            .filter(|year| *year <= 9999)
            .map(|year| year.to_string()),
        description: text(&value["overview"], 16_384),
        poster: image(&value["poster"], "posters", "m"),
        landscape: image(&value["fanart"], "fanart", "w"),
        logo: None,
        progress: None,
        badges: vec!["Simkl".into()],
    })
}

/// TV coordinates come from the provider's episode list; missing or duplicated coordinates fail closed.
pub fn episodes(value: &Value, parent: &Card) -> Result<Vec<Card>, Error> {
    if !parent.identity.id.starts_with("simkl:tv:") {
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
        let Some((season, episode)) = value["season"].as_u64().zip(value["episode"].as_u64())
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
