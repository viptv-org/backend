//! Private transient native transport authority. No grant enters serializable app models.
use crate::{
    CoreError,
    dto::{PlaybackPlatform, PlaybackV2Request},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use facet::Facet;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json, value::RawValue};
use std::{fmt, sync::Mutex};

type Result<T> = std::result::Result<T, CoreError>;
const MAX_BODY: usize = 6 * 1024 * 1024;
const MAX_META: usize = 4 * 1024 * 1024;
const MAX_ENCODED: usize = 5_592_408;
const MAX_SECONDS: u64 = 9_007_199_254_740;
const MAX_SAFE: u64 = 9_007_199_254_740_991;
const POLICY: &str = "public_dht_tcp_v1";
fn invalid() -> CoreError {
    CoreError::InvalidInput
}
fn parse<T: serde::de::DeserializeOwned>(input: &str) -> Result<T> {
    serde_json::from_str(input).map_err(|_| invalid())
}
fn identifier(s: &str) -> bool {
    crate::domain::playback_protocol::valid_identifier(s)
}
fn language(s: &str) -> bool {
    !s.is_empty() && s.len() <= 35 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
fn android(platform: &PlaybackPlatform) -> bool {
    matches!(
        platform,
        PlaybackPlatform::Android | PlaybackPlatform::AndroidTv
    )
}

/// A private wire object is never sanitized into ordinary presentation data:
/// rejecting it also prevents reflected copies under otherwise harmless keys.
pub(crate) fn contains_private_transport(value: &Value) -> bool {
    match value {
        Value::Object(fields) => fields.iter().any(|(key, value)| {
            let key = key.to_ascii_lowercase().replace(['_', '-'], "");
            matches!(
                key.as_str(),
                "grant" | "infohash" | "magnet" | "metainfobase64" | "nativetorrent"
            ) || contains_private_transport(value)
        }),
        Value::Array(values) => values.iter().any(contains_private_transport),
        Value::String(value) => value.to_ascii_lowercase().contains("magnet:?"),
        _ => false,
    }
}

#[derive(Clone, Deserialize, Serialize, Facet)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[facet(rename_all = "camelCase")]
pub struct NativeTorrentNegotiationFacts {
    pub platform: PlaybackPlatform,
    pub qualified: bool,
    pub scope_matches: bool,
    pub status: Option<u16>,
    pub authorization_refused: bool,
    pub body: String,
}
impl fmt::Debug for NativeTorrentNegotiationFacts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeTorrentNegotiationFacts(<redacted>)")
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
#[repr(C)]
pub enum NativeTorrentNegotiationDecision {
    RejectStale,
    AuthRecovery,
    Legacy,
    Advertise,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
#[repr(C)]
pub enum NativeTorrentRecoveryAction {
    Retry,
    ChooseSource,
    Back,
}
#[derive(Clone, Debug, Deserialize, Serialize, Facet)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[facet(rename_all = "camelCase")]
pub struct NativeTorrentRecoveryFacts {
    pub admitted: bool,
    pub authority_retired: bool,
    pub authorization_refused: bool,
    pub selection_refused: bool,
    pub action: NativeTorrentRecoveryAction,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Facet, PartialEq)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
#[repr(C)]
pub enum NativeTorrentRecoveryDecision {
    WaitForRetirement,
    AuthRecovery,
    ChooseSource,
    Back,
    OrdinaryRetry,
    ForceGatewayRetry,
}
pub fn recovery_decision(f: &NativeTorrentRecoveryFacts) -> NativeTorrentRecoveryDecision {
    use NativeTorrentRecoveryDecision::*;
    if f.admitted && !f.authority_retired {
        return WaitForRetirement;
    }
    match f.action {
        NativeTorrentRecoveryAction::Back => Back,
        NativeTorrentRecoveryAction::ChooseSource => ChooseSource,
        NativeTorrentRecoveryAction::Retry if f.authorization_refused => AuthRecovery,
        NativeTorrentRecoveryAction::Retry if f.selection_refused => ChooseSource,
        NativeTorrentRecoveryAction::Retry if f.admitted => ForceGatewayRetry,
        NativeTorrentRecoveryAction::Retry => OrdinaryRetry,
    }
}

pub fn negotiation_decision(
    facts: &NativeTorrentNegotiationFacts,
) -> NativeTorrentNegotiationDecision {
    use NativeTorrentNegotiationDecision::*;
    if !facts.scope_matches {
        return RejectStale;
    }
    if facts.authorization_refused || matches!(facts.status, Some(401 | 403)) {
        return AuthRecovery;
    }
    if !facts.qualified || !android(&facts.platform) || facts.status != Some(200) {
        return Legacy;
    }
    match crate::domain::playback_protocol::parse(&facts.body) {
        Ok(protocol) if protocol.native_torrent_versions == [1] => Advertise,
        _ => Legacy,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AuthorizationScope {
    server_origin: String,
    account_id: String,
    profile_id: String,
    device_authorization_epoch: String,
}
impl fmt::Debug for AuthorizationScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeAuthorizationScope(<redacted>)")
    }
}
impl AuthorizationScope {
    fn canonical(&self) -> Option<(String, &str, &str, &str)> {
        if self.server_origin.len() > 2048
            || self.server_origin.trim() != self.server_origin
            || !identifier(&self.account_id)
            || !identifier(&self.profile_id)
            || !identifier(&self.device_authorization_epoch)
        {
            return None;
        }
        let origin = url::Url::parse(&self.server_origin).ok()?;
        if origin.scheme() != "https"
            || origin.host_str().is_none()
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.query().is_some()
            || origin.fragment().is_some()
            || origin.path() != "/"
        {
            return None;
        }
        Some((
            origin.to_string(),
            &self.account_id,
            &self.profile_id,
            &self.device_authorization_epoch,
        ))
    }
}

fn authorization_scope(input: &str) -> Result<String> {
    fn nullable_scope<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> std::result::Result<Option<AuthorizationScope>, D::Error> {
        Option::<AuthorizationScope>::deserialize(d)
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Facts {
        operation: String,
        #[serde(deserialize_with = "nullable_scope")]
        previous: Option<AuthorizationScope>,
        #[serde(deserialize_with = "nullable_scope")]
        current: Option<AuthorizationScope>,
        revoked: bool,
    }
    if input.len() > 8192 {
        return Err(invalid());
    }
    let facts: Facts = parse(input)?;
    if facts.operation != "authorizationScope" {
        return Err(invalid());
    }
    let previous = facts.previous.as_ref().map(AuthorizationScope::canonical);
    let current = facts.current.as_ref().map(AuthorizationScope::canonical);
    let decision = if previous.as_ref().is_some_and(Option::is_none)
        || current.as_ref().is_some_and(Option::is_none)
    {
        "reject"
    } else if facts.revoked || current.is_none() {
        if previous.is_some() {
            "retire"
        } else {
            "reject"
        }
    } else if previous.is_none() {
        "create"
    } else if previous == current {
        "keep"
    } else {
        "retire"
    };
    serde_json::to_string(decision).map_err(|_| invalid())
}
pub(crate) fn negotiation(input: &str) -> Result<String> {
    #[derive(Deserialize)]
    struct Operation {
        operation: String,
    }
    let operation: Operation = parse(input)?;
    if operation.operation == "failure" {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Failure {
            operation: String,
            reason: String,
        }
        if input.len() > 512 {
            return Err(invalid());
        }
        let value: Failure = parse(input)?;
        let message = crate::domain::native_failure_display(&value.reason).ok_or_else(invalid)?;
        if value.operation != "failure" {
            return Err(invalid());
        }
        return serde_json::to_string(&json!({"code": value.reason, "message": message}))
            .map_err(|_| invalid());
    }
    if operation.operation == "authorizationScope" {
        return authorization_scope(input);
    }
    if operation.operation == "recovery" {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Recovery {
            operation: String,
            facts: NativeTorrentRecoveryFacts,
        }
        let value: Recovery = parse(input)?;
        if value.operation != "recovery" {
            return Err(invalid());
        }
        return serde_json::to_string(&recovery_decision(&value.facts)).map_err(|_| invalid());
    }
    if operation.operation == "releaseResponse" {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Release {
            operation: String,
            body: String,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Response {
            ok: bool,
        }
        let value: Release = parse(input)?;
        if value.operation != "releaseResponse"
            || value.body.len() > 4096
            || !parse::<Response>(&value.body)?.ok
        {
            return Err(invalid());
        }
        return Ok("true".into());
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Input {
        operation: String,
        platform: PlaybackPlatform,
        qualified: bool,
        scope_matches: bool,
        status: Option<u16>,
        authorization_refused: bool,
        body: String,
    }
    let value: Input = parse(input)?;
    if value.operation != "negotiation" {
        return Err(invalid());
    }
    let facts = NativeTorrentNegotiationFacts {
        platform: value.platform,
        qualified: value.qualified,
        scope_matches: value.scope_matches,
        status: value.status,
        authorization_refused: value.authorization_refused,
        body: value.body,
    };
    serde_json::to_string(&negotiation_decision(&facts)).map_err(|_| invalid())
}

pub(crate) fn validate_start(input: &str) -> Result<()> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Request {
        operation: String,
        playback: PlaybackV2Request,
    }
    if input.len() > 16_384 {
        return Err(invalid());
    }
    let request: Request = parse(input)?;
    if request.operation != "playbackV2" {
        return Err(invalid());
    }
    crate::policy::validate_playback_v2(&request.playback)
}

#[derive(Clone, Debug, Deserialize, Serialize, Facet)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[facet(rename_all = "camelCase")]
pub struct NativeTorrentContext {
    pub origin: String,
    pub scope: String,
    pub generation: u64,
    pub qualified: bool,
    pub negotiated: bool,
    pub vod: bool,
    pub request: PlaybackV2Request,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Facet, PartialEq)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
#[repr(C)]
pub enum NativeTorrentControlOperation {
    Start,
    Poll,
    Heartbeat,
}
#[derive(Clone, Debug, Deserialize, Serialize, Facet)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[facet(rename_all = "camelCase")]
pub struct NativeTorrentObservation {
    pub scope: String,
    pub generation: u64,
    pub sequence: u64,
    pub operation: NativeTorrentControlOperation,
    pub received_at_millis: u64,
    pub round_trip_millis: u64,
    pub uncertainty_millis: Option<u64>,
    pub max_uncertainty_millis: u64,
    pub trusted_wall_upper_unix_millis: Option<u64>,
    pub suspend_aware: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize, Facet)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[facet(rename_all = "camelCase")]
pub struct NativeTorrentClock {
    pub scope: String,
    pub generation: u64,
    pub now_millis: u64,
    pub trusted_wall_upper_unix_millis: Option<u64>,
    pub backend_revalidated: bool,
    pub suspend_aware: bool,
}
/// Safe transport state only; not a player launch/session/history projection.
#[derive(Clone, Debug, Deserialize, Serialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct NativeTorrentState {
    pub status: String,
    pub deadline_millis: Option<u64>,
    pub expires_at_unix_millis: Option<u64>,
    pub position: Option<f64>,
    pub audio_language: Option<String>,
    pub subtitle_language: Option<String>,
    pub subtitles_enabled: Option<bool>,
    pub error: Option<String>,
}
impl NativeTorrentState {
    fn empty(status: &str) -> Self {
        Self {
            status: status.into(),
            deadline_millis: None,
            expires_at_unix_millis: None,
            position: None,
            audio_language: None,
            subtitle_language: None,
            subtitles_enabled: None,
            error: None,
        }
    }
}

// This wrapper preserves unsigned integer lexical spelling at the original boundary.
#[derive(Clone, Copy)]
struct Integer(u64);
impl<'de> Deserialize<'de> for Integer {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let raw = <&RawValue>::deserialize(d)?;
        let text = raw.get();
        if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
            return Err(serde::de::Error::custom("Invalid integer"));
        }
        text.parse()
            .map(Self)
            .map_err(|_| serde::de::Error::custom("Invalid integer"))
    }
}
fn nullable_text<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<String>, D::Error> {
    Option::<String>::deserialize(d)
}
fn nullable_raw<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<&'de RawValue>, D::Error> {
    Option::<&RawValue>::deserialize(d)
}
fn present_integer<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<Integer>, D::Error> {
    Integer::deserialize(d).map(Some)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<'a> {
    id: String,
    status: String,
    #[serde(borrow, deserialize_with = "nullable_raw")]
    delivery: Option<&'a RawValue>,
    #[serde(deserialize_with = "nullable_text")]
    error_code: Option<String>,
    #[serde(deserialize_with = "nullable_text")]
    error: Option<String>,
    expires_at: Integer,
    renew_after_seconds: Integer,
}
#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct NativeTorrentPreferences {
    #[serde(deserialize_with = "nullable_text")]
    pub audio_language: Option<String>,
    #[serde(deserialize_with = "nullable_text")]
    pub subtitle_language: Option<String>,
    pub subtitles_enabled: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Delivery<'a> {
    kind: String,
    position: f64,
    live: bool,
    format: String,
    preferences: NativeTorrentPreferences,
    #[serde(borrow)]
    grant: &'a RawValue,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantWire<'a> {
    version: Integer,
    id: String,
    server_time: Integer,
    expires_at: Integer,
    info_hash: String,
    file_index: Integer,
    network_policy: String,
    #[serde(borrow)]
    input: &'a RawValue,
    #[serde(default, deserialize_with = "present_integer")]
    expected_file_size: Option<Integer>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Magnet {
    kind: String,
    uri: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Metainfo {
    kind: String,
    metainfo_base64: String,
}
#[derive(Deserialize)]
struct Kind {
    kind: String,
}
/// Dedicated transient transport object, intentionally without Serialize/Deserialize/Facet.
#[derive(Clone, PartialEq)]
pub struct NativeTorrentInput {
    pub kind: String,
    pub value: String,
}
impl fmt::Debug for NativeTorrentInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeTorrentInput(<redacted>)")
    }
}
#[derive(Clone)]
pub struct NativeTorrentGrant {
    pub version: u32,
    pub network_policy: String,
    pub id: String,
    pub server_time: u64,
    pub expires_at: u64,
    pub info_hash: String,
    pub file_index: u32,
    pub input: NativeTorrentInput,
    pub expected_file_size: Option<u64>,
}
impl fmt::Debug for NativeTorrentGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeTorrentGrant(<redacted>)")
    }
}
impl NativeTorrentGrant {
    pub fn same_identity(&self, other: &Self) -> bool {
        self.version == other.version
            && self.network_policy == other.network_policy
            && self.id == other.id
            && self.info_hash == other.info_hash
            && self.file_index == other.file_index
            && self.input == other.input
            && self.expected_file_size == other.expected_file_size
    }
}
pub fn validate_native_grant(g: &NativeTorrentGrant, envelope_expires_at: u64) -> Result<()> {
    if g.version != 1
        || g.network_policy != POLICY
        || !identifier(&g.id)
        || g.server_time > MAX_SECONDS
        || g.expires_at > MAX_SECONDS
        || g.expires_at != envelope_expires_at
        || g.expires_at
            .checked_sub(g.server_time)
            .is_none_or(|seconds| seconds == 0 || seconds > 60)
        || !crate::native_torrent_policy::canonical_v1_hash(&g.info_hash)
        || g.file_index > 65535
        || g.expected_file_size
            .is_some_and(|n| !(1..=MAX_SAFE).contains(&n))
    {
        return Err(invalid());
    }
    match g.input.kind.as_str() {
        "magnet" if g.input.value == format!("magnet:?xt=urn:btih:{}", g.info_hash) => Ok(()),
        "metainfo" => {
            if g.input.value.len() > MAX_ENCODED || !g.input.value.len().is_multiple_of(4) {
                return Err(invalid());
            }
            let bytes = STANDARD.decode(&g.input.value).map_err(|_| invalid())?;
            if bytes.is_empty()
                || bytes.len() > MAX_META
                || STANDARD.encode(&bytes) != g.input.value
            {
                return Err(invalid());
            }
            crate::native_metainfo::validate(
                &bytes,
                &g.info_hash,
                g.file_index,
                g.expected_file_size,
            )
        }
        _ => Err(invalid()),
    }
}
struct Lease {
    id: String,
    status: String,
    expires_at: u64,
    grant: Option<NativeTorrentGrant>,
    position: Option<f64>,
    preferences: Option<NativeTorrentPreferences>,
    error: Option<String>,
}
fn decode(body: &str, origin: &str) -> Result<Lease> {
    if body.len() > MAX_BODY {
        return Err(invalid());
    }
    let e: Envelope<'_> = serde_json::from_str(body).map_err(|_| invalid())?;
    if !identifier(&e.id)
        || e.expires_at.0 > MAX_SECONDS
        || e.renew_after_seconds.0 != 20
        || !matches!(
            e.status.as_str(),
            "starting" | "ready" | "failed" | "expired" | "released"
        )
        || e.error_code.as_ref().is_some_and(|s| {
            s.is_empty()
                || s.len() > 128
                || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
        || e.error.as_ref().is_some_and(|s| s.len() > 1024)
        || (e.status == "ready" && (e.error.is_some() || e.error_code.is_some()))
        || (e.status != "ready" && e.delivery.is_some())
    {
        return Err(invalid());
    }
    let mut lease = Lease {
        id: e.id,
        status: e.status,
        expires_at: e.expires_at.0,
        grant: None,
        position: None,
        preferences: None,
        error: None,
    };
    if lease.status == "failed" || lease.status == "expired" {
        let safe = crate::domain::api_error(&json!({"status":502,"error_code":e.error_code}));
        lease.error = Some(if lease.status == "expired" {
            "Playback authorization expired. Retry playback.".into()
        } else {
            safe["message"]
                .as_str()
                .unwrap_or("Playback failed.")
                .into()
        });
    }
    if lease.status != "ready" {
        return Ok(lease);
    }
    let raw = e.delivery.ok_or_else(invalid)?;
    let kind: Kind = parse(raw.get())?;
    if matches!(kind.kind.as_str(), "direct" | "gateway") {
        reject_duplicates(body)?;
        let value: Value = parse(body)?;
        let allowed = [
            "kind",
            "url",
            "headers",
            "authorization",
            "position",
            "live",
            "format",
            "preferences",
            "mode",
            "video_mode",
            "audio_mode",
            "duration",
            "audio_tracks",
            "subtitle_tracks",
            "subtitles_supported",
        ];
        if value["delivery"]
            .as_object()
            .is_none_or(|map| map.keys().any(|key| !allowed.contains(&key.as_str())))
        {
            return Err(invalid());
        }
        if [
            "grant",
            "input",
            "info_hash",
            "file_index",
            "network_policy",
            "native_torrent",
        ]
        .iter()
        .any(|key| value["delivery"].get(key).is_some())
        {
            return Err(invalid());
        }
        crate::domain::normalize_value("playbackV2", &value, origin)?;
        lease.status = "legacy".into();
        return Ok(lease);
    }
    let d: Delivery<'_> = serde_json::from_str(raw.get()).map_err(|_| invalid())?;
    if d.kind != "native_torrent"
        || d.live
        || d.format != "original"
        || !d.position.is_finite()
        || !(0.0..=604800.0).contains(&d.position)
        || [
            &d.preferences.audio_language,
            &d.preferences.subtitle_language,
        ]
        .into_iter()
        .flatten()
        .any(|s| !language(s))
    {
        return Err(invalid());
    }
    let g: GrantWire<'_> = serde_json::from_str(d.grant.get()).map_err(|_| invalid())?;
    let input_kind: Kind = parse(g.input.get())?;
    let input = match input_kind.kind.as_str() {
        "magnet" => {
            let m: Magnet = parse(g.input.get())?;
            NativeTorrentInput {
                kind: m.kind,
                value: m.uri,
            }
        }
        "metainfo" => {
            let m: Metainfo = parse(g.input.get())?;
            NativeTorrentInput {
                kind: m.kind,
                value: m.metainfo_base64,
            }
        }
        _ => return Err(invalid()),
    };
    let grant = NativeTorrentGrant {
        version: u32::try_from(g.version.0).map_err(|_| invalid())?,
        network_policy: g.network_policy,
        id: g.id,
        server_time: g.server_time.0,
        expires_at: g.expires_at.0,
        info_hash: g.info_hash,
        file_index: u32::try_from(g.file_index.0).map_err(|_| invalid())?,
        input,
        expected_file_size: g.expected_file_size.map(|n| n.0),
    };
    validate_native_grant(&grant, lease.expires_at)?;
    reject_private_hints(&d.preferences, &grant)?;
    lease.position = Some(d.position);
    lease.preferences = Some(d.preferences);
    lease.grant = Some(grant);
    Ok(lease)
}
/// Conservative suspend-aware deadline from measured facts; no local clock fabricates authorization.
pub fn native_deadline(grant: &NativeTorrentGrant, o: &NativeTorrentObservation) -> Result<u64> {
    let u = o.uncertainty_millis.ok_or_else(invalid)?;
    let wall = o.trusted_wall_upper_unix_millis.ok_or_else(invalid)?;
    let expiry = grant.expires_at.checked_mul(1000).ok_or_else(invalid)?;
    if grant.server_time > MAX_SECONDS
        || grant.expires_at > MAX_SECONDS
        || !o.suspend_aware
        || u > o.max_uncertainty_millis
        || wall < grant.server_time * 1000
        || wall >= expiry
        || wall > MAX_SAFE
        || o.received_at_millis > MAX_SAFE
        || o.round_trip_millis > MAX_SAFE
        || o.max_uncertainty_millis > MAX_SAFE
    {
        return Err(invalid());
    }
    let remaining = grant
        .expires_at
        .checked_sub(grant.server_time)
        .filter(|n| *n > 0 && *n <= 60)
        .and_then(|n| n.checked_mul(1000))
        .and_then(|n| n.checked_sub(o.round_trip_millis))
        .and_then(|n| n.checked_sub(u))
        .and_then(|n| n.checked_sub(1000))
        .filter(|n| *n > 0)
        .ok_or_else(invalid)?;
    o.received_at_millis
        .checked_add(remaining)
        .filter(|n| *n <= MAX_SAFE)
        .ok_or_else(invalid)
}
struct Controller {
    context: NativeTorrentContext,
    state: NativeTorrentState,
    grant: Option<NativeTorrentGrant>,
    playback_id: Option<String>,
    sequence: Option<u64>,
    last_observed_millis: Option<u64>,
    trusted_wall_upper: Option<u64>,
    closed: bool,
}
impl Controller {
    fn invalidate(&mut self, status: &str) {
        self.grant = None;
        self.trusted_wall_upper = None;
        self.state = NativeTorrentState::empty(status);
        self.closed = true;
    }
    fn scope(&self, scope: &str, generation: u64) -> bool {
        self.context.scope == scope && self.context.generation == generation
    }
    fn accept(
        &mut self,
        status: u16,
        body: &str,
        o: &NativeTorrentObservation,
        measured: bool,
    ) -> Result<NativeTorrentState> {
        // Replaced/older callbacks cannot revoke or replace current accepted authority.
        if !self.scope(&o.scope, o.generation)
            || o.sequence > MAX_SAFE
            || self.sequence.is_some_and(|seq| o.sequence <= seq)
        {
            return Err(invalid());
        }
        if self.closed {
            return Err(invalid());
        }
        self.sequence = Some(o.sequence);
        let result = self.adopt(status, body, o, measured);
        if result.is_err() {
            self.invalidate("invalidated");
        }
        result
    }
    fn adopt(
        &mut self,
        status: u16,
        body: &str,
        o: &NativeTorrentObservation,
        measured: bool,
    ) -> Result<NativeTorrentState> {
        if !matches!(status, 200 | 202)
            || (status == 202 && o.operation != NativeTorrentControlOperation::Start)
        {
            return Err(invalid());
        }
        let lease = decode(body, &self.context.origin)?;
        if self.playback_id.as_ref().is_some_and(|id| *id != lease.id)
            || self
                .last_observed_millis
                .is_some_and(|previous| o.received_at_millis < previous)
            || (status == 202 && lease.status != "starting")
        {
            return Err(invalid());
        }
        if lease.status == "ready" {
            let request = &self.context.request;
            if !self.context.vod
                || !crate::native_torrent_policy::native_request_eligible(request)
                || lease.position != Some(request.position)
            {
                return Err(invalid());
            }
            let grant = lease.grant.ok_or_else(invalid)?;
            let mut measured_observation = o.clone();
            if measured {
                // Authenticated integer server time denotes the start of its second.
                // The full control RTT and precision bound conservatively cover receipt.
                measured_observation.trusted_wall_upper_unix_millis = grant
                    .server_time
                    .checked_mul(1000)
                    .and_then(|v| v.checked_add(o.round_trip_millis))
                    .and_then(|v| v.checked_add(o.uncertainty_millis?))
                    .and_then(|v| v.checked_add(1000));
            }
            let o = &measured_observation;
            let mut deadline = native_deadline(&grant, o)?;
            if let Some(previous) = &self.grant {
                crate::native_torrent_policy::validate_grant_transition(
                    previous,
                    &grant,
                    o.operation,
                )?;
                let last = self.state.deadline_millis.ok_or_else(invalid)?;
                if o.received_at_millis >= last
                    || o.trusted_wall_upper_unix_millis
                        .is_none_or(|wall| wall >= previous.expires_at * 1000)
                {
                    return Err(invalid());
                }
                if o.operation != NativeTorrentControlOperation::Heartbeat {
                    deadline = deadline.min(last);
                }
            }
            let prefs = lease.preferences.ok_or_else(invalid)?;
            self.state = NativeTorrentState {
                status: "ready".into(),
                deadline_millis: Some(deadline),
                expires_at_unix_millis: Some(grant.expires_at * 1000),
                position: lease.position,
                audio_language: prefs.audio_language,
                subtitle_language: prefs.subtitle_language,
                subtitles_enabled: Some(prefs.subtitles_enabled),
                error: None,
            };
            self.grant = Some(grant);
            self.trusted_wall_upper = o.trusted_wall_upper_unix_millis;
        } else {
            if self.grant.is_some() && matches!(lease.status.as_str(), "starting" | "legacy") {
                return Err(invalid());
            }
            self.grant = None;
            self.trusted_wall_upper = None;
            self.state = NativeTorrentState::empty(&lease.status);
            self.state.error = lease.error;
            self.closed = matches!(
                lease.status.as_str(),
                "failed" | "expired" | "released" | "legacy"
            );
        }
        self.playback_id = Some(lease.id);
        self.last_observed_millis = Some(o.received_at_millis);
        Ok(self.state.clone())
    }
    fn authorize(&mut self, clock: &NativeTorrentClock) -> Result<()> {
        if !self.scope(&clock.scope, clock.generation) {
            return Err(invalid());
        }
        if self.closed || self.grant.is_none() {
            return Err(invalid());
        }
        if !clock.suspend_aware
            || clock.now_millis > MAX_SAFE
            || clock.trusted_wall_upper_unix_millis.is_none_or(|wall| {
                wall > MAX_SAFE || Some(wall) >= self.state.expires_at_unix_millis
            })
            || self
                .last_observed_millis
                .is_some_and(|receipt| clock.now_millis < receipt)
            || self
                .state
                .deadline_millis
                .is_none_or(|deadline| clock.now_millis >= deadline)
        {
            self.invalidate("expired");
            return Err(invalid());
        }
        self.last_observed_millis = Some(clock.now_millis);
        if !clock.backend_revalidated {
            return Err(invalid());
        }
        Ok(())
    }
}
/// Non-serializable one-request/generation authority. Private getters are engine-adapter-only.
#[cfg_attr(feature = "native", derive(uniffi::Object))]
#[cfg_attr(feature = "native", uniffi::export(Debug, Display))]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen::prelude::wasm_bindgen)]
pub struct NativeTorrentBridge {
    inner: Mutex<Controller>,
}
impl fmt::Debug for NativeTorrentBridge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeTorrentBridge(<redacted>)")
    }
}
impl fmt::Display for NativeTorrentBridge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}
#[cfg_attr(feature = "native", uniffi::export)]
impl NativeTorrentBridge {
    #[cfg_attr(feature = "native", uniffi::constructor)]
    pub fn new(context: String) -> Result<Self> {
        if context.len() > 16_384 {
            return Err(invalid());
        }
        let context: NativeTorrentContext = parse(&context)?;
        crate::policy::validate_playback_v2(&context.request)?;
        let origin = url::Url::parse(&context.origin).map_err(|_| invalid())?;
        if origin.scheme() != "https"
            || origin.host_str().is_none()
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.query().is_some()
            || origin.fragment().is_some()
            || origin.path() != "/"
        {
            return Err(invalid());
        }
        if !identifier(&context.scope)
            || context.generation > MAX_SAFE
            || !context.qualified
            || !context.negotiated
            || !android(&context.request.client.platform)
            || context.request.client.native_torrent.is_none()
        {
            return Err(invalid());
        }
        Ok(Self {
            inner: Mutex::new(Controller {
                context,
                state: NativeTorrentState::empty("idle"),
                grant: None,
                playback_id: None,
                sequence: None,
                last_observed_millis: None,
                trusted_wall_upper: None,
                closed: false,
            }),
        })
    }
    pub fn accept(&self, status: u16, body: String, observation: String) -> Result<String> {
        let obs = if observation.len() <= 4096 {
            parse(&observation)
        } else {
            Err(invalid())
        };
        let obs = match obs {
            Ok(obs) => obs,
            Err(error) => {
                self.invalidate()?;
                return Err(error);
            }
        };
        let state = self
            .inner
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .accept(status, &body, &obs, false)?;
        serde_json::to_string(&state).map_err(|_| invalid())
    }
    /// HTTP adapters pass bounded identity-encoded bytes without lossy UTF-8 decoding.
    pub fn accept_bytes(&self, status: u16, body: Vec<u8>, observation: String) -> Result<String> {
        if body.len() > MAX_BODY {
            self.invalidate()?;
            return Err(invalid());
        }
        let body = match String::from_utf8(body) {
            Ok(body) => body,
            Err(_) => {
                self.invalidate()?;
                return Err(invalid());
            }
        };
        self.accept(status, body, observation)
    }
    /// Derive the trusted receipt clock only after strict authenticated grant decoding.
    pub fn accept_measured_bytes(
        &self,
        status: u16,
        body: Vec<u8>,
        observation: String,
    ) -> Result<String> {
        let decoded = (|| {
            if body.len() > MAX_BODY || observation.len() > 4096 {
                return Err(invalid());
            }
            let body = String::from_utf8(body).map_err(|_| invalid())?;
            let obs: NativeTorrentObservation = parse(&observation)?;
            if obs.trusted_wall_upper_unix_millis.is_some() {
                return Err(invalid());
            }
            Ok((body, obs))
        })();
        let (body, obs) = match decoded {
            Ok(value) => value,
            Err(error) => {
                self.invalidate()?;
                return Err(error);
            }
        };
        let state = self
            .inner
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .accept(status, &body, &obs, true)?;
        serde_json::to_string(&state).map_err(|_| invalid())
    }
    /// Safe clock fact; neither grant identity nor private source input is exposed.
    pub fn trusted_wall_upper_unix_millis(&self) -> Result<Option<u64>> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .trusted_wall_upper)
    }
    pub fn state(&self) -> Result<String> {
        serde_json::to_string(&self.inner.lock().map_err(|_| CoreError::Bridge)?.state)
            .map_err(|_| invalid())
    }
    pub fn invalidate(&self) -> Result<()> {
        self.inner
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .invalidate("invalidated");
        Ok(())
    }
    pub fn authorize(&self, clock: String) -> Result<String> {
        self.with_grant(&clock, |_| ())?;
        self.state()
    }
    pub fn playback_id(&self) -> Result<Option<String>> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .playback_id
            .clone())
    }
    pub fn private_info_hash(&self, clock: String) -> Result<String> {
        self.with_grant(&clock, |g| g.info_hash.clone())
    }
    pub fn private_input_kind(&self, clock: String) -> Result<String> {
        self.with_grant(&clock, |g| g.input.kind.clone())
    }
    pub fn private_input_value(&self, clock: String) -> Result<String> {
        self.with_grant(&clock, |g| g.input.value.clone())
    }
    pub fn private_file_index(&self, clock: String) -> Result<u32> {
        self.with_grant(&clock, |g| g.file_index)
    }
    pub fn private_expected_file_size(&self, clock: String) -> Result<Option<u64>> {
        self.with_grant(&clock, |g| g.expected_file_size)
    }
    /// Engine metadata must be validated before file access; observations cannot select a substitute.
    pub fn metadata_matches(&self, facts: String, clock: String) -> Result<bool> {
        let facts = if facts.len() <= 4096 {
            parse::<NativeTorrentMetadataFacts>(&facts)
        } else {
            Err(invalid())
        };
        let facts = match facts {
            Ok(facts) => facts,
            Err(error) => {
                self.invalidate()?;
                return Err(error);
            }
        };
        let matches = self.with_grant(&clock, |g| native_metadata_matches(g, &facts))?;
        if !matches {
            self.invalidate()?;
        }
        Ok(matches)
    }
    /// Generated private adapter parameters avoid a serializable hash-bearing DTO.
    pub fn metadata_matches_native(
        &self,
        info_hash: String,
        file_index: u32,
        file_count: u32,
        selected_file_size: u64,
        validated_v1_metadata: bool,
        clock: String,
    ) -> Result<bool> {
        let facts = NativeTorrentMetadataFacts {
            info_hash,
            file_index,
            file_count,
            selected_file_size,
            validated_v1_metadata,
        };
        let matches = self.with_grant(&clock, |grant| native_metadata_matches(grant, &facts))?;
        if !matches {
            self.invalidate()?;
        }
        Ok(matches)
    }
}
impl NativeTorrentBridge {
    fn with_grant<T>(&self, clock: &str, f: impl FnOnce(&NativeTorrentGrant) -> T) -> Result<T> {
        let clock = if clock.len() <= 4096 {
            parse::<NativeTorrentClock>(clock)
        } else {
            Err(invalid())
        };
        let clock = match clock {
            Ok(clock) => clock,
            Err(error) => {
                self.invalidate()?;
                return Err(error);
            }
        };
        let mut controller = self.inner.lock().map_err(|_| CoreError::Bridge)?;
        controller.authorize(&clock)?;
        controller.grant.as_ref().map(f).ok_or_else(invalid)
    }
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
impl NativeTorrentBridge {
    #[wasm_bindgen::prelude::wasm_bindgen(constructor)]
    pub fn wasm_new(
        context: String,
    ) -> std::result::Result<NativeTorrentBridge, wasm_bindgen::JsValue> {
        Self::new(context).map_err(|e| e.to_string().into())
    }
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=accept)]
    pub fn wasm_accept(
        &self,
        status: u16,
        body: String,
        observation: String,
    ) -> std::result::Result<String, wasm_bindgen::JsValue> {
        self.accept(status, body, observation)
            .map_err(|e| e.to_string().into())
    }
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=acceptBytes)]
    pub fn wasm_accept_bytes(
        &self,
        status: u16,
        body: Vec<u8>,
        observation: String,
    ) -> std::result::Result<String, wasm_bindgen::JsValue> {
        self.accept_bytes(status, body, observation)
            .map_err(|e| e.to_string().into())
    }
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=acceptMeasuredBytes)]
    pub fn wasm_accept_measured_bytes(
        &self,
        status: u16,
        body: Vec<u8>,
        observation: String,
    ) -> std::result::Result<String, wasm_bindgen::JsValue> {
        self.accept_measured_bytes(status, body, observation)
            .map_err(|e| e.to_string().into())
    }
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=trustedWallUpperUnixMillis)]
    pub fn wasm_trusted_wall_upper(
        &self,
    ) -> std::result::Result<Option<u64>, wasm_bindgen::JsValue> {
        self.trusted_wall_upper_unix_millis()
            .map_err(|e| e.to_string().into())
    }
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=state)]
    pub fn wasm_state(&self) -> std::result::Result<String, wasm_bindgen::JsValue> {
        self.state().map_err(|e| e.to_string().into())
    }
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=authorize)]
    pub fn wasm_authorize(
        &self,
        clock: String,
    ) -> std::result::Result<String, wasm_bindgen::JsValue> {
        self.authorize(clock).map_err(|e| e.to_string().into())
    }
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=invalidate)]
    pub fn wasm_invalidate(&self) -> std::result::Result<(), wasm_bindgen::JsValue> {
        self.invalidate().map_err(|e| e.to_string().into())
    }
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=playbackId)]
    pub fn wasm_playback_id(&self) -> std::result::Result<Option<String>, wasm_bindgen::JsValue> {
        self.playback_id().map_err(|e| e.to_string().into())
    }
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=privateInfoHash)]
    pub fn wasm_info_hash(
        &self,
        clock: String,
    ) -> std::result::Result<String, wasm_bindgen::JsValue> {
        self.private_info_hash(clock)
            .map_err(|e| e.to_string().into())
    }
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=privateInputKind)]
    pub fn wasm_input_kind(
        &self,
        clock: String,
    ) -> std::result::Result<String, wasm_bindgen::JsValue> {
        self.private_input_kind(clock)
            .map_err(|e| e.to_string().into())
    }
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=privateInputValue)]
    pub fn wasm_input_value(
        &self,
        clock: String,
    ) -> std::result::Result<String, wasm_bindgen::JsValue> {
        self.private_input_value(clock)
            .map_err(|e| e.to_string().into())
    }
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=privateFileIndex)]
    pub fn wasm_file_index(
        &self,
        clock: String,
    ) -> std::result::Result<u32, wasm_bindgen::JsValue> {
        self.private_file_index(clock)
            .map_err(|e| e.to_string().into())
    }
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=privateExpectedFileSize)]
    pub fn wasm_expected_size(
        &self,
        clock: String,
    ) -> std::result::Result<Option<u64>, wasm_bindgen::JsValue> {
        self.private_expected_file_size(clock)
            .map_err(|e| e.to_string().into())
    }
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=metadataMatches)]
    pub fn wasm_metadata_matches(
        &self,
        facts: String,
        clock: String,
    ) -> std::result::Result<bool, wasm_bindgen::JsValue> {
        self.metadata_matches(facts, clock)
            .map_err(|e| e.to_string().into())
    }
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=metadataMatchesNative)]
    pub fn wasm_metadata_matches_native(
        &self,
        info_hash: String,
        file_index: u32,
        file_count: u32,
        selected_file_size: u64,
        validated_v1_metadata: bool,
        clock: String,
    ) -> std::result::Result<bool, wasm_bindgen::JsValue> {
        self.metadata_matches_native(
            info_hash,
            file_index,
            file_count,
            selected_file_size,
            validated_v1_metadata,
            clock,
        )
        .map_err(|e| e.to_string().into())
    }
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=toString)]
    pub fn wasm_redacted(&self) -> String {
        self.to_string()
    }
}

