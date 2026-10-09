//! Private v2 authority holder shared by native clients and actual WASM.
use crate::{
    CoreError,
    native_torrent::{
        Integer, NativeTorrentClock, NativeTorrentContext, NativeTorrentControlOperation,
        NativeTorrentInput, NativeTorrentObservation, NativeTorrentPreferences, NativeTorrentState,
    },
    torrent_runtime::{NETWORK_POLICY, TorrentRuntimeGrant},
};
use serde::Deserialize;
use serde_json::value::RawValue;
use std::{fmt, sync::Mutex};

fn nullable<'de, T: Deserialize<'de>, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<T>, D::Error> {
    Option::<T>::deserialize(d)
}
fn present_integer<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<Integer>, D::Error> {
    Integer::deserialize(d).map(Some)
}

type Result<T> = std::result::Result<T, CoreError>;
const MAX_SAFE: u64 = 9_007_199_254_740_991;
const MAX_BODY: usize = 6 * 1024 * 1024;
fn invalid() -> CoreError {
    CoreError::InvalidInput
}
fn parse<T: serde::de::DeserializeOwned>(text: &str) -> Result<T> {
    serde_json::from_str(text).map_err(|_| invalid())
}
fn identifier(text: &str) -> bool {
    crate::domain::playback_protocol::valid_identifier(text)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<'a> {
    id: String,
    status: String,
    #[serde(borrow, deserialize_with = "nullable")]
    delivery: Option<&'a RawValue>,
    #[serde(deserialize_with = "nullable")]
    error_code: Option<String>,
    #[serde(deserialize_with = "nullable")]
    error: Option<String>,
    expires_at: Integer,
    renew_after_seconds: Integer,
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
struct GrantWire {
    version: Integer,
    network_policy: String,
    id: String,
    server_time: Integer,
    expires_at: Integer,
    info_hash: String,
    #[serde(deserialize_with = "nullable")]
    file_index: Option<Integer>,
    #[serde(deserialize_with = "nullable")]
    archive_index: Option<Integer>,
    trackers: Vec<String>,
    input: InputWire,
    #[serde(default, deserialize_with = "present_integer")]
    expected_file_size: Option<Integer>,
}
#[derive(Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum InputWire {
    #[serde(rename = "magnet")]
    Magnet { uri: String },
    #[serde(rename = "metainfo")]
    Metainfo { metainfo_base64: String },
}

