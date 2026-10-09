//! Native transport v2 descriptors; private authority never enters ordinary DTOs.
use crate::native_torrent::{
    NativeTorrentControlOperation, NativeTorrentInput, NativeTorrentPreferences,
};
use crate::{
    CoreError,
    dto::{PlaybackPlatform, PlaybackV2Request},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::json;
use std::fmt;

pub const NETWORK_POLICY: &str = "public_discovery_verified_v2";
const MAX_SAFE: u64 = 9_007_199_254_740_991;

#[derive(Clone)]
pub struct TorrentRuntimeGrant {
    pub id: String,
    pub server_time: u64,
    pub expires_at: u64,
    pub info_hash: String,
    pub file_index: Option<u32>,
    pub archive_index: Option<u32>,
    pub input: NativeTorrentInput,
    pub trackers: Vec<String>,
    pub expected_file_size: Option<u64>,
}
impl fmt::Debug for TorrentRuntimeGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TorrentRuntimeGrant(<redacted>)")
    }
}
impl TorrentRuntimeGrant {
    pub fn same_identity(&self, other: &Self) -> bool {
        self.id == other.id
            && self.info_hash == other.info_hash
            && self.file_index == other.file_index
            && self.archive_index == other.archive_index
            && self.input == other.input
            && self.trackers == other.trackers
            && self.expected_file_size == other.expected_file_size
    }
}

pub fn native_platform(platform: &PlaybackPlatform) -> bool {
    matches!(
        platform,
        PlaybackPlatform::Android | PlaybackPlatform::AndroidTv | PlaybackPlatform::Desktop
    )
}

pub fn request_eligible(request: &PlaybackV2Request) -> bool {
    crate::policy::validate_playback_v2(request).is_ok()
        && native_platform(&request.client.platform)
        && request
            .client
            .native_torrent
            .as_ref()
            .is_some_and(|cap| cap.version == 2 && cap.network_policy == NETWORK_POLICY)
        && crate::native_torrent_policy::native_request_eligible(request)
}

pub fn validate_grant(grant: &TorrentRuntimeGrant, envelope_expiry: u64) -> Result<(), CoreError> {
    if !crate::domain::playback_protocol::valid_identifier(&grant.id)
        || !crate::native_torrent_policy::canonical_v1_hash(&grant.info_hash)
        || grant.server_time > MAX_SAFE / 1000
        || grant.expires_at > MAX_SAFE / 1000
        || grant.expires_at != envelope_expiry
        || grant
            .expires_at
            .checked_sub(grant.server_time)
            .is_none_or(|n| n == 0 || n > 60)
        || grant.file_index.is_some_and(|n| n > 65535)
        || grant.archive_index.is_some_and(|n| n > 65535)
        || grant
            .expected_file_size
            .is_some_and(|n| n == 0 || n > MAX_SAFE)
        || (grant.file_index.is_none() && grant.expected_file_size.is_some())
        || grant.trackers.len() > 32
    {
        return Err(CoreError::InvalidInput);
    }
    for tracker in &grant.trackers {
        let url = url::Url::parse(tracker).map_err(|_| CoreError::InvalidInput)?;
        if tracker.len() > 2048
            || !matches!(url.scheme(), "http" | "https" | "udp")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || url.port() == Some(0)
        {
            return Err(CoreError::InvalidInput);
        }
    }
    match grant.input.kind.as_str() {
        "magnet" if grant.input.value == format!("magnet:?xt=urn:btih:{}", grant.info_hash) => {
            Ok(())
        }
        "metainfo" => {
            if grant.input.value.len() > 5_592_408 {
                return Err(CoreError::InvalidInput);
            }
            let bytes = STANDARD
                .decode(&grant.input.value)
                .map_err(|_| CoreError::InvalidInput)?;
            if bytes.len() > 4 * 1024 * 1024 || STANDARD.encode(&bytes) != grant.input.value {
                return Err(CoreError::InvalidInput);
            }
            crate::native_metainfo::validate_runtime(
                &bytes,
                &grant.info_hash,
                grant.file_index,
                grant.expected_file_size,
            )
        }
        _ => Err(CoreError::InvalidInput),
    }
}

pub fn validate_transition(
    previous: &TorrentRuntimeGrant,
    next: &TorrentRuntimeGrant,
    operation: NativeTorrentControlOperation,
) -> Result<(), CoreError> {
    validate_grant(next, next.expires_at)?;
    if !previous.same_identity(next)
        || next.server_time < previous.server_time
        || (operation != NativeTorrentControlOperation::Heartbeat
            && next.expires_at != previous.expires_at)
        || (operation == NativeTorrentControlOperation::Heartbeat
            && next.expires_at < previous.expires_at)
    {
        return Err(CoreError::InvalidInput);
    }
    Ok(())
}