/// Dedicated transient HTTP serializer; never persist/log this output or place it in app state.
/// Admission/auth and validated metainfo/source proof remain backend-injected prerequisites.
pub fn native_ready_response(
    playback_id: &str,
    position: f64,
    preferences: &NativeTorrentPreferences,
    grant: &NativeTorrentGrant,
) -> Result<String> {
    validate_native_grant(grant, grant.expires_at)?;
    reject_private_hints(preferences, grant)?;
    if !identifier(playback_id)
        || !position.is_finite()
        || !(0.0..=604800.0).contains(&position)
        || [&preferences.audio_language, &preferences.subtitle_language]
            .into_iter()
            .flatten()
            .any(|s| !language(s))
    {
        return Err(invalid());
    }
    let input = match grant.input.kind.as_str() {
        "magnet" => json!({"kind":"magnet","uri":grant.input.value}),
        "metainfo" => json!({"kind":"metainfo","metainfo_base64":grant.input.value}),
        _ => return Err(invalid()),
    };
    let mut wire = json!({"version":grant.version,"id":grant.id,"server_time":grant.server_time,"expires_at":grant.expires_at,
        "info_hash":grant.info_hash,"file_index":grant.file_index,"network_policy":grant.network_policy,"input":input});
    if let Some(size) = grant.expected_file_size {
        wire["expected_file_size"] = json!(size);
    }
    let response = json!({"id":playback_id,"status":"ready","delivery":{"kind":"native_torrent","position":position,"live":false,"format":"original",
        "preferences":{"audio_language":preferences.audio_language,"subtitle_language":preferences.subtitle_language,"subtitles_enabled":preferences.subtitles_enabled},"grant":wire},
        "error_code":null,"error":null,"expires_at":grant.expires_at,"renew_after_seconds":20});
    let response = serde_json::to_string(&response).map_err(|_| invalid())?;
    if response.len() > MAX_BODY {
        return Err(invalid());
    }
    Ok(response)
}

