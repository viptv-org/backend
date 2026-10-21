//! The profile's `Watched` card badge, derived from facts the server already
//! keeps: no new table and no provider request.
//!
//! - A movie is watched when its progress row is complete under
//!   [`crate::library::watched`] (the History/correction rule: 95 %, an explicit
//!   "Mark watched", or an imported completion), or when an origin-scoped
//!   playback record has a completion time.
//! - A series is watched when the profile is caught up on it: the episode with
//!   the newest activity is complete *and* the next-episode resolver found no
//!   later aired episode after it (`caught_up`, or `upcoming` when the next one
//!   has not aired yet). A new aired episode turns the verdict into `next` and
//!   the badge disappears. A Simkl import whose show is `completed` with no
//!   unwatched aired episode also counts.
//!
//! Episode cards never get this badge; their own completion is shown on the
//! episode rows. The badge is private profile state, so every response
//! recomputes it and any `Watched` already on a card (cached metadata, a
//! client-sent card) is removed first.
use super::*;
use std::collections::{BTreeSet, HashSet};
use viptv_engine::presentation::Card;

pub(crate) const BADGE: &str = "Watched";

#[derive(Default)]
pub(crate) struct Watched {
    /// `(kind, id)` for shared identities (`tt…`, `simkl:…`) and IMDb hints.
    titles: HashSet<(String, String)>,
    /// Storage keys of completed origin-scoped movie playback.
    scoped: BTreeSet<scoped_library::Key>,
}

impl Watched {
    pub(crate) fn load(db: &Connection, profile: i64) -> Result<Self, ApiError> {
        let mut watched = Self::default();
        let mut movies = db
            .prepare("SELECT id,position,duration,context FROM progress WHERE profile_id=?1 AND type='movie' LIMIT 50001")
            .map_err(db_error)?;
        let rows = movies
            .query_map([profile], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?, r.get::<_, f64>(2)?, r.get::<_, String>(3)?))
            })
            .map_err(db_error)?;
        for row in rows {
            let (id, position, duration, context) = row.map_err(db_error)?;
            let context = serde_json::from_str::<Value>(&context).unwrap_or(Value::Null);
            if crate::library::watched(&json!({"position":position,"duration":duration}), &context) {
                watched.insert("movie", &id, &context);
            }
        }
        // The newest activity per show decides; the resolver verdict must be for
        // that exact episode, so an older "caught up" never outlives a rewatch.
        let mut series = db
            .prepare("WITH ranked AS (SELECT id,title_id,position,duration,context,ROW_NUMBER() OVER(PARTITION BY title_id ORDER BY updated_at DESC,rowid DESC) AS rank FROM progress WHERE profile_id=?1 AND type='series' AND title_id!='') SELECT r.title_id,r.position,r.duration,r.context,c.result FROM ranked r JOIN continuation_cache c ON c.profile_id=?1 AND c.series_id=r.title_id AND c.from_id=r.id WHERE r.rank=1 LIMIT 50001")
            .map_err(db_error)?;
        let rows = series
            .query_map([profile], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, f64>(1)?,
                    r.get::<_, f64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })
            .map_err(db_error)?;
        for row in rows {
            let (title, position, duration, context, verdict) = row.map_err(db_error)?;
            let context = serde_json::from_str::<Value>(&context).unwrap_or(Value::Null);
            let verdict = serde_json::from_str::<Value>(&verdict).unwrap_or(Value::Null);
            if matches!(verdict["status"].as_str(), Some("caught_up" | "upcoming"))
                && crate::library::watched(&json!({"position":position,"duration":duration}), &context)
            {
                watched.insert("series", &title, &context);
            }
        }
        let mut imported = db
            .prepare("SELECT local_kind,local_id,statistics FROM simkl_import_entries WHERE profile_id=?1 LIMIT 10001")
            .map_err(db_error)?;
        let rows = imported
            .query_map([profile], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
            })
            .map_err(db_error)?;
        for row in rows {
            let (kind, id, statistics) = row.map_err(db_error)?;
            let Ok(statistics) =
                serde_json::from_str::<viptv_engine::library::LibraryStatistics>(&statistics)
            else {
                continue;
            };
            if statistics.watched && statistics.has_unwatched_aired_episodes != Some(true) {
                watched.insert(&kind, &id, &Value::Null);
            }
        }
        let mut scoped = db
            .prepare("SELECT identity_scope,kind,identity_id FROM engine_scoped_records WHERE profile_id=?1 AND record_kind='playback' AND kind='movie' AND json_extract(value,'$.completed_at_millis') IS NOT NULL LIMIT 50001")
            .map_err(db_error)?;
        let rows = scoped
            .query_map([profile], |r| {
                Ok(scoped_library::Key(r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .map_err(db_error)?;
        for key in rows {
            watched.scoped.insert(key.map_err(db_error)?);
        }
        Ok(watched)
    }

    /// Best effort for a response: a failure draws no badge, never a failed page.
    pub(crate) fn for_profile(app: &App, profile: i64) -> Self {
        app.db
            .lock()
            .ok()
            .and_then(|db| Self::load(&db, profile).ok())
            .unwrap_or_default()
    }

    fn insert(&mut self, kind: &str, id: &str, context: &Value) {
        if !matches!(kind, "movie" | "series") {
            return;
        }
        self.titles.insert((kind.to_owned(), id.to_owned()));
        if let Some(imdb) = context["imdb_id"].as_str().filter(|v| v.starts_with("tt")) {
            self.titles.insert((kind.to_owned(), imdb.to_owned()));
        }
    }

    pub(crate) fn contains(&self, card: &Card) -> bool {
        let identity = &card.identity;
        if identity.episode.is_some() || !matches!(identity.kind.as_str(), "movie" | "series") {
            return false;
        }
        let shared = |id: &str| self.titles.contains(&(identity.kind.clone(), id.to_owned()));
        if viptv_engine::origin::shared_identity(identity) && shared(&identity.id) {
            return true;
        }
        if identity
            .ids
            .iter()
            .any(|id| id.namespace == "imdb" && id.value.starts_with("tt") && shared(&id.value))
        {
            return true;
        }
        identity.kind == "movie"
            && scoped_library::raw_key(card).is_ok_and(|key| self.scoped.contains(&key))
    }

    pub(crate) fn mark(&self, card: &mut Card) {
        card.badges.retain(|badge| !badge.eq_ignore_ascii_case(BADGE));
        // The card contract bounds badges at 32; never make a valid card invalid.
        if card.badges.len() < 32 && self.contains(card) {
            card.badges.push(BADGE.into());
        }
    }

    pub(crate) fn mark_all<'a>(&self, cards: impl IntoIterator<Item = &'a mut Card>) {
        for card in cards {
            self.mark(card);
        }
    }

    /// For responses that are already JSON (search rows, catalog pages).
    pub(crate) fn mark_values(&self, cards: &mut Value) {
        for value in cards.as_array_mut().into_iter().flatten() {
            // Only the badge list changes; the rest of the card stays byte-for-byte.
            if let Ok(mut card) = serde_json::from_value::<Card>(value.clone()) {
                self.mark(&mut card);
                value["badges"] = json!(card.badges);
            }
        }
    }
}

/// Stored cards are not a place for another profile's (or a stale) badge.
pub(crate) fn strip(card: &mut Card) {
    card.badges.retain(|badge| !badge.eq_ignore_ascii_case(BADGE));
}

pub(crate) fn selected_profile(lease: &ResourceLease) -> Option<i64> {
    let auth::Principal::Account { profile_id, .. } = &lease.principal;
    *profile_id
}
