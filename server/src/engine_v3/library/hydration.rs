use super::*;
use futures::{stream, StreamExt};
use std::time::Instant;
use viptv_engine::presentation::Card;

const ROWS_PER_PASS: usize = 200;
const LOOKUPS: usize = 4;
const LOOKUP_SPACING: Duration = Duration::from_millis(100);
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(15);
const WRITE_BATCH: usize = 16;
const RUNS: usize = 2;
const RETRY_AFTER: Duration = Duration::from_secs(60 * 60);
pub(super) const REFRESH_AFTER_MILLIS: u64 = 4000;

#[derive(Default)]
struct Profile {
    running: bool,
    again: bool,
    failed: HashMap<(String, String), Instant>,
    done: usize,
    filled: usize,
    total: usize,
}

pub(crate) struct Jobs {
    profiles: Mutex<HashMap<i64, Profile>>,
    runs: tokio::sync::Semaphore,
    #[cfg(test)]
    pub(crate) enabled: std::sync::atomic::AtomicBool,
}
impl Default for Jobs {
    fn default() -> Self {
        Self {
            profiles: Default::default(),
            runs: tokio::sync::Semaphore::new(RUNS),
            #[cfg(test)]
            enabled: Default::default(),
        }
    }
}

fn thin(card: Option<&Card>) -> bool {
    card.is_none_or(|card| card.description.is_none() && card.metadata.is_none())
}

pub(crate) fn stored(raw: &str, kind: &str, id: &str) -> Option<Card> {
    serde_json::from_str::<Card>(raw)
        .ok()
        .filter(|card| card.identity.kind == kind && card.identity.id == id)
}

fn fill(existing: Option<Card>, resolved: Card, name: &str, poster: Option<&str>) -> Card {
    let mut card = match existing {
        None => resolved,
        Some(mut card) => {
            card.category = card.category.or(resolved.category);
            card.metadata = card.metadata.or(resolved.metadata);
            card.subtitle = card.subtitle.or(resolved.subtitle);
            card.description = card.description.or(resolved.description);
            card.poster = card.poster.or(resolved.poster);
            card.landscape = card.landscape.or(resolved.landscape);
            card.logo = card.logo.or(resolved.logo);
            if card.badges.is_empty() {
                card.badges = resolved.badges;
            }
            card
        }
    };
    card.title = name.to_owned();
    if let Some(poster) = poster {
        card.poster = Some(poster.to_owned());
    }
    card.progress = None;
    card.added_at_millis = None;
    card.metadata_origin = None;
    super::super::art::fill(&mut card);
    card
}

pub(crate) fn schedule(app: &App, lease: &ResourceLease, profile: i64) -> bool {
    let jobs = app.library_hydration.clone();
    #[cfg(test)]
    if !jobs.enabled.load(std::sync::atomic::Ordering::SeqCst) {
        return false;
    }
    let auth::Principal::Account {
        profile_id: Some(selected),
        ..
    } = &lease.principal
    else {
        return false;
    };
    if *selected != profile {
        return false;
    }
    let Ok(mut profiles) = jobs.profiles.lock() else {
        return false;
    };
    let state = profiles.entry(profile).or_default();
    if state.running {
        state.again = true;
        return true;
    }
    state.running = true;
    state.again = false;
    state.done = 0;
    state.filled = 0;
    state.total = 0;
    drop(profiles);
    let app = app.clone().with_lease(lease.clone());
    let lease = lease.clone();
    tokio::spawn(async move {
        let _done = Done(app.library_hydration.clone(), profile);
        loop {
            let more = match app.library_hydration.runs.acquire().await {
                Ok(_turn) => pass(&app, &lease, profile).await.unwrap_or(false),
                Err(_) => false,
            };
            // Stopping and clearing `running` need one lock, or a concurrent import starts a second run.
            let Ok(mut profiles) = app.library_hydration.profiles.lock() else {
                break;
            };
            let state = profiles.entry(profile).or_default();
            if !(more || std::mem::take(&mut state.again)) {
                state.running = false;
                break;
            }
        }
    });
    true
}

struct Done(Arc<Jobs>, i64);
impl Drop for Done {
    fn drop(&mut self) {
        if let Ok(mut profiles) = self.0.profiles.lock() {
            let state = profiles.entry(self.1).or_default();
            state.running = false;
        }
    }
}

