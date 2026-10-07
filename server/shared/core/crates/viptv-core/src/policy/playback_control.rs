//! Portable playback decisions. Shells own player effects, clocks, cancellation and session fencing.
//! Inputs contain transport-free facts; no source URL, header or capability token crosses this API.
use crate::CoreError;
use facet::Facet;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;

type Result<T> = std::result::Result<T, CoreError>;

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct PlaybackTimelineFacts {
    /// Delivery/timeline mode, not processing mode: gateway copy is still managed.
    pub delivery_mode: String,
    pub launch_position_millis: i64,
    pub segment_position_millis: i64,
    pub title_offset_millis: i64,
    pub title_position_millis: i64,
    pub native_duration_millis: Option<i64>,
    pub title_duration_millis: Option<i64>,
    pub pause_anchor_millis: Option<i64>,
    pub player_error: bool,
    pub trusted_position_millis: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct PlaybackTimelineProjection {
    pub launch_offset_millis: i64,
    pub position_millis: i64,
    pub segment_position_millis: i64,
    pub duration_millis: Option<i64>,
    /// An anchor/error fallback must not overwrite the shell's last trusted native observation.
    pub update_trusted_position: bool,
}

pub fn uses_managed_replacement(delivery_mode: &str) -> bool {
    !delivery_mode.eq_ignore_ascii_case("direct")
}

pub fn timeline(v: &PlaybackTimelineFacts) -> PlaybackTimelineProjection {
    let candidate = v
        .segment_position_millis
        .max(0)
        .saturating_add(v.title_offset_millis.max(0));
    let update_trusted_position =
        v.pause_anchor_millis.is_none() && !(v.player_error && v.trusted_position_millis > 0);
    PlaybackTimelineProjection {
        launch_offset_millis: if uses_managed_replacement(&v.delivery_mode) {
            v.launch_position_millis.max(0)
        } else {
            0
        },
        position_millis: v.pause_anchor_millis.unwrap_or({
            if v.player_error && v.trusted_position_millis > 0 {
                v.trusted_position_millis
            } else {
                candidate
            }
        }),
        segment_position_millis: v
            .title_position_millis
            .saturating_sub(v.title_offset_millis)
            .max(0),
        // Preserve the title-duration preference for managed segments, which may be shorter than the title.
        duration_millis: if v.delivery_mode == "direct" {
            v.native_duration_millis.or(v.title_duration_millis)
        } else {
            v.title_duration_millis.or(v.native_duration_millis)
        },
        update_trusted_position,
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct PlaybackSeekFacts {
    pub current_millis: i64,
    pub delta_millis: i64,
    pub duration_millis: Option<i64>,
    pub range_start_millis: Option<i64>,
    pub range_end_millis: Option<i64>,
}

pub fn seek_preview(v: &PlaybackSeekFacts) -> Option<i64> {
    let raw = v.current_millis.saturating_add(v.delta_millis);
    let (start, end) = match (v.duration_millis, v.range_start_millis, v.range_end_millis) {
        (Some(duration), _, _) if duration >= 0 => (0, duration),
        (None, Some(start), Some(end)) if start <= end => (start, end),
        _ => return None,
    };
    let target = raw.clamp(start, end);
    (target.abs_diff(v.current_millis) >= 500).then_some(target)
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct PlaybackPauseFacts {
    pub delivery_mode: String,
    pub live: bool,
    pub anchor_millis: Option<i64>,
    pub launch_position_millis: i64,
    pub play_when_ready: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct PlaybackPauseDecision {
    pub uses_anchor: bool,
    pub replace_on_resume: bool,
    pub anchor_after_open_millis: Option<i64>,
}

pub fn pause(v: &PlaybackPauseFacts) -> PlaybackPauseDecision {
    let uses_anchor = !v.live && uses_managed_replacement(&v.delivery_mode);
    PlaybackPauseDecision {
        uses_anchor,
        replace_on_resume: uses_anchor && v.anchor_millis.is_some(),
        anchor_after_open_millis: (uses_anchor && !v.play_when_ready)
            .then_some(v.launch_position_millis),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct PlaybackRecoveryFacts {
    pub server_managed: bool,
    pub network_failure: bool,
    pub already_attempted: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct PlaybackDeliveryFacts {
    pub direct_delivery: bool,
    pub can_play_direct: bool,
    pub force_gateway: bool,
    pub automatic_conversion: bool,
}

pub fn delivery_compatible(v: &PlaybackDeliveryFacts) -> bool {
    !v.direct_delivery || (v.can_play_direct && !v.force_gateway && v.automatic_conversion)
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct PlaybackLeaseFacts {
    pub expected_id: String,
    pub actual_id: String,
    pub status: String,
    pub has_session: bool,
    pub expires_at_millis: f64,
    pub now_millis: i64,
    /// Shell compares sensitive transport values without serializing them here.
    pub heartbeat: bool,
    pub same_delivery_url: bool,
    pub same_delivery_kind: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Facet)]
#[serde(rename_all = "snake_case")]
#[facet(rename_all = "snake_case")]
#[repr(C)]
pub enum PlaybackLeaseDecision {
    Invalid,
    Terminal,
    Expired,
    Pending,
    Ready,
}

pub fn lease(v: &PlaybackLeaseFacts) -> PlaybackLeaseDecision {
    if v.expected_id != v.actual_id {
        return PlaybackLeaseDecision::Invalid;
    }
    if matches!(v.status.as_str(), "failed" | "expired" | "released") {
        return PlaybackLeaseDecision::Terminal;
    }
    if v.expires_at_millis <= v.now_millis as f64 {
        return PlaybackLeaseDecision::Expired;
    }
    let ready = v.status == "ready" && v.has_session;
    if v.heartbeat && (!ready || !v.same_delivery_url || !v.same_delivery_kind) {
        return PlaybackLeaseDecision::Invalid;
    }
    if ready {
        PlaybackLeaseDecision::Ready
    } else {
        PlaybackLeaseDecision::Pending
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct PlaybackAuthorityFacts {
    pub expires_at_millis: i64,
    pub now_millis: i64,
    pub elapsed_millis: i64,
    /// Client-specific observation limit, not a protocol lease lifetime.
    pub observation_cap_millis: i64,
    pub wait_millis: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct PlaybackAuthorityBudget {
    pub remaining_millis: i64,
    pub delay_millis: i64,
}

pub fn authority(v: &PlaybackAuthorityFacts) -> PlaybackAuthorityBudget {
    let remaining = v
        .expires_at_millis
        .saturating_sub(v.now_millis)
        .min(v.observation_cap_millis.saturating_sub(v.elapsed_millis))
        .max(0);
    PlaybackAuthorityBudget {
        remaining_millis: remaining,
        delay_millis: v.wait_millis.min(remaining),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct PlaybackFailureFacts {
    pub gateway_error: bool,
    pub status: i32,
    pub invalid_response: bool,
    pub io_error: bool,
    pub has_lease_id: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct PlaybackFailureDecision {
    pub retry_renewal: bool,
    /// Reconcile the exact request and release it; never select a different delivery/source.
    pub reconcile_and_release: bool,
}

pub fn failure(v: &PlaybackFailureFacts) -> PlaybackFailureDecision {
    let refused = v.gateway_error && (400..=499).contains(&v.status) && v.status != 408;
    let terminal = if v.gateway_error {
        ((1..=499).contains(&v.status) && v.status != 408) || v.invalid_response
    } else {
        !v.io_error
    };
    PlaybackFailureDecision {
        retry_renewal: !terminal,
        reconcile_and_release: v.has_lease_id || !refused,
    }
}

/// Validation facts for a normalized page and the exact request/snapshot being extended.
/// A shell retains its own viewport budget and fences request/profile revisions before adoption.
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct LivePageValidationFacts {
    pub catalog_id: Option<String>,
    pub generation: Option<String>,
    pub ids: Vec<String>,
    pub names: Vec<String>,
    pub categories: bool,
    pub next_cursor: Option<String>,
    pub previous_cursor: Option<String>,
    pub requested_catalog_id: Option<String>,
    pub limit: u32,
    pub check_snapshot: bool,
    pub snapshot_catalog_id: Option<String>,
    pub snapshot_generation: Option<String>,
    pub known_ids: Vec<String>,
    pub cursor: Option<String>,
    pub previous: bool,
    pub extending_window: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Facet)]
#[serde(rename_all = "snake_case")]
#[facet(rename_all = "snake_case")]
#[repr(C)]
pub enum LivePageValidationDecision {
    Valid,
    CatalogChanged,
    Invalid,
}

pub fn live_page(v: &LivePageValidationFacts) -> LivePageValidationDecision {
    if v.check_snapshot
        && (v.catalog_id != v.snapshot_catalog_id || v.generation != v.snapshot_generation)
        || v.extending_window && v.ids.is_empty()
    {
        return LivePageValidationDecision::CatalogChanged;
    }
    let known: HashSet<_> = v.known_ids.iter().collect();
    let mut ids = HashSet::new();
    let paged = v.cursor.is_some() || v.extending_window;
    let continuing = if v.previous {
        &v.previous_cursor
    } else {
        &v.next_cursor
    };
    if v.ids.len() > v.limit as usize
        || v.requested_catalog_id.is_some() && v.requested_catalog_id != v.catalog_id
        || v.ids.len() != v.names.len()
        || v.ids.iter().zip(&v.names).any(|(id, name)| {
            v.categories && (id.trim().is_empty() || name.trim().is_empty())
                || !ids.insert(id)
                || paged && known.contains(id)
        })
        || v.ids.is_empty()
            && (v.categories && (v.next_cursor.is_some() || v.previous_cursor.is_some())
                || v.cursor.is_some())
        || v.cursor.is_some() && continuing == &v.cursor
    {
        return LivePageValidationDecision::Invalid;
    }
    LivePageValidationDecision::Valid
}

fn decode<T: serde::de::DeserializeOwned>(v: &Value) -> Result<T> {
    serde_json::from_value(v.clone()).map_err(|_| CoreError::InvalidInput)
}
fn encode(v: impl Serialize) -> Result<Value> {
    serde_json::to_value(v).map_err(|_| CoreError::InvalidInput)
}

/// Register as the `playbackControl` normalizer. All clocks/configuration are caller facts.
pub fn normalize(v: &Value) -> Result<Value> {
    match v["operation"].as_str().unwrap_or("") {
        "timeline" => encode(timeline(&decode(v)?)),
        "seekPreview" => encode(seek_preview(&decode(v)?)),
        "seekCommit" => Ok(json!(uses_managed_replacement(
            v["deliveryMode"].as_str().ok_or(CoreError::InvalidInput)?
        ))),
        "pause" => encode(pause(&decode(v)?)),
        "recovery" => {
            let v: PlaybackRecoveryFacts = decode(v)?;
            Ok(json!(
                v.server_managed && v.network_failure && !v.already_attempted
            ))
        }
        "delivery" => Ok(json!(delivery_compatible(&decode(v)?))),
        "lease" => encode(lease(&decode(v)?)),
        "authority" => encode(authority(&decode(v)?)),
        "failure" => encode(failure(&decode(v)?)),
        "livePage" => encode(live_page(&decode(v)?)),
        _ => Err(CoreError::InvalidInput),
    }
}
