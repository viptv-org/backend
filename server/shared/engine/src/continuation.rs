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
/// The episodes after `current` in its own order and season family (regular
/// seasons or specials), sorted by coordinates. `Err` means the metadata does
/// not identify `current` exactly once with matching coordinates.
fn sequence<'a>(
    current: &MediaIdentity,
    episodes: &'a [EpisodeAvailability],
) -> Result<
    (
        &'a EpisodeAvailability,
        Vec<(u32, u32, &'a EpisodeAvailability)>,
    ),
    (),
> {
    if validate_identity(current).is_err() || current.kind != "series" || episodes.len() > 10000 {
        return Err(());
    }
    let matches = episodes
        .iter()
        .filter(|e| e.card.identity.kind == current.kind && e.card.identity.id == current.id)
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(());
    }
    let at = matches[0].card.identity.episode.as_ref().ok_or(())?;
    if current
        .episode
        .as_ref()
        .is_some_and(|coordinates| coordinates != at)
    {
        return Err(());
    }
    let season = at.season.ok_or(())?;
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
        .map(|(s, e, _, entry)| (s, e, entry))
        .collect::<Vec<_>>();
    later.sort_by_key(|(season, episode, _)| (*season, *episode));
    Ok((matches[0], later))
}
/// Resolve one verified metadata sequence; never synthesize IDs or aired seasons.
pub fn next_episode(
    current: &MediaIdentity,
    episodes: &[EpisodeAvailability],
    now: u64,
) -> NextEpisodeResult {
    let result = |status, item| NextEpisodeResult { status, item };
    let unavailable = || result(ContinuationStatus::Unavailable, None);
    let Ok((anchor, later)) = sequence(current, episodes) else {
        return unavailable();
    };
    let Some((next_season, next_episode, next)) = later.first() else {
        return result(ContinuationStatus::CaughtUp, None);
    };
    if later
        .iter()
        .filter(|(s, e, _)| s == next_season && e == next_episode)
        .count()
        != 1
        || episodes
            .iter()
            .filter(|e| e.card.identity.id == next.card.identity.id)
            .count()
            != 1
        || next.card.parent_identity != anchor.card.parent_identity
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

/// Episodes that aired after the profile last finished an episode of a series.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewEpisodes {
    /// The first released episode the profile has not finished: the one to play.
    pub next: Card,
    /// Released, unfinished episodes after the finished one, `next` included.
    pub count: u32,
    /// When `next` was released. It only changes once the profile finishes
    /// `next`, so later releases never move a queue ordered by it.
    pub first_released_at_millis: u64,
    /// Ascending release times of the counted episodes that carry one.
    pub released_at_millis: Vec<u64>,
}

/// New episodes since `current` was finished at `finished_at`, or `None`.
///
/// They exist only when the profile had caught up: the first released episode
/// after `current` that `watched` does not exclude was released after
/// `finished_at`. A backlog that was already available is not new, so a
/// release while the profile is behind adds to the count of an existing
/// promotion but never starts one. Counting follows [`next_episode`]: the same
/// episode order, regular seasons apart from specials, and a release proven by
/// a past date or an explicit aired flag. Ambiguous metadata yields `None`.
pub fn new_episodes(
    current: &MediaIdentity,
    finished_at: u64,
    episodes: &[EpisodeAvailability],
    watched: impl Fn(&Card) -> bool,
    now: u64,
) -> Option<NewEpisodes> {
    let (anchor, later) = sequence(current, episodes).ok()?;
    let mut seen = std::collections::HashSet::new();
    for (season, episode, entry) in &later {
        if !seen.insert((*season, *episode))
            || entry.card.parent_identity != anchor.card.parent_identity
            || episodes
                .iter()
                .filter(|e| e.card.identity.id == entry.card.identity.id)
                .count()
                != 1
        {
            return None;
        }
    }
    let released = |entry: &EpisodeAvailability| {
        entry.aired != Some(false)
            && match entry.released_at_millis {
                Some(date) => date <= now,
                None => entry.aired == Some(true),
            }
    };
    let pending = later
        .iter()
        .map(|(_, _, entry)| *entry)
        .filter(|entry| released(entry) && !watched(&entry.card))
        .collect::<Vec<_>>();
    let first = pending.first()?;
    let first_released_at_millis = first
        .released_at_millis
        .filter(|date| *date > finished_at)?;
    let mut next = first.card.clone();
    next.progress = None;
    next.added_at_millis = None;
    let mut released_at_millis = pending
        .iter()
        .filter_map(|entry| entry.released_at_millis)
        .collect::<Vec<_>>();
    released_at_millis.sort_unstable();
    Some(NewEpisodes {
        next,
        count: u32::try_from(pending.len()).unwrap_or(u32::MAX),
        first_released_at_millis,
        released_at_millis,
    })
}

/// The card label for `count` new episodes: "New episode", then "+2 new episodes".
pub fn new_episodes_badge(count: u32) -> String {
    if count <= 1 {
        "New episode".into()
    } else {
        format!("+{count} new episodes")
    }
}

/// Whether a card badge is a [`new_episodes_badge`] label.
pub fn is_new_episodes_badge(badge: &str) -> bool {
    badge == "New episode"
        || badge
            .strip_prefix('+')
            .and_then(|rest| rest.strip_suffix(" new episodes"))
            .is_some_and(|count| !count.is_empty() && count.bytes().all(|b| b.is_ascii_digit()))
}
