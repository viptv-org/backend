//! VIPTV behavior shared by browser WASM and native shells. Roku is independent.
pub mod app;
pub mod domain;
pub mod dto;
mod native_metainfo;
pub mod native_torrent;
pub mod native_torrent_policy;
pub mod policy;
pub mod torrent_runtime;
pub mod vizio;
pub use app::*;
use crux_core::{
    Core,
    bridge::{Bridge, EffectId, JsonFfiFormat},
};
pub use native_torrent::NativeTorrentBridge;

#[derive(Debug, thiserror::Error)]
#[cfg_attr(feature = "native", derive(uniffi::Error))]
pub enum CoreError {
    #[error("Invalid server response or input")]
    InvalidInput,
    #[error("Could not process core request")]
    Bridge,
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen::prelude::wasm_bindgen)]
#[cfg_attr(feature = "native", derive(uniffi::Object))]
pub struct CoreBridge {
    inner: Bridge<Viptv, JsonFfiFormat>,
}
impl Default for CoreBridge {
    fn default() -> Self {
        Self {
            inner: Bridge::new(Core::new()),
        }
    }
}

#[cfg_attr(feature = "native", uniffi::export)]
impl CoreBridge {
    #[cfg_attr(feature = "native", uniffi::constructor)]
    pub fn new() -> Self {
        Self::default()
    }
    pub fn update(&self, event: String) -> Result<String, CoreError> {
        let mut output = vec![];
        self.inner
            .update(event.as_bytes(), &mut output)
            .map_err(|_| CoreError::Bridge)?;
        String::from_utf8(output).map_err(|_| CoreError::Bridge)
    }
    pub fn resolve(&self, id: u32, result: String) -> Result<String, CoreError> {
        let mut output = vec![];
        self.inner
            .resolve(EffectId(id), result.as_bytes(), &mut output)
            .map_err(|_| CoreError::Bridge)?;
        String::from_utf8(output).map_err(|_| CoreError::Bridge)
    }
    pub fn view(&self) -> Result<String, CoreError> {
        let mut output = vec![];
        self.inner
            .view(&mut output)
            .map_err(|_| CoreError::Bridge)?;
        String::from_utf8(output).map_err(|_| CoreError::Bridge)
    }
}
#[cfg_attr(feature = "native", uniffi::export)]
pub fn normalize(kind: String, input: String, origin: String) -> Result<String, CoreError> {
    if input.len() > 2 * 1024 * 1024 {
        return Err(CoreError::InvalidInput);
    }
    // Closed protocol validation must precede Value's loss of duplicate fields/tokens.
    if kind == "playbackProtocolV2" {
        return serde_json::to_string(&domain::playback_protocol::parse(&input)?)
            .map_err(|_| CoreError::InvalidInput);
    }
    if kind == "torrentRuntimeProtocol" {
        return serde_json::to_string(&domain::playback_protocol::parse_runtime(&input)?)
            .map_err(|_| CoreError::InvalidInput);
    }
    if kind == "nativeTorrent" {
        return native_torrent::negotiation(&input);
    }
    if kind == "request" {
        // Dispatch must not erase a repeated operation before closed validation.
        #[derive(serde::Deserialize)]
        struct Operation {
            operation: String,
        }
        let operation: Operation =
            serde_json::from_str(&input).map_err(|_| CoreError::InvalidInput)?;
        if operation.operation.is_empty() {
            return Err(CoreError::InvalidInput);
        }
    }
    let value: serde_json::Value =
        serde_json::from_str(&input).map_err(|_| CoreError::InvalidInput)?;
    if kind != "request" && native_torrent::contains_private_transport(&value) {
        return Err(CoreError::InvalidInput);
    }
    if kind == "request"
        && matches!(
            value["operation"].as_str(),
            Some(
                "playbackProtocolV2"
                    | "torrentRuntimeProtocol"
                    | "playbackV2CancelRequest"
                    | "playbackV2Status"
                    | "playbackV2Heartbeat"
                    | "playbackV2Stop"
            )
        )
    {
        domain::playback_protocol::validate_request(&input)?;
    }
    if kind == "request"
        && (value["operation"] == "playbackV2"
            || value["playback"]["client"].get("nativeTorrent").is_some())
    {
        native_torrent::validate_start(&input)?;
    }
    let normalized = domain::normalize_owned(&kind, value, &origin)?;
    validate_normalized(&kind, &normalized)?;
    serde_json::to_string(&normalized).map_err(|_| CoreError::InvalidInput)
}

/// Plan one SmartCast request without performing network or credential I/O.
#[cfg_attr(feature = "native", uniffi::export)]
pub fn vizio_request(operation: String, input: String) -> String {
    serde_json::to_string(&vizio::plan_request(&operation, &input))
        .expect("SmartCast request results are serializable")
}

/// Interpret SmartCast HTTP and protocol status without exposing transport details.
#[cfg_attr(feature = "native", uniffi::export)]
pub fn vizio_response(status: u16, body: String, allow_statusless: bool) -> String {
    serde_json::to_string(&vizio::parse_response(status, &body, allow_statusless))
        .expect("SmartCast response results are serializable")
}

/// Return the bounded modern/legacy host probe order for one caller-approved /24.
#[cfg_attr(feature = "native", uniffi::export)]
pub fn vizio_discovery_candidates(subnet: String) -> String {
    let result = vizio::discovery_candidates(&subnet);
    serde_json::to_string(&result).expect("SmartCast discovery results are serializable")
}

