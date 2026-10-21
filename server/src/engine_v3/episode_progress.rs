//! The selected profile's own watch state on a Details episode window.
//!
//! Episode cards come from a provider document; whether the viewer finished an
//! episode or is part-way through it is private profile state. Each response
//! reads it from the two stores playback writes, by the same keys:
//! origin-scoped playback records, and legacy progress rows (which also hold
//! Stremio/Simkl imports and "Mark watched" corrections).
//!
//! - Part-way (resumable) → `progress` is the fraction; no `Watched` badge.
//! - Completed → a `Watched` badge and no partial bar. A completion-only import
//!   has no clock, so the badge is its only trace; no duration is invented.
//! - Otherwise neither. Failed reads leave the card unmarked; they never fail
//!   the page.
use super::*;
use viptv_engine::{playback::PlaybackRecord, presentation::Card};

// The same badge `watched` puts on finished titles.
use super::watched::BADGE as WATCHED;

struct State {
    watched: bool,
    progress: Option<f64>,
    updated_millis: u64,
}

/// Mark the `episodes` of a serialized `DetailSnapshot` in place.
pub(super) fn mark(app: &App, profile: i64, detail: &mut Value) {
    let Some(episodes) = detail["episodes"].as_array_mut() else {
        return;
    };
    let Ok(db) = app.db.lock() else {
        return;
    };
    for slot in episodes {
        let Ok(mut card) = serde_json::from_value::<Card>(slot.clone()) else {
            continue;
        };
        // Never trust a provider/cached copy of private state.
        card.badges.retain(|badge| badge != WATCHED);
        card.progress = None;
        if card.identity.kind == "series" && card.identity.episode.is_some() {
            if let Some(state) = state(&db, profile, &card) {
                if state.progress.is_some() {
                    card.progress = state.progress;
                } else if state.watched {
                    card.badges.push(WATCHED.into());
                }
            }
        }
        if let Ok(value) = serde_json::to_value(card) {
            *slot = value;
        }
    }
}

/// The newest of the episode's scoped record and legacy row; scoped wins a tie
/// (the History merge rule).
fn state(db: &Connection, profile: i64, card: &Card) -> Option<State> {
    let mut newest: Option<State> = None;
    let mut legacy_ids = Vec::new();
    for key in scoped_library::playback_keys_db(db, profile, card) {
        if key.0 == "shared" || key.0 == "legacy" {
            legacy_ids.push(key.2.clone());
        }
        if newest.is_none() {
            newest = scoped(db, profile, &key);
        }
    }
    legacy_ids.dedup();
    for id in legacy_ids {
        if let Some(row) = legacy(db, profile, &id) {
            if newest
                .as_ref()
                .is_none_or(|scoped| row.updated_millis > scoped.updated_millis)
            {
                newest = Some(row);
            }
        }
    }
    newest
}

fn scoped(db: &Connection, profile: i64, key: &scoped_library::Key) -> Option<State> {
    let raw: String = db
        .query_row(
            "SELECT value FROM engine_scoped_records WHERE profile_id=?1 AND record_kind='playback' AND identity_scope=?2 AND kind=?3 AND identity_id=?4",
            params![profile, key.0, key.1, key.2],
            |r| r.get(0),
        )
        .ok()?;
    let record: PlaybackRecord = serde_json::from_str(&raw).ok()?;
    let resumable = record.can_resume();
    Some(State {
        watched: !resumable && record.completed_at_millis.is_some(),
        progress: resumable.then(|| record.progress()).flatten(),
        updated_millis: record.updated_at_millis,
    })
}

fn legacy(db: &Connection, profile: i64, id: &str) -> Option<State> {
    let (position, duration, updated, context): (f64, f64, i64, String) = db
        .query_row(
            "SELECT position,duration,updated_at,context FROM progress WHERE profile_id=?1 AND type='series' AND id=?2",
            params![profile, id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .ok()?;
    let context = serde_json::from_str::<Value>(&context).unwrap_or(Value::Null);
    let mut item = json!({"position":position,"duration":duration});
    crate::library::add_watch_fields(&mut item, &context);
    let resumable = item["resume_active"] == true;
    Some(State {
        watched: !resumable && item["watched"] == true,
        progress: resumable.then(|| (position / duration).clamp(0.0, 1.0)),
        updated_millis: u64::try_from(updated).unwrap_or(0).saturating_mul(1000),
    })
}
