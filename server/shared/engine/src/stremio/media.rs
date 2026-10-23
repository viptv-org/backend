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
    detail_page(raw, provider, kind, id, offset, 24)
}
pub fn detail_page(
    raw: &str,
    provider: &str,
    kind: &str,
    id: &str,
    offset: usize,
    limit: usize,
) -> Result<DetailSnapshot, Error> {
    if raw.len() > 2 * 1024 * 1024 || offset > 100_000 || !(1..=128).contains(&limit) {
        return Err(Error::InvalidQuery);
    }
    let root: Value = serde_json::from_str(raw).map_err(|_| Error::InvalidQuery)?;
    let meta = if root["meta"].is_object() {
        &root["meta"]
    } else {
        &root
    };
    if meta
        .get("type")
        .is_some_and(|value| !value.is_null() && value.as_str() != Some(kind))
    {
        return Err(Error::Unresolved);
    }
    if meta["id"].as_str() != Some(id) {
        return Err(Error::Unresolved);
    }
    let mut parent = meta.clone();
    parent["id"] = Value::String(id.into());
    parent["type"] = Value::String(kind.into());
    let mut item = cards(&serde_json::json!({"metas":[parent]}).to_string(), provider)?
        .into_iter()
        .next()
        .ok_or(Error::Unresolved)?;
    // Only the complete host-fetched metadata document can add alternate IDs.
    // Preview/catalog hints remain outside this proof path; video fields cannot
    // override the parent identity used for episode matching.
    for external in authoritative_external_ids(meta) {
        if !item.identity.ids.contains(&external) {
            item.identity.ids.push(external);
        }
    }
    let all = episode_cards(meta, provider, &item)?;
    let seasons = crate::presentation::episode_seasons(&all);
    let next_cursor = (all.len() > offset + limit).then(|| (offset + limit).to_string());
    let episodes = all.into_iter().skip(offset).take(limit).collect();
    Ok(DetailSnapshot {
        recommendations: recommendations(meta, provider, &item.identity.id)?,
        item,
        episodes,
        next_cursor,
        seasons,
    })
}

/// Normalize the already-fetched complete episode document before paging.
pub fn episode_cards(meta: &Value, provider: &str, item: &Card) -> Result<Vec<Card>, Error> {
    let all = meta["videos"].as_array().cloned().unwrap_or_default();
    if all.len() > 10000 {
        return Err(Error::InvalidQuery);
    }
    let mut episodes = Vec::new();
    for video in all.iter() {
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
            // Sparse metas number episodes in `number`, or only inside the video
            // id ("tt1234567:1:2"). Without coordinates the card is dead: Play can
            // never resolve it and the row shows titles that do nothing.
            let episode = video["episode"].as_u64()
                .or_else(|| video["number"].as_u64())
                .or_else(|| {
                    let id = video["id"].as_str()?;
                    let parts: Vec<&str> = id.split(':').collect();
                    if parts.len() == 3 { parts[2].parse::<u64>().ok() } else { None }
                });
            if let Some(episode) = episode {
                if episode == 0 {
                    continue;
                }
                let season = if video["season"].is_null() {
                    None
                } else {
                    Some(
                        u32::try_from(video["season"].as_u64().ok_or(Error::InvalidQuery)?)
                            .map_err(|_| Error::InvalidQuery)?,
                    )
                };
                card.identity.episode = Some(EpisodeCoordinates {
                    order: "aired".into(),
                    season,
                    episode: u32::try_from(episode).map_err(|_| Error::InvalidQuery)?,
                });
            }
            card.parent_identity = Some(item.identity.clone());
            episodes.push(card);
        }
    }
    Ok(crate::presentation::canonical_episodes(episodes))
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
                metadata_origin: None,
                identity: MediaIdentity {
                    id,
                    kind: kind.clone(),
                    ids,
                    episode: None,
                    edition: None,
                },
                provider_id: provider.into(),
                title,
                subtitle: field(v, "releaseInfo", 256),
                description: field(v, "description", 16_384)
                    .or_else(|| field(v, "overview", 16_384)),
                poster: field(v, "poster", 4096),
                landscape: field(v, "background", 4096),
                logo: ["logo", "clearlogo", "clearLogo", "titleLogo", "title_logo"]
                    .into_iter()
                    .find_map(|key| field(v, key, 4096)),
                progress: None,
                badges: vec![],
                parent_identity: None,
                added_at_millis: None,
                category: Some(kind.clone()),
                metadata: Some(crate::presentation::CardMetadata {
                    year: field(v, "releaseInfo", 256)
                        .and_then(|s| s.get(..4).and_then(|s| s.parse::<u32>().ok()))
                        .filter(|n| *n <= 9999),
                    runtime_minutes: field(v, "runtime", 64)
                        .and_then(|s| {
                            s.split_whitespace()
                                .next()
                                .and_then(|s| s.parse::<u32>().ok())
                        })
                        .filter(|n| *n <= 10000),
                    genres: v["genres"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|v| v.as_str())
                        .take(16)
                        .map(|s| s.chars().take(128).collect())
                        .collect(),
                    rating: v["imdbRating"]
                        .as_f64()
                        .or_else(|| v["imdbRating"].as_str().and_then(|s| s.parse().ok()))
                        .filter(|n| n.is_finite() && (0.0..=10.0).contains(n)),
                    ..Default::default()
                }),
            })
        })
        .take(24)
        .collect())
}

