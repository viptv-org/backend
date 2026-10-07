//! Direct Rust policy for backend/native callers; no auth, database or transport effects.
use crate::{
    CoreError,
    dto::{PlaybackConversion, PlaybackPlatform, PlaybackV2Request},
};
use std::fmt;

/// Both resource scope and authenticated session are identity, including idempotent starts.
#[derive(Clone, PartialEq, Eq)]
pub struct NativeTorrentScope {
    pub scope_key: String,
    pub session_id: String,
}
impl fmt::Debug for NativeTorrentScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeTorrentScope(<redacted>)")
    }
}
pub fn same_native_scope(a: &NativeTorrentScope, b: &NativeTorrentScope) -> bool {
    !a.scope_key.is_empty() && !a.session_id.is_empty() && a == b
}
#[derive(Clone)]
pub struct NativeTorrentRequestIdentity {
    pub scope: NativeTorrentScope,
    pub request: PlaybackV2Request,
}
impl fmt::Debug for NativeTorrentRequestIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeTorrentRequestIdentity(<redacted>)")
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeTorrentRequestDecision {
    New,
    Idempotent,
    Conflict,
    Cancelled,
    DifferentScope,
    Invalid,
}
/// Backend supplies a session-lifetime durable tombstone; no storage/eviction policy is invented here.
pub fn native_request_transition(
    existing: Option<&NativeTorrentRequestIdentity>,
    incoming: &NativeTorrentRequestIdentity,
    tombstoned: bool,
) -> NativeTorrentRequestDecision {
    use NativeTorrentRequestDecision::*;
    if crate::policy::validate_playback_v2(&incoming.request).is_err()
        || incoming.scope.scope_key.is_empty()
        || incoming.scope.session_id.is_empty()
    {
        return Invalid;
    }
    if tombstoned {
        return Cancelled;
    }
    match existing {
        None => New,
        Some(previous) if !same_native_scope(&previous.scope, &incoming.scope) => DifferentScope,
        Some(previous) if previous.request.request_id != incoming.request.request_id => New,
        Some(previous) if previous.request == incoming.request => Idempotent,
        _ => Conflict,
    }
}
/// Authoritative backend proof and measured qualification, not client-provided authorization claims.
pub struct NativeTorrentAdmissionFacts<'a> {
    pub caller_scope: &'a NativeTorrentScope,
    pub source_scope: &'a NativeTorrentScope,
    pub request_scope: &'a NativeTorrentScope,
    pub request: &'a PlaybackV2Request,
    pub backend_policy_enabled: bool,
    pub qualified: bool,
    pub negotiated: bool,
    pub resource_authorized: bool,
    pub source_proof_current: bool,
    pub exact_vod: bool,
    pub info_hash: Option<&'a str>,
    pub file_index: Option<u32>,
}
impl fmt::Debug for NativeTorrentAdmissionFacts<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeTorrentAdmissionFacts(<redacted>)")
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeTorrentAdmissionDecision {
    Native,
    Gateway,
    RefuseAuthorization,
    RefuseSelection,
    InvalidRequest,
}
pub fn canonical_v1_hash(hash: &str) -> bool {
    hash.len() == 40
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
}
pub fn native_request_eligible(request: &PlaybackV2Request) -> bool {
    !request.force_gateway
        && matches!(request.conversion, PlaybackConversion::Auto)
        && request.audio_track.is_none()
        && request.subtitle_track.is_none()
        && request.audio_language.is_none()
        && !request.subtitles_off
}
pub fn native_admission(f: &NativeTorrentAdmissionFacts<'_>) -> NativeTorrentAdmissionDecision {
    use NativeTorrentAdmissionDecision::*;
    if !f.resource_authorized
        || !same_native_scope(f.caller_scope, f.source_scope)
        || !same_native_scope(f.caller_scope, f.request_scope)
    {
        return RefuseAuthorization;
    }
    if !f.source_proof_current {
        return RefuseSelection;
    }
    if crate::policy::validate_playback_v2(f.request).is_err() {
        return InvalidRequest;
    }
    if !f.backend_policy_enabled
        || !f.qualified
        || !f.negotiated
        || !f.exact_vod
        || !matches!(
            f.request.client.platform,
            PlaybackPlatform::Android | PlaybackPlatform::AndroidTv
        )
        || f.request.client.native_torrent.is_none()
        || !native_request_eligible(f.request)
        || f.info_hash.is_none_or(|hash| !canonical_v1_hash(hash))
        || f.file_index.is_none_or(|index| index > 65535)
    {
        return Gateway;
    }
    Native
}
/// Same-grant poll/renewal comparison. Scope must include backend session identity.
pub fn validate_native_transition(
    previous: &crate::native_torrent::NativeTorrentGrant,
    next: &crate::native_torrent::NativeTorrentGrant,
    operation: crate::native_torrent::NativeTorrentControlOperation,
    previous_scope: &NativeTorrentScope,
    next_scope: &NativeTorrentScope,
) -> Result<(), CoreError> {
    if !same_native_scope(previous_scope, next_scope) {
        return Err(CoreError::InvalidInput);
    }
    validate_grant_transition(previous, next, operation)
}
/// Client authority scopes/generations are fenced separately from backend stable session identity.
pub fn validate_grant_transition(
    previous: &crate::native_torrent::NativeTorrentGrant,
    next: &crate::native_torrent::NativeTorrentGrant,
    operation: crate::native_torrent::NativeTorrentControlOperation,
) -> Result<(), CoreError> {
    use crate::native_torrent::NativeTorrentControlOperation;
    crate::native_torrent::validate_native_grant(previous, previous.expires_at)?;
    if !previous.same_identity(next)
        || next.server_time < previous.server_time
        || next.expires_at < previous.expires_at
        || (operation != NativeTorrentControlOperation::Heartbeat
            && next.expires_at != previous.expires_at)
    {
        return Err(CoreError::InvalidInput);
    }
    crate::native_torrent::validate_native_grant(next, next.expires_at)
}
