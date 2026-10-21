//! New-episode facts for Continue Watching.
//!
//! A series whose latest finished episode was the newest released one gets a
//! "New episode" / "+N new episodes" badge and moves up once, when the next
//! episode is released. The rule itself is `viptv_engine::continuation::new_episodes`;
//! this module keeps the metadata it needs. Per profile and series it stores the
//! episodes after the finished one (identity, release time, aired flag, and the
//! first few cards) so Home stays synchronous: ordering and badges come from the
//! stored release times and the clock. Home schedules a bounded background
//! refresh of stale documents and bumps the library revision when a refresh
//! changes what the queue shows, so clients refresh quietly.
use super::*;
use serde::Serialize;
use std::collections::HashSet;
use viptv_engine::playback::PlaybackRecord;

/// Episodes kept after the finished one, and how many of them keep a card.
const STORED_EPISODES: usize = 2000;
const STORED_CARDS: usize = 4;
/// A stored document is refetched after this long, or after a shorter wait
/// once a release time it knows about has passed (to pick up the actual card).
const REFRESH_SECS: i64 = 6 * 3600;
const RELEASE_RECHECK_SECS: i64 = 15 * 60;
/// Series refreshed by one Home load, and metadata requests in flight.
const CANDIDATES_PER_RUN: usize = 24;
const CONCURRENCY: usize = 3;

#[derive(Serialize, Deserialize)]
struct Stored {
    identity: MediaIdentity,
    #[serde(default)]
    parent_identity: Option<MediaIdentity>,
    released_at_millis: Option<u64>,
    aired: Option<bool>,
    #[serde(default)]
    card: Option<Card>,
}

/// One series' episodes after `from_id`, valid for one account, configuration
/// and add-on revision (the same provenance the next-episode cache checks).
#[derive(Serialize, Deserialize)]
struct Document {
    account_id: i64,
    configuration_revision: u64,
    addon_revision: String,
    episodes: Vec<Stored>,
}

/// Record the metadata a next-episode resolution or refresh fetched for
/// `anchor`, the profile's latest finished episode of `series`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn store_db(
    db: &Connection,
    lease: &ResourceLease,
    profile: i64,
    series: &str,
    anchor: &MediaIdentity,
    episodes: &[EpisodeAvailability],
    configuration_revision: u64,
    addon_revision: &str,
) -> Result<(), ApiError> {
    let account_id = lease.principal.account_id().ok_or_else(auth::forbidden)?;
    let at = episodes
        .iter()
        .find(|entry| {
            entry.card.identity.id == anchor.id && entry.card.identity.kind == anchor.kind
        })
        .and_then(|entry| entry.card.identity.episode.clone());
    let mut kept = episodes
        .iter()
        .filter(|entry| {
            let Some(at) = at.as_ref() else {
                return false;
            };
            entry.card.identity.id == anchor.id
                || entry.card.identity.episode.as_ref().is_some_and(|c| {
                    c.order == at.order
                        && c.season.is_some()
                        && (c.season, c.episode) > (at.season, at.episode)
                })
        })
        .collect::<Vec<_>>();
    kept.sort_by_key(|entry| {
        let c = entry.card.identity.episode.as_ref();
        (
            entry.card.identity.id != anchor.id,
            c.and_then(|c| c.season),
            c.map(|c| c.episode),
        )
    });
    kept.truncate(STORED_EPISODES);
    let document = Document {
        account_id,
        configuration_revision,
        addon_revision: addon_revision.to_owned(),
        episodes: kept
            .into_iter()
            .enumerate()
            .map(|(index, entry)| Stored {
                identity: entry.card.identity.clone(),
                parent_identity: entry.card.parent_identity.clone(),
                released_at_millis: entry.released_at_millis,
                aired: entry.aired,
                card: (index <= STORED_CARDS).then(|| entry.card.clone()),
            })
            .collect(),
    };
    let document = serde_json::to_string(&document)
        .map_err(|_| ApiError::from("Release facts unavailable"))?;
    db.execute("INSERT INTO continuation_releases(profile_id,series_id,from_id,checked_at,document) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(profile_id,series_id) DO UPDATE SET from_id=excluded.from_id,checked_at=excluded.checked_at,document=excluded.document",params![profile,series,anchor.id,util::now(),document]).map_err(db_error)?;
    Ok(())
}