// Legacy HTTP deliveries retain their normalizer but cannot lose nested duplicates at this new boundary.
fn reject_duplicates(input: &str) -> Result<()> {
    struct Unique;
    impl<'de> Deserialize<'de> for Unique {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
            struct Visitor;
            impl<'de> serde::de::Visitor<'de> for Visitor {
                type Value = Unique;
                fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str("JSON without duplicate fields")
                }
                fn visit_map<A: serde::de::MapAccess<'de>>(
                    self,
                    mut map: A,
                ) -> std::result::Result<Unique, A::Error> {
                    let mut keys = std::collections::HashSet::new();
                    while let Some(key) = map.next_key::<String>()? {
                        if !keys.insert(key) {
                            return Err(serde::de::Error::custom("Duplicate field"));
                        }
                        map.next_value::<Unique>()?;
                    }
                    Ok(Unique)
                }
                fn visit_seq<A: serde::de::SeqAccess<'de>>(
                    self,
                    mut seq: A,
                ) -> std::result::Result<Unique, A::Error> {
                    while seq.next_element::<Unique>()?.is_some() {}
                    Ok(Unique)
                }
                fn visit_bool<E: serde::de::Error>(
                    self,
                    _: bool,
                ) -> std::result::Result<Unique, E> {
                    Ok(Unique)
                }
                fn visit_i64<E: serde::de::Error>(self, _: i64) -> std::result::Result<Unique, E> {
                    Ok(Unique)
                }
                fn visit_u64<E: serde::de::Error>(self, _: u64) -> std::result::Result<Unique, E> {
                    Ok(Unique)
                }
                fn visit_f64<E: serde::de::Error>(
                    self,
                    value: f64,
                ) -> std::result::Result<Unique, E> {
                    if value.is_finite() {
                        Ok(Unique)
                    } else {
                        Err(serde::de::Error::custom("Invalid number"))
                    }
                }
                fn visit_str<E: serde::de::Error>(self, _: &str) -> std::result::Result<Unique, E> {
                    Ok(Unique)
                }
                fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Unique, E> {
                    Ok(Unique)
                }
            }
            d.deserialize_any(Visitor)
        }
    }
    serde_json::from_str::<Unique>(input)
        .map(|_| ())
        .map_err(|_| invalid())
}

