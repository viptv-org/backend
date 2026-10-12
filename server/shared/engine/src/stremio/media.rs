use crate::{
    Error,
    identity::{EpisodeCoordinates, ExternalId, MediaIdentity},
    presentation::{Card, DetailSnapshot},
};
use serde_json::Value;
use viptv_provider::extras::bounded_exact_text;

pub fn detail(
    raw: &str,
    provider: &str,
    kind: &str,
    id: &str,
    offset: usize,
) -> Result<DetailSnapshot, Error> {
    if raw.len() > 2 * 1024 * 1024 || offset > 100_000 {
        return Err(Error::InvalidQuery);
    }
    let root: Value = serde_json::from_str(raw).map_err(|_| Error::InvalidQuery)?;
    let meta = if root["meta"].is_object() {
        &root["meta"]
    } else {
        &root
    };
    if meta["id"].as_str() != Some(id) {
        return Err(Error::Unresolved);
    }
    let mut parent = meta.clone();
    parent["id"] = Value::String(id.into());
    parent["type"] = Value::String(kind.into());
    let item = cards(&serde_json::json!({"metas":[parent]}).to_string(), provider)?
        .into_iter()
        .next()
        .ok_or(Error::Unresolved)?;
    let all = meta["videos"].as_array().cloned().unwrap_or_default();
    let mut episodes = Vec::new();
    for video in all.iter().skip(offset).take(24) {
        let mut normalized = video.clone();
        normalized["type"] = Value::String("series".into());
        if normalized["name"].is_null() {
            normalized["name"] = normalized["title"].clone();
        }
        if normalized["background"].is_null() {
            normalized["background"] = normalized["thumbnail"].clone();
        }
        if let Some(mut card) = cards(
            &serde_json::json!({"metas":[normalized]}).to_string(),
            provider,
        )?
        .into_iter()
        .next()
        {
            if let (Some(season), Some(episode)) =
                (video["season"].as_u64(), video["episode"].as_u64())
            {
                card.identity.episode = Some(EpisodeCoordinates {
                    order: "aired".into(),
                    season: u32::try_from(season).ok(),
                    episode: u32::try_from(episode).map_err(|_| Error::InvalidQuery)?,
                });
            }
            episodes.push(card);
        }
    }
    Ok(DetailSnapshot {
        item,
        episodes,
        next_cursor: (all.len() > offset + 24).then(|| (offset + 24).to_string()),
    })
}

fn field(v: &Value, key: &str, max: usize) -> Option<String> {
    bounded_exact_text(&v[key], max)
}
pub fn cards(raw: &str, provider: &str) -> Result<Vec<Card>, Error> {
    if raw.len() > 2 * 1024 * 1024 {
        return Err(Error::InvalidQuery);
    }
    let root: Value = serde_json::from_str(raw).map_err(|_| Error::InvalidQuery)?;
    let values = root["metas"].as_array().ok_or(Error::InvalidQuery)?;
    let mut seen = std::collections::HashSet::new();
    Ok(values
        .iter()
        .take(100)
        .filter_map(|v| {
            let id = field(v, "id", 512)?;
            let kind = field(v, "type", 64)?;
            if !seen.insert(format!("{kind}:{id}")) {
                return None;
            }
            let title = field(v, "name", 512).unwrap_or_else(|| id.clone());
            let ids = id
                .strip_prefix("tt")
                .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .map(|_| {
                    vec![ExternalId {
                        namespace: "imdb".into(),
                        value: id.clone(),
                    }]
                })
                .unwrap_or_default();
            Some(Card {
                identity: MediaIdentity {
                    id,
                    kind,
                    ids,
                    episode: None,
                    edition: None,
                },
                provider_id: provider.into(),
                title,
                subtitle: field(v, "releaseInfo", 256),
                description: field(v, "description", 1600),
                poster: field(v, "poster", 4096),
                landscape: field(v, "background", 4096),
                logo: field(v, "logo", 4096),
                progress: None,
                badges: vec![],
            })
        })
        .take(24)
        .collect())
}