struct Holder {
    context: NativeTorrentContext,
    state: NativeTorrentState,
    grant: Option<TorrentRuntimeGrant>,
    playback_id: Option<String>,
    sequence: Option<u64>,
    last_elapsed: Option<u64>,
    trusted_wall: Option<u64>,
    closed: bool,
    resolution: Option<(u32, Option<u32>, u64)>,
}
impl Holder {
    fn invalidate(&mut self, status: &str) {
        self.grant = None;
        self.resolution = None;
        self.trusted_wall = None;
        self.state = NativeTorrentState::empty(status);
        self.closed = true
    }
    fn current_scope(&self, scope: &str, generation: u64) -> bool {
        self.context.scope == scope && self.context.generation == generation
    }
    fn authorize(&mut self, clock: &NativeTorrentClock) -> Result<()> {
        if !self.current_scope(&clock.scope, clock.generation) {
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
                .last_elapsed
                .is_some_and(|last| clock.now_millis < last)
            || self
                .state
                .deadline_millis
                .is_none_or(|deadline| clock.now_millis >= deadline)
        {
            self.invalidate("expired");
            return Err(invalid());
        }
        self.last_elapsed = Some(clock.now_millis);
        if !clock.backend_revalidated {
            return Err(invalid());
        }
        Ok(())
    }
    fn accept(
        &mut self,
        status: u16,
        body: &str,
        observation: &NativeTorrentObservation,
        measured: bool,
    ) -> Result<NativeTorrentState> {
        if !self.current_scope(&observation.scope, observation.generation)
            || observation.sequence > MAX_SAFE
            || self.sequence.is_some_and(|old| observation.sequence <= old)
        {
            return Err(invalid());
        }
        if self.closed {
            return Err(invalid());
        }
        self.sequence = Some(observation.sequence);
        let result = self.adopt(status, body, observation, measured);
        if result.is_err() {
            self.invalidate("invalidated")
        }
        result
    }
    fn adopt(
        &mut self,
        status: u16,
        body: &str,
        observation: &NativeTorrentObservation,
        measured: bool,
    ) -> Result<NativeTorrentState> {
        if body.len() > MAX_BODY
            || !matches!(status, 200 | 202)
            || (status == 202 && observation.operation != NativeTorrentControlOperation::Start)
        {
            return Err(invalid());
        }
        let envelope: Envelope<'_> = serde_json::from_str(body).map_err(|_| invalid())?;
        if !identifier(&envelope.id)
            || envelope.expires_at.0 > MAX_SAFE / 1000
            || envelope.renew_after_seconds.0 != 20
            || !matches!(
                envelope.status.as_str(),
                "starting" | "ready" | "failed" | "expired" | "released"
            )
            || self
                .playback_id
                .as_ref()
                .is_some_and(|id| id != &envelope.id)
            || self
                .last_elapsed
                .is_some_and(|time| observation.received_at_millis < time)
            || (status == 202 && envelope.status != "starting")
            || (envelope.status == "ready"
                && (envelope.error.is_some() || envelope.error_code.is_some()))
            || (envelope.status != "ready" && envelope.delivery.is_some())
            || envelope.error_code.as_ref().is_some_and(|code| {
                code.is_empty()
                    || code.len() > 128
                    || !code
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            })
            || (measured && observation.trusted_wall_upper_unix_millis.is_some())
            || envelope
                .error
                .as_ref()
                .is_some_and(|error| error.len() > 1024)
        {
            return Err(invalid());
        }
        self.playback_id = Some(envelope.id);
        self.last_elapsed = Some(observation.received_at_millis);
        if envelope.status != "ready" {
            if self.grant.is_some() && envelope.status == "starting" {
                return Err(invalid());
            }
            self.grant = None;
            self.trusted_wall = None;
            self.state = NativeTorrentState::empty(&envelope.status);
            if matches!(envelope.status.as_str(), "failed" | "expired") {
                let safe = crate::domain::api_error(
                    &serde_json::json!({"status":502,"error_code":envelope.error_code}),
                );
                self.state.error = Some(
                    safe["message"]
                        .as_str()
                        .unwrap_or("Playback failed.")
                        .into(),
                );
            }
            self.closed = matches!(envelope.status.as_str(), "failed" | "expired" | "released");
            return Ok(self.state.clone());
        }
        let delivery = envelope.delivery.ok_or_else(invalid)?;
        #[derive(Deserialize)]
        struct Kind {
            kind: String,
        }
        let kind: Kind = parse(delivery.get())?;
        if matches!(kind.kind.as_str(), "direct" | "gateway") {
            if self.grant.is_some() {
                return Err(invalid());
            }
            crate::native_torrent::validate_ordinary_native_response(body, &self.context.origin)?;
            self.invalidate("legacy");
            return Ok(self.state.clone());
        }
        let delivery: Delivery<'_> = serde_json::from_str(delivery.get()).map_err(|_| invalid())?;
        if delivery.kind != "native_torrent"
            || delivery.live
            || delivery.format != "original"
            || delivery.position != self.context.request.position
            || !delivery.position.is_finite()
            || !(0.0..=604800.0).contains(&delivery.position)
        {
            return Err(invalid());
        }
        let wire: GrantWire = parse(delivery.grant.get())?;
        if wire.version.0 != 2 || wire.network_policy != NETWORK_POLICY {
            return Err(invalid());
        }
        let index = |value: Option<Integer>| {
            value
                .map(|value| u32::try_from(value.0).map_err(|_| invalid()))
                .transpose()
        };
        let input = match wire.input {
            InputWire::Magnet { uri } => NativeTorrentInput {
                kind: "magnet".into(),
                value: uri,
            },
            InputWire::Metainfo { metainfo_base64 } => NativeTorrentInput {
                kind: "metainfo".into(),
                value: metainfo_base64,
            },
        };
        let grant = TorrentRuntimeGrant {
            id: wire.id,
            server_time: wire.server_time.0,
            expires_at: wire.expires_at.0,
            info_hash: wire.info_hash,
            file_index: index(wire.file_index)?,
            archive_index: index(wire.archive_index)?,
            input,
            trackers: wire.trackers,
            expected_file_size: wire.expected_file_size.map(|n| n.0),
        };
        crate::torrent_runtime::validate_grant(&grant, envelope.expires_at.0)?;
        // Reuse the canonical serializer's preference/privacy validation.
        crate::torrent_runtime::ready_response(
            self.playback_id.as_deref().ok_or_else(invalid)?,
            delivery.position,
            &delivery.preferences,
            &grant,
        )?;
        let mut measured_observation = observation.clone();
        if measured {
            if observation.trusted_wall_upper_unix_millis.is_some() {
                return Err(invalid());
            }
            measured_observation.trusted_wall_upper_unix_millis = grant
                .server_time
                .checked_mul(1000)
                .and_then(|n| n.checked_add(observation.round_trip_millis))
                .and_then(|n| n.checked_add(observation.uncertainty_millis?))
                .and_then(|n| n.checked_add(1000));
        }
        let observation = &measured_observation;
        let mut deadline = crate::native_torrent::authority_deadline(
            grant.server_time,
            grant.expires_at,
            observation,
        )?;
        if let Some(previous) = &self.grant {
            crate::torrent_runtime::validate_transition(previous, &grant, observation.operation)?;
            let old = self.state.deadline_millis.ok_or_else(invalid)?;
            if observation.received_at_millis >= old
                || observation
                    .trusted_wall_upper_unix_millis
                    .is_none_or(|wall| wall >= previous.expires_at * 1000)
            {
                return Err(invalid());
            }
            if observation.operation != NativeTorrentControlOperation::Heartbeat {
                deadline = deadline.min(old)
            }
        }
        self.state = NativeTorrentState {
            status: "ready".into(),
            deadline_millis: Some(deadline),
            expires_at_unix_millis: Some(grant.expires_at * 1000),
            position: Some(delivery.position),
            audio_language: delivery.preferences.audio_language,
            subtitle_language: delivery.preferences.subtitle_language,
            subtitles_enabled: Some(delivery.preferences.subtitles_enabled),
            error: None,
        };
        self.trusted_wall = observation.trusted_wall_upper_unix_millis;
        self.grant = Some(grant);
        Ok(self.state.clone())
    }
}

#[cfg_attr(feature = "native", derive(uniffi::Object))]
#[cfg_attr(feature = "native", uniffi::export(Debug, Display))]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen::prelude::wasm_bindgen)]
pub struct TorrentRuntimeBridge {
    inner: Mutex<Holder>,
}
impl fmt::Debug for TorrentRuntimeBridge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TorrentRuntimeBridge(<redacted>)")
    }
}
impl fmt::Display for TorrentRuntimeBridge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