/// The episodes this profile has finished in `series`, by identity and by
/// aired-order coordinates (imported history may lack provider identities).
struct Watched {
    ids: HashSet<String>,
    coordinates: HashSet<(u32, u32)>,
}
impl Watched {
    fn read(db: &Connection, profile: i64, series: &str) -> Result<Self, ApiError> {
        let mut query = db
            .prepare("SELECT id,position,duration,context FROM progress WHERE profile_id=?1 AND type='series' AND title_id=?2")
            .map_err(db_error)?;
        let rows = query
            .query_map(params![profile, series], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, f64>(1)?,
                    r.get::<_, f64>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .map_err(db_error)?;
        let mut watched = Watched {
            ids: HashSet::new(),
            coordinates: HashSet::new(),
        };
        for row in rows {
            let (id, position, duration, context) = row.map_err(db_error)?;
            let context: Value = serde_json::from_str(&context).unwrap_or(Value::Null);
            if !crate::library::watched(&json!({"position":position,"duration":duration}), &context)
            {
                continue;
            }
            if let (Some(season), Some(episode)) = (
                context["season"]
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok()),
                context["episode"]
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok()),
            ) {
                watched.coordinates.insert((season, episode));
            }
            watched.ids.insert(id);
        }
        Ok(watched)
    }
    fn contains(&self, card: &Card) -> bool {
        self.ids.contains(&card.identity.id)
            || card.identity.episode.as_ref().is_some_and(|c| {
                c.order == "aired"
                    && c.season
                        .is_some_and(|season| self.coordinates.contains(&(season, c.episode)))
            })
    }
}

fn bare(identity: MediaIdentity, parent_identity: Option<MediaIdentity>) -> Card {
    Card {
        title: identity.id.clone(),
        identity,
        category: None,
        metadata: None,
        provider_id: "library".into(),
        subtitle: None,
        description: None,
        poster: None,
        landscape: None,
        logo: None,
        progress: None,
        badges: vec![],
        parent_identity,
        added_at_millis: None,
        metadata_origin: None,
    }
}

