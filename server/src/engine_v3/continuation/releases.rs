//! New-episode facts for Continue Watching, and the durable next-episode
//! verdict per finished series.
//!
//! A series whose latest finished episode was the newest released one gets a
//! compact "+N" badge and moves up once, when the next episode is released.
//! The rule itself is `viptv_engine::continuation::new_episodes`; this module
//! keeps the metadata it needs. Per profile and series it stores the episodes
//! after the finished one (identity, release time, aired flag, and the first
//! few cards) and the series backdrop in `continuation_releases`, so Home
//! stays synchronous: ordering, badges and the next-episode verdict
//! (`verdicts_db`, read by the Watched badge) come from stored release times
//! and the clock, card art (`landscape_db`) from the stored cards. Rows are
//! never evicted.
//!
//! Each row carries `next_check_at`. Home loads and Stremio imports start a
//! background run for the profile's own session that refreshes every due
//! series in rate-limited batches until none is due, so a whole import is
//! covered and an interrupted run resumes on the next trigger. A series is
//! checked again shortly after its next known air time (bypassing the add-on
//! response cache), daily without a known date, weekly once dormant, and with
//! growing back-off while its metadata is unavailable. A refresh that changes
//! what the queue shows bumps the library revision, so clients refresh quietly.
use super::*;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use viptv_engine::playback::PlaybackRecord;

/// The provider bound on one series' episode list (`next_episode` takes at
/// most this many); the first few after the finished one keep their cards.
const STORED_EPISODES: usize = 10000;
const STORED_CARDS: usize = 4;
/// Re-check this long after a known air time passes.
const AIRED_GRACE_SECS: i64 = 5 * 60;
/// Without a known future air date: daily, or weekly once the newest known
/// episode is older than `DORMANT_AFTER_SECS`.
const ACTIVE_SECS: i64 = 24 * 3600;
const DORMANT_SECS: i64 = 7 * 24 * 3600;
const DORMANT_AFTER_SECS: i64 = 180 * 24 * 3600;
/// Unavailable metadata: one hour, doubling per failure, at most a week.
const RETRY_SECS: i64 = 3600;
/// A claimed series is retried after this long if its run never finished.
const CLAIM_SECS: i64 = 15 * 60;
/// Series claimed per batch, lookups in flight per run and server-wide, and
/// the spacing of lookup starts: the add-on request rate limit.
const BATCH: usize = 24;
const CONCURRENCY: usize = 3;
const SERVER_LOOKUPS: usize = 4;
const LOOKUP_SPACING: Duration = Duration::from_millis(250);
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(15);

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
    /// The series backdrop (absent from documents stored before it was kept).
    #[serde(default)]
    landscape: Option<String>,
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
    landscape: Option<&str>,
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
        landscape: landscape.map(str::to_owned),
    };
    let document = serde_json::to_string(&document)
        .map_err(|_| ApiError::from("Release facts unavailable"))?;
    let now = util::now();
    let (next_check_at, failures) = if at.is_some() {
        (next_check(now, episodes), 0)
    } else {
        // The finished episode is not in the provider's list: unavailable.
        let failures: i64 = db
            .query_row(
                "SELECT failures FROM continuation_releases WHERE profile_id=?1 AND series_id=?2",
                params![profile, series],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?
            .unwrap_or(0);
        (now + retry_after(failures + 1), failures + 1)
    };
    db.execute("INSERT INTO continuation_releases(profile_id,series_id,from_id,checked_at,document,next_check_at,failures) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(profile_id,series_id) DO UPDATE SET from_id=excluded.from_id,checked_at=excluded.checked_at,document=excluded.document,next_check_at=excluded.next_check_at,failures=excluded.failures",params![profile,series,anchor.id,now,document,next_check_at,failures]).map_err(db_error)?;
    Ok(())
}

/// When to look at a series again: just after its next known air time, else
/// daily, or weekly when its newest known episode is long past.
fn next_check(now: i64, episodes: &[EpisodeAvailability]) -> i64 {
    let now_millis = (now as u64).saturating_mul(1000);
    let releases = episodes.iter().filter_map(|e| e.released_at_millis);
    if let Some(next) = releases.clone().filter(|at| *at > now_millis).min() {
        return (next / 1000) as i64 + AIRED_GRACE_SECS;
    }
    let newest = releases.max().map(|at| (at / 1000) as i64);
    if newest.is_some_and(|at| at < now - DORMANT_AFTER_SECS) {
        now + DORMANT_SECS
    } else {
        now + ACTIVE_SECS
    }
}