fn reject_private_hints(
    preferences: &NativeTorrentPreferences,
    grant: &NativeTorrentGrant,
) -> Result<()> {
    if [&preferences.audio_language, &preferences.subtitle_language]
        .into_iter()
        .flatten()
        .any(|s| s.contains(&grant.info_hash) || s.contains(&grant.input.value) || s == &grant.id)
    {
        return Err(invalid());
    }
    Ok(())
}
/// Observations of validated v1 metadata are private transport facts, not presentation/source fields.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeTorrentMetadataFacts {
    pub info_hash: String,
    pub file_index: u32,
    pub file_count: u32,
    pub selected_file_size: u64,
    pub validated_v1_metadata: bool,
}
impl fmt::Debug for NativeTorrentMetadataFacts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeTorrentMetadataFacts(<redacted>)")
    }
}
pub fn native_metadata_matches(g: &NativeTorrentGrant, facts: &NativeTorrentMetadataFacts) -> bool {
    facts.validated_v1_metadata
        && facts.info_hash == g.info_hash
        && facts.file_index == g.file_index
        && (1..=4096).contains(&facts.file_count)
        && facts.file_index < facts.file_count
        // File length is metadata, not a reservation for the rolling piece cache.
        && (1..=MAX_SAFE).contains(&facts.selected_file_size)
        && g.expected_file_size
            .is_none_or(|size| size == facts.selected_file_size)
}