/// New episodes of `series` since the profile finished `anchor`, from the
/// stored document, or `None` when there are none or the facts are stale,
/// foreign or for another finished episode.
pub(crate) fn state_db(
    db: &Connection,
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    series: &str,
    anchor: &PlaybackRecord,
    now_millis: u64,
) -> Result<Option<NewEpisodes>, ApiError> {
    let row: Option<(String, Option<String>)> = db
        .query_row(
            "SELECT from_id,document FROM continuation_releases WHERE profile_id=?1 AND series_id=?2",
            params![profile, series],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(db_error)?;
    let Some((from, Some(raw))) = row else {
        return Ok(None);
    };
    if from != anchor.identity.id {
        return Ok(None);
    }
    let Ok(document) = serde_json::from_str::<Document>(&raw) else {
        return Ok(None);
    };
    if !current(db, app, lease, profile, &document)? {
        return Ok(None);
    }
    let Some(stored_anchor) = document.episodes.first().map(|e| e.identity.clone()) else {
        return Ok(None);
    };
    if stored_anchor.id != anchor.identity.id {
        return Ok(None);
    }
    let episodes = document
        .episodes
        .into_iter()
        .map(|stored| EpisodeAvailability {
            card: stored
                .card
                .unwrap_or_else(|| bare(stored.identity, stored.parent_identity)),
            released_at_millis: stored.released_at_millis,
            aired: stored.aired,
        })
        .collect::<Vec<_>>();
    let watched = Watched::read(db, profile, series)?;
    Ok(viptv_engine::continuation::new_episodes(
        &stored_anchor,
        anchor.updated_at_millis,
        &episodes,
        |card| watched.contains(card),
        now_millis,
    ))
}

fn current(
    db: &Connection,
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    document: &Document,
) -> Result<bool, ApiError> {
    Ok(lease.principal.account_id() == Some(document.account_id)
        && configuration_revision(db, profile)? == document.configuration_revision
        && app.addons.revision_db(db)? == document.addon_revision)
}

/// The queue card for a series with new episodes: the series title and art
/// with the episode to play as its identity and caption, and the count badge.
pub(crate) fn present(
    found: &NewEpisodes,
    anchor: &PlaybackRecord,
    verified: Option<Card>,
) -> Card {
    let mut card = verified
        .filter(|card| card.identity == found.next.identity)
        .unwrap_or_else(|| found.next.clone());
    let episode_title = std::mem::replace(&mut card.title, anchor.title.clone());
    let coordinates = card.identity.episode.as_ref().map(|c| {
        let episode = format!("E{}", c.episode);
        c.season
            .map_or(episode.clone(), |season| format!("S{season} {episode}"))
    });
    let named = (!episode_title.is_empty()
        && episode_title != card.identity.id
        && episode_title != anchor.title)
        .then_some(episode_title);
    card.subtitle = match (coordinates, named) {
        (Some(at), Some(name)) => Some(format!("{at} · {name}")),
        (Some(at), None) => Some(at),
        (None, name) => name,
    };
    if anchor.poster.is_some() {
        card.poster = anchor.poster.clone();
    }
    if card.landscape.is_none() {
        card.landscape = anchor.landscape.clone();
    }
    card.progress = None;
    card.badges
        .retain(|badge| !viptv_engine::continuation::is_new_episodes_badge(badge));
    card.badges.insert(
        0,
        viptv_engine::continuation::new_episodes_badge(found.count),
    );
    card
}

struct Candidate {
    series: String,
    anchor: PlaybackRecord,
}

/// Refresh stale release facts for this profile's finished series in the
/// background. Bounded per call; a no-op when every document is fresh.
pub(crate) fn schedule(app: &App, lease: &ResourceLease, profile: i64) {
    let candidates = match claim(app, lease, profile) {
        Ok(candidates) if !candidates.is_empty() => candidates,
        _ => return,
    };
    let app = app.clone();
    let lease = lease.clone();
    tokio::spawn(async move {
        let _ = tokio::time::timeout(
            Duration::from_secs(120),
            refresh(&app, &lease, profile, candidates),
        )
        .await;
    });
}

/// Pick the series whose documents are missing or stale and mark them checked
/// now, so concurrent Home loads do not fetch the same documents twice.
fn claim(app: &App, lease: &ResourceLease, profile: i64) -> Result<Vec<Candidate>, ApiError> {
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_profile(&db, profile)?;
    lease.validate(&db)?;
    // Restricted kids sessions keep the existing queue; they never trigger fetches.
    if crate::kids::restricted(&db, &lease.principal)? {
        return Ok(vec![]);
    }
    let queue = crate::continuation::queue_items(&db, profile, 0, 50000, true)?;
    let now = util::now();
    let mut candidates = Vec::new();
    for item in queue["items"].as_array().into_iter().flatten() {
        if candidates.len() >= CANDIDATES_PER_RUN {
            break;
        }
        if item["type"] != "series" {
            continue;
        }
        // Only finished episodes (the queue page's own status): an in-progress
        // episode is resumed, not promoted.
        match super::super::history::record(item) {
            Some(rendered) if state(item, &rendered) != ContinuationStatus::Resume => {}
            _ => continue,
        }
        let anchor = if item["previous_episode"].is_object() {
            &item["previous_episode"]
        } else {
            item
        };
        let Some(anchor) = super::super::history::record(anchor) else {
            continue;
        };
        let Some(series) = anchor
            .parent_identity
            .as_ref()
            .filter(|_| anchor.identity.episode.is_some())
            .map(|parent| parent.id.clone())
        else {
            continue;
        };
        // A release after its removal brought this title back: un-hide it for
        // good, so playing the new episode does not remove it again.
        if let Some(hidden) = item["queue_hidden_at"].as_u64() {
            let returned = state_db(
                &db,
                app,
                lease,
                profile,
                &series,
                &anchor,
                (now as u64).saturating_mul(1000),
            )?
            .is_some_and(|found| {
                found
                    .released_at_millis
                    .iter()
                    .any(|at| *at > hidden.saturating_mul(1000))
            });
            if returned {
                db.execute(
                    "DELETE FROM queue_hidden WHERE profile_id=?1 AND type='series' AND title_id=?2",
                    params![profile, item["queue_title_id"].as_str().unwrap_or(&series)],
                )
                .map_err(db_error)?;
            }
        }
        let row: Option<(String, i64, Option<String>)> = db
            .query_row(
                "SELECT from_id,checked_at,document FROM continuation_releases WHERE profile_id=?1 AND series_id=?2",
                params![profile, series],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(db_error)?;
        let due = match &row {
            None => true,
            Some((from, checked, raw)) => {
                let document = raw
                    .as_deref()
                    .and_then(|raw| serde_json::from_str::<Document>(raw).ok());
                let released_since = document.as_ref().is_some_and(|document| {
                    document.episodes.iter().any(|e| {
                        e.released_at_millis.is_some_and(|at| {
                            at > (*checked as u64).saturating_mul(1000)
                                && at <= (now as u64).saturating_mul(1000)
                        })
                    })
                });
                *from != anchor.identity.id
                    || *checked < now - REFRESH_SECS
                    || (released_since && *checked < now - RELEASE_RECHECK_SECS)
                    || (*checked < now - RELEASE_RECHECK_SECS
                        && !document.as_ref().map_or(Ok(false), |document| {
                            current(&db, app, lease, profile, document)
                        })?)
            }
        };
        if !due {
            continue;
        }
        // Keep the previous facts for the same finished episode until the
        // refresh replaces them; another episode's facts no longer apply.
        db.execute("INSERT INTO continuation_releases(profile_id,series_id,from_id,checked_at,document) VALUES(?1,?2,?3,?4,NULL) ON CONFLICT(profile_id,series_id) DO UPDATE SET document=CASE WHEN continuation_releases.from_id=excluded.from_id THEN continuation_releases.document END,from_id=excluded.from_id,checked_at=excluded.checked_at",params![profile,series,anchor.identity.id,now]).map_err(db_error)?;
        candidates.push(Candidate { series, anchor });
    }
    Ok(candidates)
}

async fn refresh(app: &App, lease: &ResourceLease, profile: i64, candidates: Vec<Candidate>) {
    use futures::StreamExt;
    let Ok((configuration, _)) = configuration::read(app, lease, profile) else {
        return;
    };
    let simkl = configuration.browsing_plugin == "simkl";
    let revision = configuration.scope.revision;
    futures::stream::iter(candidates)
        .map(|candidate| async move {
            let _ = refresh_one(app, lease, profile, simkl, revision, candidate).await;
        })
        .buffer_unordered(CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
}

async fn refresh_one(
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    simkl: bool,
    configuration: u64,
    candidate: Candidate,
) -> Result<(), ApiError> {
    let addon_revision = app.addons.revision()?;
    let input = NextEpisodeRequest {
        identity: candidate.anchor.identity.clone(),
        parent_identity: candidate.anchor.parent_identity.clone(),
    };
    validate(&input.identity, input.parent_identity.as_ref())?;
    let (parent, episodes) = tokio::time::timeout(
        Duration::from_secs(10),
        super::metadata::document(app, lease, profile, &input, simkl, None, None),
    )
    .await
    .map_err(|_| ApiError::from("Metadata unavailable"))??;
    if parent.identity.id != candidate.series {
        return Err("Metadata returned a different parent".into());
    }
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    lease.validate(&db)?;
    app.require_profile(&db, profile)?;
    if configuration_revision(&db, profile)? != configuration
        || app.addons.revision_db(&db)? != addon_revision
    {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Profile configuration changed".into(),
        ));
    }
    // The profile finished another episode meanwhile: its own refresh owns the row.
    let from: Option<String> = db
        .query_row(
            "SELECT from_id FROM continuation_releases WHERE profile_id=?1 AND series_id=?2",
            params![profile, candidate.series],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?;
    if from.as_deref() != Some(candidate.anchor.identity.id.as_str()) {
        return Ok(());
    }
    let now = util::now_millis().max(0) as u64;
    let summary = |found: Option<NewEpisodes>| {
        found.map(|f| {
            (
                f.count,
                f.next.identity.id,
                f.first_released_at_millis,
                f.released_at_millis,
            )
        })
    };
    let before = summary(state_db(
        &db,
        app,
        lease,
        profile,
        &candidate.series,
        &candidate.anchor,
        now,
    )?);
    store_db(
        &db,
        lease,
        profile,
        &candidate.series,
        &input.identity,
        &episodes,
        configuration,
        &addon_revision,
    )?;
    let after = summary(state_db(
        &db,
        app,
        lease,
        profile,
        &candidate.series,
        &candidate.anchor,
        now,
    )?);
    if before != after {
        db.execute("INSERT INTO engine_library_revisions VALUES(?1,1) ON CONFLICT(profile_id) DO UPDATE SET revision=revision+1",[profile]).map_err(db_error)?;
    }
    Ok(())
}