fn retry_after(failures: i64) -> i64 {
    RETRY_SECS
        .saturating_mul(1i64 << failures.clamp(1, 20).saturating_sub(1))
        .min(DORMANT_SECS)
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
    let Some((from, document)) = document_db(db, profile, series)? else {
        return Ok(None);
    };
    if from != anchor.identity.id {
        return Ok(None);
    }
    if !current(db, app, lease, profile, &document)? {
        return Ok(None);
    }
    let Some(stored_anchor) = document.episodes.first().map(|e| e.identity.clone()) else {
        return Ok(None);
    };
    if stored_anchor.id != anchor.identity.id {
        return Ok(None);
    }
    let episodes = availability(document.episodes);
    let watched = Watched::read(db, profile, series)?;
    Ok(viptv_engine::continuation::new_episodes(
        &stored_anchor,
        anchor.updated_at_millis,
        &episodes,
        |card| watched.contains(card),
        now_millis,
    ))
}

/// The stored document of `series` and the finished episode it was read for.
fn document_db(
    db: &Connection,
    profile: i64,
    series: &str,
) -> Result<Option<(String, Document)>, ApiError> {
    let row: Option<(String, Option<String>)> = db
        .query_row(
            "SELECT from_id,document FROM continuation_releases WHERE profile_id=?1 AND series_id=?2",
            params![profile, series],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(db_error)?;
    Ok(row.and_then(|(from, raw)| Some((from, serde_json::from_str::<Document>(&raw?).ok()?))))
}

/// Stored 16:9 art for a Continue Watching card of `series`: the still of
/// `episode`, else the series backdrop, from this account's metadata document
/// (whichever finished episode it was read for; art does not go stale the
/// way release facts do).
pub(crate) fn landscape_db(
    db: &Connection,
    lease: &ResourceLease,
    profile: i64,
    series: &str,
    episode: &str,
) -> Result<Option<String>, ApiError> {
    let Some((_, document)) = document_db(db, profile, series)? else {
        return Ok(None);
    };
    if lease.principal.account_id() != Some(document.account_id) {
        return Ok(None);
    }
    Ok(document
        .episodes
        .iter()
        .find(|stored| stored.identity.id == episode)
        .and_then(|stored| stored.card.as_ref()?.landscape.clone())
        .or(document.landscape))
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

/// The durable next-episode verdict per series for the finished episode it
/// was computed from: `(from_id, "next" | "caught_up" | "upcoming" |
/// "unavailable")`, evaluated against the clock (the spelling the queue cache
/// uses).
pub(crate) fn verdicts_db(
    db: &Connection,
    profile: i64,
) -> Result<HashMap<String, (String, &'static str)>, ApiError> {
    let now = util::now_millis().max(0) as u64;
    let mut query = db
        .prepare("SELECT series_id,from_id,document FROM continuation_releases WHERE profile_id=?1 AND document IS NOT NULL")
        .map_err(db_error)?;
    let rows = query
        .query_map([profile], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })
        .map_err(db_error)?;
    let mut verdicts = HashMap::new();
    for row in rows {
        let (series, from, raw) = row.map_err(db_error)?;
        let status = match serde_json::from_str::<Document>(&raw) {
            Ok(document) if document.episodes.first().is_some_and(|e| e.identity.id == from) => {
                let anchor = document.episodes[0].identity.clone();
                let episodes = availability(document.episodes);
                match viptv_engine::continuation::next_episode(&anchor, &episodes, now).status {
                    ContinuationStatus::Next => "next",
                    ContinuationStatus::CaughtUp => "caught_up",
                    ContinuationStatus::Upcoming => "upcoming",
                    _ => "unavailable",
                }
            }
            _ => "unavailable",
        };
        verdicts.insert(series, (from, status));
    }
    Ok(verdicts)
}

fn availability(stored: Vec<Stored>) -> Vec<EpisodeAvailability> {
    stored
        .into_iter()
        .map(|stored| EpisodeAvailability {
            card: stored
                .card
                .unwrap_or_else(|| bare(stored.identity, stored.parent_identity)),
            released_at_millis: stored.released_at_millis,
            aired: stored.aired,
        })
        .collect()
}

/// One background run per profile at a time; a trigger during a run asks it
/// to look for due series once more before it ends.
fn runs() -> &'static std::sync::Mutex<HashMap<(usize, i64), bool>> {
    static RUNS: std::sync::OnceLock<std::sync::Mutex<HashMap<(usize, i64), bool>>> =
        std::sync::OnceLock::new();
    RUNS.get_or_init(Default::default)
}
fn lookups() -> &'static tokio::sync::Semaphore {
    static LOOKUPS: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
    LOOKUPS.get_or_init(|| tokio::sync::Semaphore::new(SERVER_LOOKUPS))
}