/// Only recommendations supplied by the selected metadata addon are projected.
pub fn recommendations(meta: &Value, provider: &str, original: &str) -> Result<Vec<Card>, Error> {
    let values = meta["recommendations"]
        .as_array()
        .or_else(|| meta["similar"].as_array())
        .cloned()
        .unwrap_or_default();
    Ok(
        cards(&serde_json::json!({"metas":values}).to_string(), provider)?
            .into_iter()
            .filter(|c| {
                c.identity.id != original && matches!(c.identity.kind.as_str(), "movie" | "series")
            })
            .collect(),
    )
}

/// Recognize bounded, explicit provider assertions, preserving disagreements so
/// resolvers can reject them instead of selecting an arbitrary first mapping.
fn authoritative_external_ids(meta: &Value) -> Vec<ExternalId> {
    let fields: [(&str, &[&str]); 6] = [
        ("imdb", &["imdb_id", "imdbId", "imdbID"]),
        ("tmdb", &["tmdb_id", "tmdbId", "moviedb_id"]),
        ("tvdb", &["tvdb_id", "tvdbId"]),
        ("mal", &["mal_id", "malId"]),
        ("anilist", &["anilist_id", "anilistId"]),
        ("kitsu", &["kitsu_id", "kitsuId"]),
    ];
    let mut result = Vec::new();
    for (namespace, aliases) in fields {
        let values = aliases.iter().map(|field| &meta[*field]).chain(
            ["ids", "external_ids"].into_iter().flat_map(|container| {
                std::iter::once(&meta[container][namespace])
                    .chain(aliases.iter().map(move |field| &meta[container][*field]))
            }),
        );
        for value in values {
            let text = value
                .as_str()
                .filter(|value| value.len() <= 32)
                .map(str::to_owned)
                .or_else(|| value.as_u64().map(|value| value.to_string()));
            let Some(value) = text else {
                continue;
            };
            let valid = if namespace == "imdb" {
                value.strip_prefix("tt").is_some_and(|digits| {
                    !digits.is_empty()
                        && digits.len() <= 16
                        && digits.bytes().all(|byte| byte.is_ascii_digit())
                })
            } else {
                value
                    .parse::<u64>()
                    .is_ok_and(|number| number > 0 && number.to_string() == value)
            };
            if valid {
                let id = ExternalId {
                    namespace: namespace.into(),
                    value,
                };
                if !result.contains(&id) {
                    result.push(id);
                }
                if result.len() == 32 {
                    return result;
                }
            }
        }
    }
    result
}