pub fn ready_response(
    id: &str,
    position: f64,
    preferences: &NativeTorrentPreferences,
    grant: &TorrentRuntimeGrant,
) -> Result<String, CoreError> {
    validate_grant(grant, grant.expires_at)?;
    if !crate::domain::playback_protocol::valid_identifier(id)
        || !position.is_finite()
        || !(0.0..=604800.0).contains(&position)
    {
        return Err(CoreError::InvalidInput);
    }
    for language in [&preferences.audio_language, &preferences.subtitle_language]
        .into_iter()
        .flatten()
    {
        if language.is_empty()
            || language.len() > 35
            || !language
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
            || language == &grant.id
            || language.contains(&grant.info_hash)
            || language.contains(&grant.input.value)
        {
            return Err(CoreError::InvalidInput);
        }
    }
    let input = match grant.input.kind.as_str() {
        "magnet" => json!({"kind":"magnet","uri":grant.input.value}),
        "metainfo" => json!({"kind":"metainfo","metainfo_base64":grant.input.value}),
        _ => return Err(CoreError::InvalidInput),
    };
    let mut wire = json!({"version":2,"network_policy":NETWORK_POLICY,"id":grant.id,
        "server_time":grant.server_time,"expires_at":grant.expires_at,"info_hash":grant.info_hash,
        "file_index":grant.file_index,"archive_index":grant.archive_index,"trackers":grant.trackers,"input":input});
    if let Some(size) = grant.expected_file_size {
        wire["expected_file_size"] = json!(size)
    }
    serde_json::to_string(&json!({"id":id,"status":"ready","expires_at":grant.expires_at,"renew_after_seconds":20,
        "delivery":{"kind":"native_torrent","live":false,"format":"original","position":position,
            "preferences":{"audio_language":preferences.audio_language,"subtitle_language":preferences.subtitle_language,"subtitles_enabled":preferences.subtitles_enabled},"grant":wire},
        "error_code":null,"error":null})).map_err(|_|CoreError::InvalidInput)
}

/// V2 uses fresh native authority on retry; it never requests a gateway fallback.
pub(crate) fn decision(input: &str) -> Result<String, CoreError> {
    use crate::native_torrent::{
        NativeTorrentNegotiationDecision, NativeTorrentNegotiationFacts,
        NativeTorrentRecoveryDecision, NativeTorrentRecoveryFacts,
    };
    #[derive(serde::Deserialize)]
    #[serde(tag = "operation", deny_unknown_fields)]
    enum Operation {
        #[serde(rename = "negotiation", rename_all = "camelCase")]
        Negotiation {
            platform: PlaybackPlatform,
            qualified: bool,
            scope_matches: bool,
            status: Option<u16>,
            authorization_refused: bool,
            body: String,
        },
        #[serde(rename = "recovery")]
        Recovery { facts: NativeTorrentRecoveryFacts },
        #[serde(rename = "stage")]
        Stage { stage: String },
        #[serde(rename = "failure")]
        Failure { stage: String, reason: String },
    }
    let op: Operation = serde_json::from_str(input).map_err(|_| CoreError::InvalidInput)?;
    match op {
        Operation::Stage { stage } => {
            let text = match stage.as_str() {
                "finding_peers" => "Finding peers…",
                "fetching_metadata" => "Fetching metadata…",
                "opening_archive" => "Opening archive…",
                "buffering" | "playing" => "Buffering…",
                _ => return Err(CoreError::InvalidInput),
            };
            serde_json::to_string(text).map_err(|_| CoreError::InvalidInput)
        }
        Operation::Failure { stage, reason } => {
            let timeout = match stage.as_str() {
                "finding_peers" => "native_no_peers",
                "fetching_metadata" => "native_metadata_timeout",
                "opening_archive" => "native_archive_timeout",
                "buffering" | "playing" => "native_buffering_timeout",
                _ => return Err(CoreError::InvalidInput),
            };
            let code = match reason.as_str() {
                "startup_stalled" | "startup_deadline" => timeout,
                "authority_expired" => "native_authorization_expired",
                "cache_unavailable" | "metadata_cache_unavailable" => "native_cache_unavailable",
                "invalid_metadata" | "private_torrent_unsupported" => "native_metadata_invalid",
                "file_index_unavailable"
                | "empty_file"
                | "invalid_archive_selection"
                | "archive_index_unavailable" => "native_file_unavailable",
                "archive_volume_missing" => "native_archive_missing",
                "compressed_archive_unsupported" => "native_archive_compressed",
                "encrypted_archive_unsupported" => "native_archive_encrypted",
                "archive_metadata_invalid"
                | "archive_member_unsupported"
                | "invalid_archive_volume" => "native_archive_invalid",
                "clock_unavailable" => "native_authorization_expired",
                _ => return Err(CoreError::InvalidInput),
            };
            let message =
                crate::domain::native_failure_display(code).ok_or(CoreError::InvalidInput)?;
            Ok(json!({"code":code,"message":message}).to_string())
        }
        Operation::Negotiation {
            platform,
            qualified,
            scope_matches,
            status,
            authorization_refused,
            body,
        } => {
            let f = NativeTorrentNegotiationFacts {
                platform,
                qualified,
                scope_matches,
                status,
                authorization_refused,
                body,
            };
            use NativeTorrentNegotiationDecision::*;
            let decision = if !f.scope_matches {
                RejectStale
            } else if f.authorization_refused || matches!(f.status, Some(401 | 403)) {
                AuthRecovery
            } else if f.qualified
                && native_platform(&f.platform)
                && f.status == Some(200)
                && crate::domain::playback_protocol::parse_runtime(&f.body)
                    .is_ok_and(|protocol| protocol.native_torrent_versions == [2])
            {
                Advertise
            } else {
                Legacy
            };
            serde_json::to_string(&decision).map_err(|_| CoreError::InvalidInput)
        }
        Operation::Recovery { facts } => {
            let decision = match crate::native_torrent::recovery_decision(&facts) {
                NativeTorrentRecoveryDecision::ForceGatewayRetry => {
                    NativeTorrentRecoveryDecision::OrdinaryRetry
                }
                other => other,
            };
            serde_json::to_string(&decision).map_err(|_| CoreError::InvalidInput)
        }
    }
}
