//! Portable shell lifetime decisions. Adapters own jobs, clocks, effects and route/focus mechanics.
use crate::{CoreError, Identity};
use facet::Facet;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct ForegroundAuthorityInput {
    pub expected: Identity,
    pub current: Identity,
    pub profile_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Facet)]
#[repr(C)]
pub enum ForegroundAuthorityDecision {
    Valid,
    Revoked,
    ProfileUnavailable,
}

pub fn foreground_authority(v: &ForegroundAuthorityInput) -> ForegroundAuthorityDecision {
    use ForegroundAuthorityDecision::*;
    if v.current.account.id != v.expected.account.id {
        return Revoked;
    }
    let prior = v
        .expected
        .profiles
        .iter()
        .find(|p| Some(&p.id) == v.profile_id.as_ref());
    let current = v
        .current
        .profiles
        .iter()
        .find(|p| Some(&p.id) == v.profile_id.as_ref());
    if v.current.account.role != v.expected.account.role
        || v.current.restricted != v.expected.restricted
        || v.current.profile_setup_required != v.expected.profile_setup_required
        || (v.profile_id.is_some()
            && (current.is_none()
                || v.current.profile_id != v.profile_id
                || current.is_some_and(|p| p.kid == Some(true))
                    != prior.is_some_and(|p| p.kid == Some(true))
                || current.is_some_and(|p| p.setup_complete == Some(true))
                    != prior.is_some_and(|p| p.setup_complete == Some(true))))
    {
        return ProfileUnavailable;
    }
    Valid
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct HomeRevisionInput {
    pub scope_valid: bool,
    pub observed_revision: Option<String>,
    /// Read after the adapter has joined any active load. Only successful loads acknowledge revisions.
    pub rendered_revision: Option<String>,
    /// Absent before refresh, otherwise the refresh effect's success fact.
    pub refresh_succeeded: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Facet)]
#[repr(C)]
pub enum HomeRevisionDecision {
    Unchanged,
    Refresh,
    Refreshed,
    RetryLater,
    Unsupported,
    ScopeLost,
}

pub fn home_revision(v: &HomeRevisionInput) -> HomeRevisionDecision {
    use HomeRevisionDecision::*;
    // A failed refresh remains retryable even if its owner disappeared during the effect.
    if v.refresh_succeeded == Some(false) {
        return RetryLater;
    }
    if !v.scope_valid {
        return ScopeLost;
    }
    if v.refresh_succeeded == Some(true) {
        return Refreshed;
    }
    match &v.observed_revision {
        None => Unsupported,
        Some(observed) if Some(observed) == v.rendered_revision.as_ref() => Unchanged,
        Some(_) => Refresh,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Facet)]
