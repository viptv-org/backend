use super::*;
use serde::Serialize;
use viptv_engine::origin::MetadataOrigin;
use std::collections::{HashMap, HashSet};
use viptv_engine::playback::PlaybackRecord;

const STORED_EPISODES: usize = 10000;
const STORED_CARDS: usize = 4;
const AIRED_GRACE_SECS: i64 = 5 * 60;
const ACTIVE_SECS: i64 = 24 * 3600;
const DORMANT_SECS: i64 = 7 * 24 * 3600;
const DORMANT_AFTER_SECS: i64 = 180 * 24 * 3600;
const RETRY_SECS: i64 = 3600;
const CLAIM_SECS: i64 = 15 * 60;
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

#[derive(Serialize, Deserialize)]
struct Document {
    account_id: i64,
    configuration_revision: u64,
    addon_revision: String,
    episodes: Vec<Stored>,
    #[serde(default)]
    landscape: Option<String>,
    #[serde(default)]
    origin: Option<MetadataOrigin>,
}

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
    origin: Option<&MetadataOrigin>,
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
        origin: origin.cloned(),
    };
    let document = serde_json::to_string(&document)
        .map_err(|_| ApiError::from("Release facts unavailable"))?;
    let now = util::now();
    let (next_check_at, failures) = if at.is_some() {
        (next_check(now, episodes), 0)
    } else {
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

pub(crate) struct UpNext {
    pub card: Card,
    pub count: u32,
    pub released_at_millis: Vec<u64>,
    pub new_release_at_millis: Option<u64>,
    pub origin: Option<MetadataOrigin>,
}

fn pending_episodes(
    anchor: &MediaIdentity,
    episodes: &[EpisodeAvailability],
    watched: impl Fn(&Card) -> bool,
    now_millis: u64,
) -> Option<NewEpisodes> {
    viptv_engine::continuation::new_episodes(anchor, 0, episodes, watched, now_millis)
}

pub(crate) fn up_next_db(
    db: &Connection,
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    series: &str,
    anchor: &PlaybackRecord,
    now_millis: u64,
) -> Result<Option<UpNext>, ApiError> {
    let Some(facts) = facts_db(db, app, lease, profile, series, anchor)? else {
        return Ok(None);
    };
    let watched = Watched::read(db, profile, series)?;
    let Some(pending) = pending_episodes(
        &facts.anchor,
        &facts.episodes,
        |card| watched.contains(card),
        now_millis,
    ) else {
        return Ok(None);
    };
    Ok(Some(UpNext {
        card: pending.next,
        count: pending.count,
        released_at_millis: pending.released_at_millis,
        new_release_at_millis: (pending.first_released_at_millis > anchor.updated_at_millis)
            .then_some(pending.first_released_at_millis),
        origin: facts.origin,
    }))
}

pub(crate) fn upcoming_db(
    db: &Connection,
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    series: &str,
    anchor: &PlaybackRecord,
    now_millis: u64,
) -> Result<Option<viptv_engine::continuation::NextEpisode>, ApiError> {
    let Some(facts) = facts_db(db, app, lease, profile, series, anchor)? else {
        return Ok(None);
    };
    Ok(viptv_engine::continuation::upcoming_episode(
        &facts.anchor,
        &facts.episodes,
        now_millis,
    ))
}

struct Facts {
    anchor: MediaIdentity,
    episodes: Vec<EpisodeAvailability>,
    origin: Option<MetadataOrigin>,
}

fn facts_db(
    db: &Connection,
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    series: &str,
    anchor: &PlaybackRecord,
) -> Result<Option<Facts>, ApiError> {
    let Some((from, document)) = document_db(db, profile, series)? else {
        return Ok(None);
    };
    if from != anchor.identity.id {
        return Ok(None);
    }
    if !current(db, app, lease, profile, &document)? {
        return Ok(None);
    }
    let Some(stored) = document.episodes.first() else {
        return Ok(None);
    };
    if stored.identity.id != anchor.identity.id {
        return Ok(None);
    }
    let played_from = stored.card.as_ref().map(|card| card.provider_id.as_str());
    if !crate::continuation::same_origin(&anchor.provider_id, played_from) {
        return Ok(None);
    }
    Ok(Some(Facts {
        anchor: stored.identity.clone(),
        episodes: availability(document.episodes),
        origin: document.origin,
    }))
}

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

pub(crate) fn caption(
    identity: &MediaIdentity,
    episode_title: &str,
    series_title: &str,
) -> Option<String> {
    let coordinates = identity.episode.as_ref().map(|c| {
        let episode = format!("E{}", c.episode);
        c.season
            .map_or(episode.clone(), |season| format!("S{season} {episode}"))
    });
    let named = (!episode_title.is_empty()
        && episode_title != identity.id
        && episode_title != series_title)
        .then(|| episode_title.to_owned());
    match (coordinates, named) {
        (Some(at), Some(name)) => Some(format!("{at} · {name}")),
        (Some(at), None) => Some(at),
        (None, name) => name,
    }
}

pub(crate) fn present(found: &UpNext, anchor: &PlaybackRecord, verified: Option<Card>) -> Card {
    let mut card = verified
        .filter(|card| card.identity == found.card.identity)
        .unwrap_or_else(|| found.card.clone());
    if card.metadata_origin.is_none() {
        card.metadata_origin = found.origin.clone();
    }
    let episode_title = std::mem::replace(&mut card.title, anchor.title.clone());
    card.subtitle = caption(&card.identity, &episode_title, &anchor.title);
    if anchor.poster.is_some() {
        card.poster = anchor.poster.clone();
    }
    if card.landscape.is_none() {
        card.landscape = anchor.landscape.clone();
    }
    card.progress = None;
    card.badges
        .retain(|badge| !viptv_engine::continuation::is_new_episodes_badge(badge));
    if found.new_release_at_millis.is_some() {
        card.badges.insert(
            0,
            viptv_engine::continuation::new_episodes_badge(found.count),
        );
    }
    card
}

struct Candidate {
    series: String,
    anchor: PlaybackRecord,
}

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

fn runs() -> &'static std::sync::Mutex<HashMap<(usize, i64), bool>> {
    static RUNS: std::sync::OnceLock<std::sync::Mutex<HashMap<(usize, i64), bool>>> =
        std::sync::OnceLock::new();
    RUNS.get_or_init(Default::default)
}
fn lookups() -> &'static tokio::sync::Semaphore {
    static LOOKUPS: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
    LOOKUPS.get_or_init(|| tokio::sync::Semaphore::new(SERVER_LOOKUPS))
}

pub(crate) fn schedule(app: &App, lease: &ResourceLease, profile: i64) {
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

fn claim(app: &App, lease: &ResourceLease, profile: i64) -> Result<Vec<Candidate>, ApiError> {
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_profile(&db, profile)?;
    lease.validate(&db)?;
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
        if let Some(hidden) = item["queue_hidden_at"].as_u64() {
            let returned = up_next_db(
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
    let (parent, episodes) = super::metadata::series_document(
        app,
        lease,
        profile,
        &input,
        simkl,
        true,
        LOOKUP_TIMEOUT,
    )
    .await?;
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
        let found = up_next_db(db, app, lease, profile, &candidate.series, &candidate.anchor, now)?;
        let verdict = verdicts_db(db, profile)?.remove(&candidate.series);
        Ok((
            found.map(|f| {
                (
                    f.count,
                    f.card.identity.id,
                    f.new_release_at_millis,
                    f.released_at_millis,
                )
            }),
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
        parent.metadata_origin.as_ref(),
    )?;
    if visible(&db)? != before {
        db.execute("INSERT INTO engine_library_revisions VALUES(?1,1) ON CONFLICT(profile_id) DO UPDATE SET revision=revision+1",[profile]).map_err(db_error)?;
    }
    Ok(())
}
