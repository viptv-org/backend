//! Host-validated lookup provenance, distinct from a shared persistence identity.
use crate::{Error, identity::MediaIdentity, presentation::Card};
use facet::Facet;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Facet)]
#[serde(deny_unknown_fields)]
pub struct MetadataOrigin {
    pub provider_id: String,
    pub connection_id: Option<String>,
    pub incarnation: Option<String>,
}
impl MetadataOrigin {
    pub fn validate(&self) -> Result<(), Error> {
        if self.provider_id == "simkl" && self.connection_id.is_none() && self.incarnation.is_none()
        {
            return Ok(());
        }
        if self.provider_id != "stremio" {
            return Err(Error::InvalidQuery);
        }
        let id = self.connection_id.as_deref().ok_or(Error::InvalidQuery)?;
        let incarnation = self.incarnation.as_deref().ok_or(Error::InvalidQuery)?;
        if id.is_empty()
            || id.len() > 128
            || id.chars().any(char::is_control)
            || incarnation.len() != 32
            || !incarnation
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(Error::InvalidQuery);
        }
        Ok(())
    }
    pub fn key(&self) -> Result<String, Error> {
        self.validate()?;
        serde_json::to_string(self).map_err(|_| Error::InvalidQuery)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
pub struct ScopedMediaIdentity {
    pub identity: MediaIdentity,
    pub metadata_origin: MetadataOrigin,
}

pub fn shared_identity(identity: &MediaIdentity) -> bool {
    let parts = identity.id.split(':').collect::<Vec<_>>();
    let imdb = |value: &str| {
        value.strip_prefix("tt").is_some_and(|digits| {
            !digits.is_empty() && digits.len() <= 16 && digits.bytes().all(|b| b.is_ascii_digit())
        })
    };
    let positive = |value: &str| {
        value
            .parse::<u64>()
            .is_ok_and(|n| n > 0 && n.to_string() == value)
    };
    match parts.as_slice() {
        [id] => identity.episode.is_none() && imdb(id),
        ["simkl", category, id] => {
            identity.episode.is_none()
                && matches!(*category, "movie" | "tv" | "anime")
                && positive(id)
        }
        [id, season, episode] if imdb(id) => coordinates(identity, season, episode),
        ["simkl", category, id, season, episode] => {
            matches!(*category, "tv" | "anime")
                && positive(id)
                && coordinates(identity, season, episode)
        }
        _ => false,
    }
}
fn coordinates(identity: &MediaIdentity, season: &str, episode: &str) -> bool {
    identity.kind == "series"
        && identity.episode.as_ref().is_some_and(|e| {
            e.order == "aired"
                && e.season
                    .and_then(|s| {
                        season
                            .parse::<u32>()
                            .ok()
                            .map(|v| s == v && v.to_string() == season)
                    })
                    .unwrap_or(false)
                && episode
                    .parse::<u32>()
                    .is_ok_and(|v| v > 0 && v == e.episode && v.to_string() == episode)
        })
}

/// Only authoritative host metadata may supply external identity aliases. Lookup
/// IDs and metadata_origin always remain on the original card/record.
pub fn verified_storage_identity(card: &Card) -> MediaIdentity {
    if shared_identity(&card.identity) || card.metadata_origin.is_none() {
        return card.identity.clone();
    }
    let imdb = |identity: &MediaIdentity| {
        let mut unique = std::collections::BTreeSet::new();
        for id in identity.ids.iter().filter(|id| id.namespace == "imdb") {
            if !id.value.strip_prefix("tt").is_some_and(|digits| {
                !digits.is_empty()
                    && digits.len() <= 16
                    && digits.bytes().all(|b| b.is_ascii_digit())
            }) {
                return None;
            }
            unique.insert(id.value.clone());
        }
        (unique.len() == 1)
            .then(|| unique.into_iter().next())
            .flatten()
    };
    let target = if card.identity.episode.is_some() {
        card.parent_identity
            .as_ref()
            .and_then(|parent| {
                if shared_identity(parent)
                    && parent.id.starts_with("tt")
                    && parent
                        .ids
                        .iter()
                        .filter(|id| id.namespace == "imdb")
                        .all(|id| id.value == parent.id)
                {
                    Some(parent.id.clone())
                } else {
                    imdb(parent)
                }
            })
            .and_then(|parent| {
                let e = card.identity.episode.as_ref()?;
                Some(format!(
                    "{parent}:{}:{}",
                    e.season.filter(|_| e.order == "aired")?,
                    e.episode
                ))
            })
    } else {
        imdb(&card.identity)
    };
    let mut result = card.identity.clone();
    if let Some(target) = target {
        result.id = target;
    }
    if shared_identity(&result) {
        result
    } else {
        card.identity.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn card() -> Card {
        serde_json::from_value(serde_json::json!({"identity":{"kind":"movie","id":"opaque","ids":[{"namespace":"imdb","value":"tt1234567"}]},"provider_id":"addon:one","metadata_origin":{"provider_id":"stremio","connection_id":"addon:one","incarnation":"a".repeat(32)},"title":"Fixture","badges":[]})).unwrap()
    }
    #[test]
    fn verified_alias_never_changes_lookup_provenance_and_conflicts_remain_private() {
        let mut c = card();
        let original = c.metadata_origin.clone();
        assert_eq!(verified_storage_identity(&c).id, "tt1234567");
        assert_eq!(c.identity.id, "opaque");
        assert_eq!(c.metadata_origin, original);
        c.identity.ids.push(crate::identity::ExternalId {
            namespace: "imdb".into(),
            value: "tt7654321".into(),
        });
        assert_eq!(verified_storage_identity(&c).id, "opaque");
        c.identity.ids.pop();
        c.metadata_origin = None;
        assert_eq!(verified_storage_identity(&c).id, "opaque");
    }
    #[test]
    fn episode_alias_requires_a_unique_parent_assertion_and_aired_coordinates() {
        let mut c = card();
        c.identity.kind = "series".into();
        c.identity.episode = Some(crate::identity::EpisodeCoordinates {
            order: "aired".into(),
            season: Some(2),
            episode: 3,
        });
        c.parent_identity=Some(serde_json::from_value(serde_json::json!({"kind":"series","id":"private-parent","ids":[{"namespace":"imdb","value":"tt1234567"}]})).unwrap());
        assert_eq!(verified_storage_identity(&c).id, "tt1234567:2:3");
        c.identity.episode.as_mut().unwrap().order = "absolute".into();
        assert_eq!(verified_storage_identity(&c).id, "opaque");
        c.identity.episode.as_mut().unwrap().order = "aired".into();
        c.parent_identity
            .as_mut()
            .unwrap()
            .ids
            .push(crate::identity::ExternalId {
                namespace: "imdb".into(),
                value: "tt7654321".into(),
            });
        assert_eq!(verified_storage_identity(&c).id, "opaque");
    }
}

/// Expected claims constrain a lookup; they never authorize a new shared alias.
pub fn metadata_assertions_match(expected: &Card, actual: &Card) -> bool {
    fn valid(namespace: &str, value: &str) -> Option<String> {
        match namespace {
            "imdb" => value
                .strip_prefix("tt")
                .filter(|digits| {
                    !digits.is_empty()
                        && digits.len() <= 16
                        && digits.bytes().all(|b| b.is_ascii_digit())
                })
                .map(|digits| format!("tt{digits}")),
            "tmdb" | "tvdb" | "simkl" | "simkl_episode" | "kitsu" | "mal" | "anilist" => value
                .bytes()
                .all(|b| b.is_ascii_digit())
                .then(|| {
                    value
                        .parse::<u64>()
                        .ok()
                        .filter(|n| *n > 0)
                        .map(|n| n.to_string())
                })
                .flatten(),
            _ => None,
        }
    }
    fn claims(
        identity: &MediaIdentity,
    ) -> std::collections::BTreeMap<String, std::collections::BTreeSet<String>> {
        let mut claims = std::collections::BTreeMap::new();
        for id in &identity.ids {
            if let Some(value) = valid(&id.namespace, &id.value) {
                claims
                    .entry(id.namespace.clone())
                    .or_insert_with(std::collections::BTreeSet::new)
                    .insert(value);
            }
        }
        if let Some(value) = identity
            .id
            .split(':')
            .next()
            .and_then(|id| valid("imdb", id))
        {
            claims
                .entry("imdb".into())
                .or_insert_with(std::collections::BTreeSet::new)
                .insert(value);
        }
        claims
    }
    fn compatible(expected: &MediaIdentity, actual: &MediaIdentity) -> bool {
        if expected.kind != actual.kind
            || expected.id != actual.id
            || expected
                .episode
                .as_ref()
                .is_some_and(|value| actual.episode.as_ref() != Some(value))
            || expected
                .edition
                .as_ref()
                .is_some_and(|value| actual.edition.as_ref() != Some(value))
        {
            return false;
        }
        let actual = claims(actual);
        claims(expected)
            .into_iter()
            .all(|(namespace, values)| values.len() == 1 && actual.get(&namespace) == Some(&values))
    }
    compatible(&expected.identity, &actual.identity)
        && expected
            .metadata_origin
            .as_ref()
            .is_none_or(|origin| actual.metadata_origin.as_ref() == Some(origin))
        && expected.parent_identity.as_ref().is_none_or(|parent| {
            actual
                .parent_identity
                .as_ref()
                .is_some_and(|actual| compatible(parent, actual))
        })
}

#[cfg(test)]
mod consistency_tests {
    use super::*;
    #[test]
    fn numeric_assertions_are_checked_without_treating_malformed_hints_as_authority() {
        let mut expected:Card=serde_json::from_value(serde_json::json!({"identity":{"id":"opaque","kind":"movie","ids":[{"namespace":"imdb","value":"forged-sibling-id"},{"namespace":"tmdb","value":"123"}]},"provider_id":"stremio","title":"Fixture","badges":[]})).unwrap();
        let mut actual = expected.clone();
        actual.identity.ids = vec![crate::identity::ExternalId {
            namespace: "tmdb".into(),
            value: "456".into(),
        }];
        assert!(!metadata_assertions_match(&expected, &actual));
        actual.identity.ids[0].value = "123".into();
        assert!(metadata_assertions_match(&expected, &actual));
        expected.identity.edition = Some("Director cut".into());
        assert!(!metadata_assertions_match(&expected, &actual));
        actual.identity.edition = expected.identity.edition.clone();
        assert!(metadata_assertions_match(&expected, &actual));
    }
}