#[repr(C)]
pub enum PreviewRoute {
    Details,
    Sources,
    Player,
    Other,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct PreviewScopeInput {
    pub profile_id: Option<String>,
    pub media_type: String,
    pub media_id: String,
    pub has_episode: bool,
    pub active_key: Option<String>,
    pub route: PreviewRoute,
    /// Title disposal retains only its own picker/player, not another Title frame.
    pub releasing: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct PreviewScopeDecision {
    pub key: Option<String>,
    pub keep: bool,
}

pub fn preview_scope(v: &PreviewScopeInput) -> PreviewScopeDecision {
    let key = v
        .profile_id
        .as_ref()
        .filter(|_| v.media_type != "live" && (v.media_type != "series" || v.has_episode))
        .map(|profile| format!("{profile}\0{}\0{}", v.media_type, v.media_id));
    let profile_matches = v
        .active_key
        .as_ref()
        .zip(v.profile_id.as_ref())
        .is_some_and(|(key, profile)| key.starts_with(&format!("{profile}\0")));
    let keep = profile_matches
        && match v.route {
            // The native exact-target effect replaces a changed Details target after rendering.
            PreviewRoute::Details => !v.releasing,
            PreviewRoute::Sources | PreviewRoute::Player => {
                v.active_key.is_some() && v.active_key == key
            }
            PreviewRoute::Other => false,
        };
    PreviewScopeDecision { key, keep }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Facet)]
#[repr(C)]
pub enum PreviewAction {
    Start,
    Adopt,
    Update,
    Result,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct PreviewInput {
    pub action: PreviewAction,
    pub requested_key: String,
    pub active_key: Option<String>,
    pub running: bool,
    pub has_sources: bool,
    pub done: bool,
    pub failed: bool,
    /// Entry identity is a native ownership fact, not merely equality of target keys.
    pub owner_matches: bool,
    /// Monotonic age and reuse budget supplied by the shell. Omitted by legacy shells.
    #[serde(default)]
    pub elapsed_millis: Option<i64>,
    #[serde(default)]
    pub reuse_budget_millis: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Facet)]
#[repr(C)]
pub enum PreviewDecision {
    Retain,
    BeginSettled,
    BeginImmediate,
    Accept,
    Reject,
    Cancelled,
    Failed,
    Ready,
}

pub fn preview(v: &PreviewInput) -> PreviewDecision {
    use PreviewDecision::*;
    let same_key = v.active_key.as_ref() == Some(&v.requested_key);
    let fresh = match (v.elapsed_millis, v.reuse_budget_millis) {
        (Some(age), Some(budget)) => age >= 0 && age < budget,
        _ => true,
    };
    match v.action {
        PreviewAction::Start => {
            if same_key && fresh {
                Retain
            } else {
                BeginSettled
            }
        }
        PreviewAction::Adopt => {
            if same_key && fresh && (v.running || v.has_sources) {
                Retain
            } else {
                BeginImmediate
            }
        }
        PreviewAction::Update => {
            if v.owner_matches {
                Accept
            } else {
                Reject
            }
        }
        PreviewAction::Result => {
            if !v.done {
                Cancelled
            } else if v.failed && !v.has_sources {
                Failed
            } else {
                Ready
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct UpNextPlaybackInput {
    pub media_key: String,
    pub previous_key: Option<String>,
    pub attempted_key: Option<String>,
    pub resume_awaiting_key: Option<String>,
    pub explicit_resume: bool,
    pub position_millis: i64,
    pub duration_millis: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct UpNextPlaybackDecision {
    pub attempted_key: Option<String>,
    pub resume_awaiting_key: Option<String>,
}

pub fn up_next_playback(v: &UpNextPlaybackInput) -> UpNextPlaybackDecision {
    UpNextPlaybackDecision {
        attempted_key: if v.previous_key.as_ref() != Some(&v.media_key) {
            None
        } else {
            v.attempted_key.clone()
        },
        resume_awaiting_key: if v.explicit_resume
            && v.duration_millis
                .is_some_and(|duration| v.position_millis >= duration.saturating_sub(10_000))
        {
            Some(v.media_key.clone())
        } else {
            v.resume_awaiting_key.clone()
        },
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct UpNextGateInput {
    pub media_key: String,
    pub attempted_key: Option<String>,
    pub resume_awaiting_key: Option<String>,
    pub ended: bool,
    pub eligible: bool,
    pub continuation_busy: bool,
    pub blocked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct UpNextGateDecision {
    pub attempted_key: Option<String>,
    pub resume_awaiting_key: Option<String>,
    pub start: bool,
}

pub fn up_next_gate(v: &UpNextGateInput) -> UpNextGateDecision {
    let awaiting = v.resume_awaiting_key.as_ref() == Some(&v.media_key);
    let suppressed = v.blocked || (awaiting && !v.ended);
    let start = !suppressed
        && v.eligible
        && v.attempted_key.as_ref() != Some(&v.media_key)
        && !v.continuation_busy;
    UpNextGateDecision {
        attempted_key: if start {
            Some(v.media_key.clone())
        } else {
            v.attempted_key.clone()
        },
        resume_awaiting_key: if !v.blocked && awaiting && v.ended {
            None
        } else {
            v.resume_awaiting_key.clone()
        },
        start,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Facet)]
#[repr(C)]
pub enum CountdownAction {
    Begin,
    Advance,
    Cancel,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct CountdownInput {
    pub action: CountdownAction,
    pub remaining_millis: i64,
    pub active: bool,
    pub elapsed_millis: i64,
    pub progressing: bool,
    pub scope_matches: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct CountdownDecision {
    pub remaining_millis: i64,
    pub seconds: i64,
    pub active: bool,
    pub done: bool,
}

pub const NEXT_COUNTDOWN_MILLIS: i64 = 10_000;

pub fn countdown(v: &CountdownInput) -> CountdownDecision {
    let mut remaining = v.remaining_millis.max(0);
    let mut active = v.active;
    if v.scope_matches {
        match v.action {
            CountdownAction::Begin => {
                remaining = NEXT_COUNTDOWN_MILLIS;
                active = true;
            }
            CountdownAction::Cancel => active = false,
            CountdownAction::Advance if active && v.progressing => {
                remaining = remaining.saturating_sub(v.elapsed_millis.max(0)).max(0);
            }
            CountdownAction::Advance => {}
        }
    }
    CountdownDecision {
        remaining_millis: remaining,
        seconds: remaining / 1000 + i64::from(remaining % 1000 != 0),
        active,
        done: v.scope_matches && active && remaining == 0,
    }
}

/// One JSON normalizer with operation-specific generated input/output contracts.
pub fn shell_lifecycle(v: &Value) -> Result<Value, CoreError> {
    fn run<I: for<'de> Deserialize<'de>, O: Serialize>(
        v: &Value,
        f: fn(&I) -> O,
    ) -> Result<Value, CoreError> {
        let input = serde_json::from_value(v.clone()).map_err(|_| CoreError::InvalidInput)?;
        serde_json::to_value(f(&input)).map_err(|_| CoreError::InvalidInput)
    }
    match v["operation"].as_str() {
        Some("foregroundAuthority") => run(v, foreground_authority),
        Some("homeRevision") => run(v, home_revision),
        Some("previewScope") => run(v, preview_scope),
        Some("preview") => run(v, preview),
        Some("upNextPlayback") => run(v, up_next_playback),
        Some("upNextGate") => run(v, up_next_gate),
        Some("countdown") => run(v, countdown),
        _ => Err(CoreError::InvalidInput),
    }
}