async fn pass(app: &App, lease: &ResourceLease, profile: i64) -> Result<bool, ApiError> {
    let rows = {
        let db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        app.require_profile(&db, profile)?;
        if kids::restricted(&db, &lease.principal)? {
            return Ok(false);
        }
        let mut query = db
            .prepare("SELECT type,id,context FROM favorites WHERE profile_id=?1 AND type IN ('movie','series') ORDER BY added_at_millis DESC,rowid DESC LIMIT 10001")
            .map_err(db_error)?;
        let rows = query
            .query_map([profile], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        rows
    };
    let now = Instant::now();
    let mut wanted: Vec<(String, String)> = {
        let mut profiles = app
            .library_hydration
            .profiles
            .lock()
            .map_err(|_| ApiError::from("Library unavailable"))?;
        let state = profiles.entry(profile).or_default();
        state
            .failed
            .retain(|_, at| now.duration_since(*at) < RETRY_AFTER);
        let wanted: Vec<_> = rows
            .into_iter()
            .filter(|(kind, id, raw)| thin(stored(raw, kind, id).as_ref()))
            .map(|(kind, id, _)| (kind, id))
            .filter(|key| !state.failed.contains_key(key))
            .collect();
        state.total = state.done + wanted.len();
        wanted
    };
    let more = wanted.len() > ROWS_PER_PASS;
    wanted.truncate(ROWS_PER_PASS);
    if wanted.is_empty() {
        return Ok(false);
    }
    let mut interval = tokio::time::interval(LOOKUP_SPACING);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let pace = tokio::sync::Mutex::new(interval);
    let mut lookups = stream::iter(wanted)
        .map(|(kind, id)| {
            let pace = &pace;
            async move {
                pace.lock().await.tick().await;
                let card = tokio::time::timeout(
                    LOOKUP_TIMEOUT,
                    super::super::catalog::title_card(
                        app.clone(),
                        lease.clone(),
                        kind.clone(),
                        id.clone(),
                    ),
                )
                .await
                .ok()
                .and_then(Result::ok)
                .filter(|card| card.identity.kind == kind && card.identity.id == id);
                ((kind, id), card)
            }
        })
        .buffer_unordered(LOOKUPS);
    let mut batch = Vec::new();
    while let Some((key, card)) = lookups.next().await {
        if let Ok(mut profiles) = app.library_hydration.profiles.lock() {
            let state = profiles.entry(profile).or_default();
            state.done = (state.done + 1).min(state.total);
        }
        match card {
            Some(card) => batch.push((key, card)),
            None => {
                lease.validate(
                    &*app
                        .db
                        .lock()
                        .map_err(|_| ApiError::from("Database unavailable"))?,
                )?;
                remember_failed(app, profile, vec![key]);
            }
        }
        if batch.len() >= WRITE_BATCH {
            let unfilled = write(app, lease, profile, std::mem::take(&mut batch))?;
            remember_failed(app, profile, unfilled);
        }
    }
    let unfilled = write(app, lease, profile, batch)?;
    remember_failed(app, profile, unfilled);
    Ok(more)
}

fn remember_failed(app: &App, profile: i64, keys: Vec<(String, String)>) {
    if let Ok(mut profiles) = app.library_hydration.profiles.lock() {
        let failed = &mut profiles.entry(profile).or_default().failed;
        for key in keys {
            if failed.len() < 10_000 {
                failed.insert(key, Instant::now());
            }
        }
    }
}

fn write(
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    batch: Vec<((String, String), Card)>,
) -> Result<Vec<(String, String)>, ApiError> {
    let mut unfilled = Vec::new();
    let mut filled = 0;
    if batch.is_empty() {
        return Ok(unfilled);
    }
    let mut db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_profile(&db, profile)?;
    if kids::restricted(&db, &lease.principal)? {
        return Err(auth::forbidden());
    }
    let tx = db.transaction().map_err(db_error)?;
    for ((kind, id), resolved) in batch {
        let row: Option<(String, Option<String>, String)> = tx
            .query_row(
                "SELECT name,poster,context FROM favorites WHERE profile_id=?1 AND type=?2 AND id=?3",
                params![profile, kind, id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(db_error)?;
        let Some((name, poster, raw)) = row else {
            continue;
        };
        let existing = stored(&raw, &kind, &id);
        if !thin(existing.as_ref()) {
            continue;
        }
        let card = fill(existing, resolved, &name, poster.as_deref());
        let context = serde_json::to_string(&card).ok();
        let (Some(context), Ok(())) = (context, card.validate_library()) else {
            unfilled.push((kind, id));
            continue;
        };
        let still_thin = thin(Some(&card));
        if still_thin {
            unfilled.push((kind.clone(), id.clone()));
        }
        let changed = tx.execute(
            "UPDATE favorites SET context=?4,poster=COALESCE(poster,?5) WHERE profile_id=?1 AND type=?2 AND id=?3 AND context=?6",
            params![profile, kind, id, context, card.poster, raw],
        )
        .map_err(db_error)?;
        filled += usize::from(changed == 1 && !still_thin);
    }
    tx.commit().map_err(db_error)?;
    drop(db);
    if let Ok(mut profiles) = app.library_hydration.profiles.lock() {
        profiles.entry(profile).or_default().filled += filled;
    }
    Ok(unfilled)
}

pub(crate) fn progress(app: &App, profile: i64) -> Value {
    let (running, done, filled, total) = app
        .library_hydration
        .profiles
        .lock()
        .ok()
        .and_then(|profiles| {
            profiles
                .get(&profile)
                .map(|s| (s.running, s.done, s.filled, s.total))
        })
        .unwrap_or_default();
    json!({"running":running,"done":done,"filled":filled,"total":total})
}

pub(crate) async fn status(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
) -> ApiResult {
    let app = app.with_lease(lease);
    blocking(move || {
        app.require_profile(
            &*app
                .db
                .lock()
                .map_err(|_| ApiError::from("Database unavailable"))?,
            profile,
        )?;
        Ok(axum::Json(progress(&app, profile)))
    })
    .await
}

pub(crate) fn running(app: &App, profile: i64) -> bool {
    app.library_hydration
        .profiles
        .lock()
        .is_ok_and(|p| p.get(&profile).is_some_and(|s| s.running))
}

#[cfg(test)]
pub(crate) async fn settled(app: &App, profile: i64) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while running(app, profile) {
        assert!(
            Instant::now() < deadline,
            "library hydration did not settle"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