/// The display name from a SmartCast deviceinfo response, or `null` when the
/// answering host is not a Vizio television. Direct-probe discovery uses
/// this after connecting to each candidate from `vizio_discovery_candidates`.
#[cfg_attr(feature = "native", uniffi::export)]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen::prelude::wasm_bindgen)]
pub fn vizio_deviceinfo_name(body: String) -> Option<String> {
    vizio::deviceinfo_name(&body)
}

/// Report whether this target can execute SmartCast directly or needs a LAN bridge.
#[cfg_attr(feature = "native", uniffi::export)]
pub fn vizio_platform_support(platform: String) -> String {
    serde_json::to_string(&vizio::platform_support(&platform))
        .expect("SmartCast support is serializable")
}

/// Stateful SmartCast workflow used by Android mobile and Tauri desktop.
///
/// The shell executes each returned request and resolves it by ID. The bridge
/// keeps pairing credentials, fresh hash values, retry state, and command
/// serialization out of UI code.
#[cfg_attr(feature = "native", derive(uniffi::Object))]
pub struct SmartCastBridge {
    inner: std::sync::Mutex<vizio::VizioController>,
}

#[cfg_attr(feature = "native", uniffi::export)]
impl SmartCastBridge {
    #[cfg_attr(feature = "native", uniffi::constructor)]
    pub fn new(config: String) -> Result<Self, CoreError> {
        Ok(Self {
            inner: std::sync::Mutex::new(
                vizio::VizioController::new(&config).map_err(|_| CoreError::InvalidInput)?,
            ),
        })
    }

    pub fn start(&self, operation: String, input: String) -> Result<String, CoreError> {
        let output = self
            .inner
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .start(&operation, &input);
        serialize_smartcast(output)
    }

    pub fn resolve(&self, request_id: u32, status: u16, body: String) -> Result<String, CoreError> {
        let output = self
            .inner
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .resolve(request_id, status, &body);
        serialize_smartcast(output)
    }

    pub fn reject(&self, request_id: u32) -> Result<String, CoreError> {
        let output = self
            .inner
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .reject(request_id);
        serialize_smartcast(output)
    }

    pub fn cancel(&self) -> Result<(), CoreError> {
        self.inner.lock().map_err(|_| CoreError::Bridge)?.cancel();
        Ok(())
    }

    /// Return the in-memory pairing credential only to the native vault adapter.
    pub fn credential(&self) -> Result<Option<String>, CoreError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .auth_token())
    }

    pub fn clear_credential(&self) -> Result<(), CoreError> {
        self.inner
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .clear_auth_token();
        Ok(())
    }
}

fn serialize_smartcast(output: vizio::VizioControllerOutput) -> Result<String, CoreError> {
    serde_json::to_string(&output).map_err(|_| CoreError::Bridge)
}
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
impl CoreBridge {
    #[wasm_bindgen::prelude::wasm_bindgen(constructor)]
    pub fn wasm_new() -> Self {
        Self::new()
    }
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=update)]
    pub fn wasm_update(&self, event: String) -> Result<String, wasm_bindgen::JsValue> {
        self.update(event).map_err(|e| e.to_string().into())
    }
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=resolve)]
    pub fn wasm_resolve(&self, id: u32, result: String) -> Result<String, wasm_bindgen::JsValue> {
        self.resolve(id, result).map_err(|e| e.to_string().into())
    }
    #[wasm_bindgen::prelude::wasm_bindgen(js_name=view)]
    pub fn wasm_view(&self) -> Result<String, wasm_bindgen::JsValue> {
        self.view().map_err(|e| e.to_string().into())
    }
}
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(js_name=normalize)]
pub fn wasm_normalize(
    kind: String,
    input: String,
    origin: String,
) -> Result<String, wasm_bindgen::JsValue> {
    normalize(kind, input, origin).map_err(|e| e.to_string().into())
}
#[cfg(feature = "native")]
uniffi::setup_scaffolding!();

// Keep serde validation out of the WASI caller's BrightScript label budget.
#[cfg_attr(target_os = "wasi", inline(never))]
fn validate_normalized(kind: &str, v: &serde_json::Value) -> Result<(), CoreError> {
    #[cfg_attr(target_os = "wasi", inline(never))]
    fn check<T: serde::de::DeserializeOwned>(v: &serde_json::Value) -> Result<(), CoreError> {
        T::deserialize(v)
            .map(|_| ())
            .map_err(|_| CoreError::InvalidInput)
    }
    match kind {
        "cardPresentation" => check::<dto::CardPresentation>(v),
        "homeActions" => check::<dto::HomeActions>(v),
        "episodeWatching" => check::<dto::EpisodeWatching>(v),
        "phonePresentation" => check::<dto::PhonePresentation>(v),
        "sourceRanks" => check::<dto::SourceRanks>(v),
        "sourceProducerLabels" => check::<Vec<dto::SourceProducerOutcome>>(v),
        "sourcesPollStep" => check::<dto::SourcesPollStep>(v),
        "discoverPolicy" => check::<dto::DiscoverPolicyProjection>(v),
        "catalog" => check::<dto::Catalog>(v),
        "catalogs" => check::<Vec<dto::Catalog>>(v),
        "media" => check::<dto::MediaItem>(v),
        "source" => check::<dto::MediaSource>(v),
        "playback" => check::<dto::PlaybackSession>(v),
        "discover" => check::<dto::DiscoverPage>(v),
        _ => Ok(()),
    }
}