/// Refresh every due series of this profile in the background, batch after
/// batch until none is due. Never blocks the caller.
pub(crate) fn schedule(app: &App, lease: &ResourceLease, profile: i64) {
    // Only the profile's own session refreshes it (as Details hydration does).
    let auth::Principal::Account {
        profile_id: Some(selected),
        ..
    } = &lease.principal
    else {
        return;
    };
    if *selected != profile {
        return;
    }
    let key = (Arc::as_ptr(&app.db) as usize, profile);
    {
        let Ok(mut runs) = runs().lock() else {
            return;
        };
        if let Some(again) = runs.get_mut(&key) {
            *again = true;
            return;
        }
        runs.insert(key, false);
    }
    let app = app.clone().with_lease(lease.clone());
    let lease = lease.clone();
    tokio::spawn(async move {
        loop {
            match claim(&app, &lease, profile) {
                Ok(batch) if !batch.is_empty() => refresh(&app, &lease, profile, batch).await,
                _ => {
                    let Ok(mut runs) = runs().lock() else {
                        return;
                    };
                    if runs.get(&key) == Some(&true) {
                        runs.insert(key, false);
                        continue;
                    }
                    runs.remove(&key);
                    return;
                }
            }
        }
    });
}

/// Pick up to one batch of due series and lease them for `CLAIM_SECS`, so a
/// concurrent run never fetches them twice and an interrupted one resumes.
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
        if candidates.len() >= BATCH {
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
        let row: Option<(String, i64, Option<String>, i64)> = db
            .query_row(
                "SELECT from_id,next_check_at,document,failures FROM continuation_releases WHERE profile_id=?1 AND series_id=?2",
                params![profile, series],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .map_err(db_error)?;
        let due = match &row {
            None => true,
            Some((from, next_check_at, raw, failures)) => {
                *from != anchor.identity.id
                    || *next_check_at <= now
                    // Facts from another configuration are refetched once;
                    // a failing refetch then waits for its back-off.
                    || *failures == 0 && raw
                        .as_deref()
                        .and_then(|raw| serde_json::from_str::<Document>(raw).ok())
                        .map_or(Ok(false), |document| {
                            current(&db, app, lease, profile, &document).map(|ok| !ok)
                        })?
            }
        };
        if !due {
            continue;
        }
        // Keep the previous facts for the same finished episode until the
        // refresh replaces them; another episode's facts no longer apply.
        db.execute("INSERT INTO continuation_releases(profile_id,series_id,from_id,checked_at,document,next_check_at,failures) VALUES(?1,?2,?3,?4,NULL,?5,0) ON CONFLICT(profile_id,series_id) DO UPDATE SET document=CASE WHEN continuation_releases.from_id=excluded.from_id THEN continuation_releases.document END,failures=CASE WHEN continuation_releases.from_id=excluded.from_id THEN continuation_releases.failures ELSE 0 END,from_id=excluded.from_id,next_check_at=excluded.next_check_at",params![profile,series,anchor.identity.id,now,now+CLAIM_SECS]).map_err(db_error)?;
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
            let Ok(_permit) = lookups().acquire().await else {
                return;
            };
            tokio::time::sleep(LOOKUP_SPACING).await;
            let series = candidate.series.clone();
            let from = candidate.anchor.identity.id.clone();
            if refresh_one(app, lease, profile, simkl, revision, candidate).await.is_err() {
                failed(app, profile, &series, &from);
            }
        })
        .buffer_unordered(CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
}

/// Back off a series whose metadata could not be read.
fn failed(app: &App, profile: i64, series: &str, from: &str) {
    let Ok(db) = app.db.lock() else {
        return;
    };
    let now = util::now();
    let failures: i64 = db
        .query_row(
            "SELECT failures FROM continuation_releases WHERE profile_id=?1 AND series_id=?2 AND from_id=?3",
            params![profile, series, from],
            |r| r.get(0),
        )
        .unwrap_or(0)
        + 1;
    let _ = db.execute(
        "UPDATE continuation_releases SET failures=?4,next_check_at=?5 WHERE profile_id=?1 AND series_id=?2 AND from_id=?3",
        params![profile, series, from, failures, now + retry_after(failures)],
    );
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
    // Background checks are spaced and rare, so they skip the add-on response
    // cache: a check right after an air time must see the provider's list.
    let (parent, episodes) = tokio::time::timeout(
        LOOKUP_TIMEOUT,
        super::metadata::document(app, lease, profile, &input, simkl, None, None, true),
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
    let visible = |db: &Connection| -> Result<_, ApiError> {
        let found = state_db(db, app, lease, profile, &candidate.series, &candidate.anchor, now)?;
        let verdict = verdicts_db(db, profile)?.remove(&candidate.series);
        Ok((
            found.map(|f| (f.count, f.next.identity.id, f.first_released_at_millis, f.released_at_millis)),
            verdict,
        ))
    };
    let before = visible(&db)?;
    store_db(
        &db,
        lease,
        profile,
        &candidate.series,
        &input.identity,
        &episodes,
        configuration,
        &addon_revision,
        parent.landscape.as_deref(),
    )?;
    if visible(&db)? != before {
        db.execute("INSERT INTO engine_library_revisions VALUES(?1,1) ON CONFLICT(profile_id) DO UPDATE SET revision=revision+1",[profile]).map_err(db_error)?;
    }
    Ok(())
}