#[cfg(test)]
mod identity_proof_tests {
    use super::*;
    #[test]
    fn full_metadata_preserves_explicit_ids_without_promoting_catalog_hints_or_video_overrides() {
        let meta = serde_json::json!({"id":"opaque-parent","type":"series","name":"Series","imdb_id":"tt1111111","tmdb_id":123,"ids":{"tvdb":"456"},"videos":[{"id":"opaque-child","name":"Episode","season":2,"episode":3,"imdb_id":"tt9999999","tmdb_id":999}]});
        let preview = cards(
            &serde_json::json!({"metas":[meta.clone()]}).to_string(),
            "addon:one",
        )
        .unwrap()
        .remove(0);
        assert!(preview.identity.ids.is_empty());
        let detail = detail_page(
            &serde_json::json!({"meta":meta}).to_string(),
            "addon:one",
            "series",
            "opaque-parent",
            0,
            24,
        )
        .unwrap();
        assert!(detail.item.identity.ids.contains(&ExternalId {
            namespace: "imdb".into(),
            value: "tt1111111".into()
        }));
        assert!(detail.item.identity.ids.contains(&ExternalId {
            namespace: "tmdb".into(),
            value: "123".into()
        }));
        assert!(detail.item.identity.ids.contains(&ExternalId {
            namespace: "tvdb".into(),
            value: "456".into()
        }));
        assert_eq!(
            detail.episodes[0].parent_identity.as_ref().unwrap().ids,
            detail.item.identity.ids
        );
        assert!(detail.episodes[0].identity.ids.is_empty());
    }
    #[test]
    fn present_conflicting_metadata_kind_is_not_rewritten_into_authority() {
        assert!(matches!(
            detail_page(
                r#"{"meta":{"id":"opaque","type":"movie","name":"Wrong kind","imdb_id":"tt1111111"}}"#,
                "addon:one",
                "series",
                "opaque",
                0,
                24
            ),
            Err(Error::Unresolved)
        ));
        assert!(
            detail_page(
                r#"{"meta":{"id":"opaque","name":"Missing kind"}}"#,
                "addon:one",
                "series",
                "opaque",
                0,
                24
            )
            .is_ok()
        );
    }
    #[test]
    fn sparse_videos_number_episodes_from_number_or_the_video_id() {
        // Cinemeta's live meta for obscure series ships videos with no `episode`
        // field: the number is in `number`, or only inside `id` (tt…:season:n).
        let detail = detail_page(
            &serde_json::json!({"meta":{"id":"tt33673360","type":"series","name":"Sparse",
                "videos":[
                    {"id":"tt33673360:1:1","season":1,"number":1},
                    {"id":"tt33673360:1:2","season":1}
                ]}}).to_string(),
            "addon:one", "series", "tt33673360", 0, 24,
        ).unwrap();
        let coordinates: Vec<_> = detail.episodes.iter().filter_map(|e| e.identity.episode.clone()).collect();
        assert_eq!(coordinates.len(), 2, "every sparse video must still carry its episode number");
        assert_eq!(coordinates[0].episode, 1);
        assert_eq!(coordinates[1].episode, 2);
        assert_eq!(coordinates[0].season, Some(1));
    }
    #[test]
    fn conflicts_remain_visible_and_invalid_nested_fractional_or_noncanonical_ids_are_ignored() {
        let detail=detail_page(r#"{"meta":{"id":"tt1111111","name":"Movie","imdb_id":"tt2222222","ids":{"imdb":"tt3333333","tmdb":"00123"},"tmdb_id":1.25,"tvdb_id":{"id":123},"mal_id":true,"anilist_id":0,"kitsu_id":"12x"}}"#,"addon:one","movie","tt1111111",0,24).unwrap();
        assert_eq!(
            detail
                .item
                .identity
                .ids
                .iter()
                .filter(|id| id.namespace == "imdb")
                .count(),
            3
        );
        assert_eq!(detail.item.identity.ids.len(), 3);
        assert!(crate::identity::resolve(&detail.item.identity, "imdb", &[]).is_err());
    }
}