#[cfg_attr(feature = "native", uniffi::export)]
impl TorrentRuntimeBridge {
    #[cfg_attr(feature = "native", uniffi::constructor)]
    pub fn new(context: String) -> Result<Self> {
        if context.len() > 16384 {
            return Err(invalid());
        }
        let context: NativeTorrentContext = parse(&context)?;
        let origin = url::Url::parse(&context.origin).map_err(|_| invalid())?;
        if !identifier(&context.scope)
            || context.generation > MAX_SAFE
            || !context.qualified
            || !context.negotiated
            || !context.vod
            || !crate::torrent_runtime::request_eligible(&context.request)
            || origin.scheme() != "https"
            || origin.host_str().is_none()
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.query().is_some()
            || origin.fragment().is_some()
            || origin.path() != "/"
        {
            return Err(invalid());
        }
        Ok(Self {
            inner: Mutex::new(Holder {
                context,
                state: NativeTorrentState::empty("idle"),
                grant: None,
                playback_id: None,
                sequence: None,
                last_elapsed: None,
                trusted_wall: None,
                closed: false,
                resolution: None,
            }),
        })
    }
    pub fn accept_bytes(&self, status: u16, body: Vec<u8>, observation: String) -> Result<String> {
        self.accept_internal(status, body, observation, false)
    }
    pub fn accept_measured_bytes(
        &self,
        status: u16,
        body: Vec<u8>,
        observation: String,
    ) -> Result<String> {
        self.accept_internal(status, body, observation, true)
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
    pub fn trusted_wall_upper_unix_millis(&self) -> Result<Option<u64>> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .trusted_wall)
    }
    /// Bind the runtime's verified selection once; renewal does not choose another file.
    pub fn bind_resolution(
        &self,
        file_index: u32,
        archive_index: Option<u32>,
        length: u64,
        clock: String,
    ) -> Result<bool> {
        if clock.len() > 4096 {
            self.invalidate()?;
            return Err(invalid());
        }
        let clock: NativeTorrentClock = match parse(&clock) {
            Ok(value) => value,
            Err(error) => {
                self.invalidate()?;
                return Err(error);
            }
        };
        let mut holder = self.inner.lock().map_err(|_| CoreError::Bridge)?;
        holder.authorize(&clock)?;
        let grant = holder.grant.as_ref().ok_or_else(invalid)?;
        let resolution = (file_index, archive_index, length);
        if file_index > 65535
            || archive_index.is_some_and(|n| n > 65535)
            || length == 0
            || length > MAX_SAFE
            || grant.file_index.is_some_and(|n| n != file_index)
            || grant
                .archive_index
                .is_some_and(|n| Some(n) != archive_index)
            || (archive_index.is_none() && grant.expected_file_size.is_some_and(|n| n != length))
            || holder
                .resolution
                .is_some_and(|previous| previous != resolution)
        {
            holder.invalidate("invalidated");
            return Err(invalid());
        }
        holder.resolution = Some(resolution);
        Ok(true)
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
    pub fn private_file_index(&self, clock: String) -> Result<Option<u32>> {
        self.with_grant(&clock, |g| g.file_index)
    }
    pub fn private_archive_index(&self, clock: String) -> Result<Option<u32>> {
        self.with_grant(&clock, |g| g.archive_index)
    }
    pub fn private_trackers(&self, clock: String) -> Result<Vec<String>> {
        self.with_grant(&clock, |g| g.trackers.clone())
    }
    pub fn private_expected_file_size(&self, clock: String) -> Result<Option<u64>> {
        self.with_grant(&clock, |g| g.expected_file_size)
    }
}
impl TorrentRuntimeBridge {
    fn accept_internal(
        &self,
        status: u16,
        body: Vec<u8>,
        observation: String,
        measured: bool,
    ) -> Result<String> {
        let decoded = (|| {
            if body.len() > MAX_BODY || observation.len() > 4096 {
                return Err(invalid());
            };
            Ok((
                String::from_utf8(body).map_err(|_| invalid())?,
                parse::<NativeTorrentObservation>(&observation)?,
            ))
        })();
        let (body, observation) = match decoded {
            Ok(value) => value,
            Err(error) => {
                self.invalidate()?;
                return Err(error);
            }
        };
        let state = self.inner.lock().map_err(|_| CoreError::Bridge)?.accept(
            status,
            &body,
            &observation,
            measured,
        )?;
        serde_json::to_string(&state).map_err(|_| invalid())
    }
    fn with_grant<T>(
        &self,
        clock: &str,
        read: impl FnOnce(&TorrentRuntimeGrant) -> T,
    ) -> Result<T> {
        let clock = if clock.len() <= 4096 {
            parse::<NativeTorrentClock>(clock)
        } else {
            Err(invalid())
        };
        let clock = match clock {
            Ok(value) => value,
            Err(error) => {
                self.invalidate()?;
                return Err(error);
            }
        };
        let mut holder = self.inner.lock().map_err(|_| CoreError::Bridge)?;
        holder.authorize(&clock)?;
        holder.grant.as_ref().map(read).ok_or_else(invalid)
    }
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
impl TorrentRuntimeBridge {
    #[wasm_bindgen::prelude::wasm_bindgen(constructor)]
    pub fn wasm_new(
        context: String,
    ) -> std::result::Result<TorrentRuntimeBridge, wasm_bindgen::JsValue> {
        Self::new(context).map_err(|e| e.to_string().into())
    }
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=state)]
    pub fn wasm_state(&self) -> std::result::Result<String, wasm_bindgen::JsValue> {
        self.state().map_err(|e| e.to_string().into())
    }
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=invalidate)]
    pub fn wasm_invalidate(&self) -> std::result::Result<(), wasm_bindgen::JsValue> {
        self.invalidate().map_err(|e| e.to_string().into())
    }
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=playbackId)]
    pub fn wasm_playback_id(&self) -> std::result::Result<Option<String>, wasm_bindgen::JsValue> {
        self.playback_id().map_err(|e| e.to_string().into())
    }
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=trustedWallUpperUnixMillis)]
    pub fn wasm_trusted_wall_upper_unix_millis(
        &self,
    ) -> std::result::Result<Option<u64>, wasm_bindgen::JsValue> {
        self.trusted_wall_upper_unix_millis()
            .map_err(|e| e.to_string().into())
    }
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=authorize)]
    pub fn wasm_authorize(
        &self,
        clock: String,
    ) -> std::result::Result<String, wasm_bindgen::JsValue> {
        self.authorize(clock).map_err(|e| e.to_string().into())
    }
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=privateInfoHash)]
    pub fn wasm_private_info_hash(
        &self,
        clock: String,
    ) -> std::result::Result<String, wasm_bindgen::JsValue> {
        self.private_info_hash(clock)
            .map_err(|e| e.to_string().into())
    }
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=privateInputKind)]
    pub fn wasm_private_input_kind(
        &self,
        clock: String,
    ) -> std::result::Result<String, wasm_bindgen::JsValue> {
        self.private_input_kind(clock)
            .map_err(|e| e.to_string().into())
    }
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=privateInputValue)]
    pub fn wasm_private_input_value(
        &self,
        clock: String,
    ) -> std::result::Result<String, wasm_bindgen::JsValue> {
        self.private_input_value(clock)
            .map_err(|e| e.to_string().into())
    }
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=privateFileIndex)]
    pub fn wasm_private_file_index(
        &self,
        clock: String,
    ) -> std::result::Result<Option<u32>, wasm_bindgen::JsValue> {
        self.private_file_index(clock)
            .map_err(|e| e.to_string().into())
    }
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=privateArchiveIndex)]
    pub fn wasm_private_archive_index(
        &self,
        clock: String,
    ) -> std::result::Result<Option<u32>, wasm_bindgen::JsValue> {
        self.private_archive_index(clock)
            .map_err(|e| e.to_string().into())
    }
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=privateTrackers)]
    pub fn wasm_private_trackers(
        &self,
        clock: String,
    ) -> std::result::Result<Vec<String>, wasm_bindgen::JsValue> {
        self.private_trackers(clock)
            .map_err(|e| e.to_string().into())
    }
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=privateExpectedFileSize)]
    pub fn wasm_private_expected_file_size(
        &self,
        clock: String,
    ) -> std::result::Result<Option<u64>, wasm_bindgen::JsValue> {
        self.private_expected_file_size(clock)
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
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=bindResolution)]
    pub fn wasm_bind_resolution(
        &self,
        file_index: u32,
        archive_index: Option<u32>,
        length: u64,
        clock: String,
    ) -> std::result::Result<bool, wasm_bindgen::JsValue> {
        self.bind_resolution(file_index, archive_index, length, clock)
            .map_err(|e| e.to_string().into())
    }
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=toString)]
    pub fn wasm_redacted(&self) -> String {
        self.to_string()
    }
}
