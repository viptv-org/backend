//! Typed viewing operations over existing profile-owned state; no second queue/history.
use super::*;
use viptv_engine::{continuation::*, identity::MediaIdentity, presentation::Card};
mod metadata;
mod next;
mod page;
use release_millis as clock;
mod visibility;
pub(crate) use next::resolve;
pub(crate) use page::{get, snapshot};
pub(crate) use visibility::update;
#[cfg(test)]
mod tests;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Page {
    #[serde(default)]
    offset: u32,
    #[serde(default = "limit")]
    limit: u32,
}
fn limit() -> u32 {
    40
}
fn validate(identity: &MediaIdentity, parent: Option<&MediaIdentity>) -> Result<(), ApiError> {
    viptv_engine::continuation::validate_identity(identity)
        .map_err(|_| ApiError::from("Invalid continuation identity"))?;
    if let Some(parent) = parent {
        viptv_engine::continuation::validate_identity(parent)
            .map_err(|_| ApiError::from("Invalid parent identity"))?;
        if identity.kind != "series"
            || identity.episode.is_none()
            || parent.kind != "series"
            || parent.episode.is_some()
            || parent.id == identity.id
        {
            return Err("Invalid continuation parent".into());
        }
    }
    Ok(())
}
fn state(value: &Value, record: &viptv_engine::playback::PlaybackRecord) -> ContinuationStatus {
    match value["queue_status"].as_str() {
        Some("next") => ContinuationStatus::Next,
        Some("caught_up") => ContinuationStatus::CaughtUp,
        Some("upcoming") => ContinuationStatus::Upcoming,
        Some("unavailable") => ContinuationStatus::Unavailable,
        Some("pending") => ContinuationStatus::Pending,
        _ if record.completed_at_millis.is_some() && record.identity.episode.is_some() => {
            ContinuationStatus::Pending
        }
        _ => ContinuationStatus::Resume,
    }
}
pub(crate) fn release_millis(value: &Value) -> Option<u64> {
    let s = value.as_str()?;
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .and_then(|d| u64::try_from(d.timestamp_millis()).ok())
        .or_else(|| {
            chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .ok()?
                .and_hms_opt(0, 0, 0)
                .and_then(|d| u64::try_from(d.and_utc().timestamp_millis()).ok())
        })
}

fn revision_db(db: &Connection, profile: i64, expected: u64) -> Result<(), ApiError> {
    let current: u64 = db
        .query_row(
            "SELECT revision FROM engine_profile_configuration WHERE profile_id=?1",
            [profile],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?
        .unwrap_or(0);
    if current != expected {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Profile configuration changed".into(),
        ));
    }
    Ok(())
}

fn configuration_revision(db: &Connection, profile: i64) -> Result<u64, ApiError> {
    db.query_row(
        "SELECT revision FROM engine_profile_configuration WHERE profile_id=?1",
        [profile],
        |r| r.get(0),
    )
    .optional()
    .map_err(db_error)
    .map(|value| value.unwrap_or(0))
}
