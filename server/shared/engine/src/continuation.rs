//! Typed viewing-queue and next-episode contracts; identities remain host-authoritative.
use crate::{Error, identity::MediaIdentity, presentation::Card, profile::Scope};
use facet::Facet;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Facet)]
#[repr(C)]
pub enum ContinuationStatus {
    Resume,
    Next,
    Pending,
    CaughtUp,
    Upcoming,
    Unavailable,
}
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
pub struct ContinuationEntry {
    pub card: Card,
    pub record: crate::playback::PlaybackRecord,
    pub status: ContinuationStatus,
    pub previous_identity: Option<MediaIdentity>,
}
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
pub struct ContinuationPage {
    pub scope: Scope,
    pub revision: u64,
    pub entries: Vec<ContinuationEntry>,
    pub next_cursor: Option<String>,
    pub complete: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(deny_unknown_fields)]
pub struct NextEpisodeRequest {
    pub identity: MediaIdentity,
    pub parent_identity: Option<MediaIdentity>,
}
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
pub struct NextEpisodeResult {
    pub status: ContinuationStatus,
    pub item: Option<Card>,
}
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(deny_unknown_fields)]
pub struct QueueVisibilityRequest {
    pub identity: MediaIdentity,
    pub parent_identity: Option<MediaIdentity>,
    pub hidden: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
pub struct QueueVisibilityReceipt {
    pub scope: Scope,
    pub revision: u64,
    pub identity: MediaIdentity,
    pub hidden: bool,
}
#[derive(Clone, Debug)]
pub struct EpisodeAvailability {
    pub card: Card,
    pub released_at_millis: Option<u64>,
    pub aired: Option<bool>,
}
/// Keep validation shared; caller-supplied external mappings remain hints, never proof.
pub fn validate_identity(identity: &MediaIdentity) -> Result<(), Error> {
    let bounded = |value: &str, max: usize| {
        !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
    };
    if !bounded(&identity.id, 512)
        || !matches!(identity.kind.as_str(), "movie" | "series")
        || identity.ids.len() > 32
        || identity
            .ids
            .iter()
            .any(|id| !bounded(&id.namespace, 64) || !bounded(&id.value, 512))
        || identity.edition.as_ref().is_some_and(|v| !bounded(v, 512))
        || identity
            .episode
            .as_ref()
            .is_some_and(|e| identity.kind != "series" || e.episode == 0 || !bounded(&e.order, 64))
    {
        return Err(Error::InvalidQuery);
    }
    Ok(())
}
/// Resolve one verified metadata sequence; never synthesize IDs or aired seasons.
pub fn next_episode(
    current: &MediaIdentity,
    episodes: &[EpisodeAvailability],
    now: u64,
) -> NextEpisodeResult {
    let result = |status, item| NextEpisodeResult { status, item };
    let unavailable = || result(ContinuationStatus::Unavailable, None);
    if validate_identity(current).is_err() || current.kind != "series" || episodes.len() > 10000 {
        return unavailable();
    }
    let matches = episodes
        .iter()
        .filter(|e| e.card.identity.kind == current.kind && e.card.identity.id == current.id)
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return unavailable();
    }
    let Some(at) = matches[0].card.identity.episode.as_ref() else {
        return unavailable();
    };
    if current
        .episode
        .as_ref()
        .is_some_and(|coordinates| coordinates != at)
    {
        return unavailable();
    }
    let Some(season) = at.season else {
        return unavailable();
    };
    let mut later = episodes
        .iter()
        .filter_map(|e| {
            let c = e.card.identity.episode.as_ref()?;
            Some((c.season?, c.episode, c.order.as_str(), e))
        })
        .filter(|(s, e, order, _)| {
            *order == at.order
                && ((*s, *e) > (season, at.episode))
                && ((season == 0 && *s == 0) || (season > 0 && *s > 0))
        })
        .collect::<Vec<_>>();
    later.sort_by_key(|(season, episode, _, _)| (*season, *episode));
    let Some((next_season, next_episode, _, next)) = later.first() else {
        return result(ContinuationStatus::CaughtUp, None);
    };
    if later
        .iter()
        .filter(|(s, e, _, _)| s == next_season && e == next_episode)
        .count()
        != 1
        || episodes
            .iter()
            .filter(|e| e.card.identity.id == next.card.identity.id)
            .count()
            != 1
        || next.card.parent_identity != matches[0].card.parent_identity
    {
        return unavailable();
    }
    if next.aired == Some(false) || next.released_at_millis.is_some_and(|date| date > now) {
        return result(ContinuationStatus::Upcoming, None);
    }
    if next.released_at_millis.is_none() && next.aired != Some(true) {
        return unavailable();
    }
    let mut card = next.card.clone();
    card.progress = None;
    card.added_at_millis = None;
    result(ContinuationStatus::Next, Some(card))
}
