//! Calendar episodes of the profile's own library series ("your shows"),
//! including series a Stremio import saved to the library.
//!
//! Air dates come from the episode documents Continue Watching reads for its
//! new-episode facts, through the same `metadata::series_document`: the Simkl
//! episode list when Simkl is the browsing integration, otherwise the metadata
//! add-ons' `videos[].released`, and the series' own provider when that one
//! cannot serve it. A date the release badges have is the date shown here.
//!
//! One profile's list is cached for `TTL`, keyed by account, configuration,
//! add-on revision and the set of library series, and concurrent reads of the
//! same profile share one fetch. At most `MAX_SERIES` series (most recently
//! added first) are looked up; failed or unfinished lookups make the list
//! partial, which is kept only for `PARTIAL_TTL`.
use super::*;
use std::collections::HashMap;
use std::time::Instant;

/// Library series looked up per profile, most recently added first.
pub(crate) const MAX_SERIES: usize = 50;
const CONCURRENCY: usize = 4;
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(8);
/// A cold fetch returns what it has after this long; the rest follows on a
/// read after `PARTIAL_TTL`.
const DEADLINE: Duration = Duration::from_secs(12);
const TTL: Duration = Duration::from_secs(30 * 60);
/// How long a partial list (a lookup failed or missed the deadline) is kept;
/// also the client's refresh hint while lookups are pending.
pub(crate) const PARTIAL_TTL: Duration = Duration::from_secs(15);
/// Episodes kept around now; calendar windows outside it have no library rows.
pub(crate) const HORIZON_MILLIS: u64 = 180 * 24 * 3_600_000;
/// Cached profiles; beyond this, reads fetch without caching.
const CACHED_PROFILES: usize = 1024;

#[derive(Clone)]
pub(crate) struct Episode {
    /// The library card of the series (art filled in from its metadata).
    pub(crate) series: Card,
    /// The typed episode card, with the series as its parent.
    pub(crate) episode: Card,
    pub(crate) released_at_millis: u64,
    /// Identities the series is known by (see `identity_keys`), to recognise
    /// the same episode in the public schedule.
    pub(crate) keys: Vec<String>,
}

pub(crate) struct Episodes {
    pub(crate) episodes: Vec<Episode>,
    /// Some series were not looked up before the deadline: read again after
    /// `PARTIAL_TTL`.
    pub(crate) pending: bool,
    /// Some lookups failed; retried after `PARTIAL_TTL`.
    failed: bool,
}

#[derive(Clone, PartialEq)]
struct CacheKey {
    account: i64,
    configuration: u64,
    addon_revision: String,
    series: Vec<String>,
}
type Slot = tokio::sync::Mutex<Option<(Instant, CacheKey, Arc<Episodes>)>>;
/// Per database and profile. The `Weak` keeps the database's allocation, so
/// its address cannot name another database while the slot exists.
type Slots = HashMap<(usize, i64), (std::sync::Weak<std::sync::Mutex<Connection>>, Arc<Slot>)>;

fn cache() -> &'static std::sync::Mutex<Slots> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Slots>> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Whether the profile has library series the calendar can look up (the
/// Calendar destination's "your shows" source).
pub(crate) fn has_series_db(db: &Connection, profile: i64) -> Result<bool, ApiError> {
    Ok(!series_db(db, profile)?.is_empty())
}

fn series_db(db: &Connection, profile: i64) -> Result<Vec<Card>, ApiError> {
    let mut query = db
        .prepare("SELECT id,type,name,poster,context,added_at_millis FROM favorites WHERE profile_id=?1 AND type='series' ORDER BY added_at_millis IS NULL,added_at_millis DESC,id LIMIT ?2")
        .map_err(db_error)?;
    let items = query
        .query_map(params![profile, MAX_SERIES as i64], |r| {
            Ok(json!({"id":r.get::<_,String>(0)?,"type":r.get::<_,String>(1)?,"name":r.get::<_,String>(2)?,"poster":r.get::<_,Option<String>>(3)?,"added_at_millis":r.get::<_,Option<u64>>(5)?,"engine_favorite":serde_json::from_str::<Value>(&r.get::<_,Option<String>>(4)?.unwrap_or_default()).unwrap_or(Value::Null)}))
        })
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    Ok(items
        .iter()
        .filter_map(super::super::library::card)
        .filter(|card| {
            card.identity.kind == "series"
                && card.identity.episode.is_none()
                && viptv_engine::continuation::validate_identity(&card.identity).is_ok()
        })
        .collect())
}

/// The released and upcoming episodes of the profile's library series within
/// `HORIZON_MILLIS` of now. `simkl` is whether Simkl is the profile's
/// browsing integration. Restricted (kids) sessions are not served here.
pub(crate) async fn episodes(
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    simkl: bool,
) -> Result<Arc<Episodes>, ApiError> {
    let (key, series) = {
        let db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        lease.validate(&db)?;
        app.require_profile(&db, profile)?;
        if crate::kids::restricted(&db, &lease.principal)? {
            return Err(auth::forbidden());
        }
        let series = series_db(&db, profile)?;
        let key = CacheKey {
            account: lease.principal.account_id().ok_or_else(auth::forbidden)?,
            configuration: configuration_revision(&db, profile)?,
            addon_revision: app.addons.revision_db(&db)?,
            series: series.iter().map(|card| card.identity.id.clone()).collect(),
        };
        (key, series)
    };
    if series.is_empty() {
        return Ok(Arc::new(Episodes {
            episodes: vec![],
            pending: false,
            failed: false,
        }));
    }
    let slot = {
        let mut slots = cache()
            .lock()
            .map_err(|_| ApiError::from("Calendar unavailable"))?;
        let id = (Arc::as_ptr(&app.db) as usize, profile);
        if slots.len() >= CACHED_PROFILES && !slots.contains_key(&id) {
            // Keep fetches in flight coalesced; idle profiles can fetch again.
            slots.retain(|_, (db, slot)| db.strong_count() > 0 && Arc::strong_count(slot) > 1);
        }
        if slots.len() >= CACHED_PROFILES && !slots.contains_key(&id) {
            Arc::default()
        } else {
            slots
                .entry(id)
                .or_insert_with(|| (Arc::downgrade(&app.db), Default::default()))
                .1
                .clone()
        }
    };
    let mut cached = slot.lock().await;
    if let Some((_, _, value)) = cached.as_ref().filter(|(at, cached_key, value)| {
        let partial = value.pending || value.failed;
        *cached_key == key && at.elapsed() < if partial { PARTIAL_TTL } else { TTL }
    }) {
        return Ok(value.clone());
    }
    let value = Arc::new(fetch(app, lease, profile, simkl, series).await);
    configuration::validate_revision(app, profile, key.configuration)?;
    *cached = Some((Instant::now(), key, value.clone()));
    Ok(value)
}

/// The identities a title is known by, comparable across providers: its id,
/// `imdb:tt…` for a `tt…` id, and every `namespace:value` external id.
pub(crate) fn identity_keys<'a>(cards: impl IntoIterator<Item = &'a Card>) -> Vec<String> {
    let mut keys = Vec::new();
    for card in cards {
        let id = &card.identity.id;
        keys.push(id.clone());
        if id.starts_with("tt") {
            keys.push(format!("imdb:{id}"));
        }
        for external in &card.identity.ids {
            keys.push(format!("{}:{}", external.namespace, external.value));
        }
    }
    keys.sort();
    keys.dedup();
    keys
}

async fn document(
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    simkl: bool,
    series: &Card,
) -> Result<(Card, Vec<EpisodeAvailability>), ApiError> {
    let request = NextEpisodeRequest {
        identity: series.identity.clone(),
        parent_identity: Some(series.identity.clone()),
    };
    super::metadata::series_document(app, lease, profile, &request, simkl, false, LOOKUP_TIMEOUT)
        .await
}

async fn fetch(
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    simkl: bool,
    series: Vec<Card>,
) -> Episodes {
    use futures::StreamExt;
    let deadline = tokio::time::Instant::now() + DEADLINE;
    let total = series.len();
    let now = util::now_millis().max(0) as u64;
    let mut lookups = futures::stream::iter(series)
        .map(|series| async move {
            let found = document(app, lease, profile, simkl, &series).await;
            (series, found)
        })
        .buffer_unordered(CONCURRENCY);
    let (mut answered, mut failed, mut episodes) = (0, false, Vec::new());
    while let Ok(Some((mut series, found))) = tokio::time::timeout_at(deadline, lookups.next()).await {
        answered += 1;
        let (parent, entries) = match found {
            Ok((parent, entries))
                if parent.identity.id == series.identity.id
                    && parent.identity.kind == series.identity.kind =>
            {
                (parent, entries)
            }
            _ => {
                failed = true;
                continue;
            }
        };
        series.poster = series.poster.or(parent.poster.clone());
        series.landscape = series.landscape.or(parent.landscape.clone());
        series.logo = series.logo.or(parent.logo.clone());
        let keys = identity_keys([&series, &parent]);
        for entry in entries {
            let Some(at) = entry.released_at_millis else {
                continue;
            };
            if at.abs_diff(now) > HORIZON_MILLIS || entry.card.identity.episode.is_none() {
                continue;
            }
            let mut episode = entry.card;
            episode
                .parent_identity
                .get_or_insert_with(|| series.identity.clone());
            episodes.push(Episode {
                series: series.clone(),
                episode,
                released_at_millis: at,
                keys: keys.clone(),
            });
        }
    }
    Episodes {
        episodes,
        pending: answered < total,
        failed,
    }
}
