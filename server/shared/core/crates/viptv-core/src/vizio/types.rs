use crate::dto::JsonObject;
use facet::Facet;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize, Facet, PartialEq)]
#[serde(rename_all = "UPPERCASE")]
#[facet(rename_all = "UPPERCASE")]
#[repr(C)]
pub enum VizioHttpMethod {
    Get,
    Put,
}

#[derive(Clone, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct VizioRequest {
    pub method: VizioHttpMethod,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Option<JsonObject>,
    pub timeout_millis: u64,
    pub max_response_bytes: u64,
}

impl std::fmt::Debug for VizioRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VizioRequest")
            .field("method", &self.method)
            .field("url", &"<redacted>")
            .field("headers", &"<redacted>")
            .field("body", &self.body.as_ref().map(|_| "<redacted>"))
            .field("timeout_millis", &self.timeout_millis)
            .field("max_response_bytes", &self.max_response_bytes)
            .finish()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet, PartialEq)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
#[repr(C)]
pub enum VizioFailureKind {
    InvalidConfig,
    InvalidInput,
    Authentication,
    InvalidParameter,
    EndpointNotFound,
    Busy,
    Transport,
    InvalidResponse,
    HttpStatus,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct VizioFailure {
    pub kind: VizioFailureKind,
    pub message: String,
    pub retryable: bool,
    pub protocol_status: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, Facet)]
#[repr(C)]
pub enum VizioRequestResult {
    Ok(VizioRequest),
    Err(VizioFailure),
}

#[derive(Clone, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct VizioProtocolResponse {
    pub status: String,
    pub detail: String,
    pub raw: JsonObject,
}

impl std::fmt::Debug for VizioProtocolResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VizioProtocolResponse")
            .field("status", &self.status)
            .field("detail", &"<redacted>")
            .field("raw", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[repr(C)]
pub enum VizioResponseResult {
    Ok(VizioProtocolResponse),
    Err(VizioFailure),
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[facet(rename_all = "SCREAMING_SNAKE_CASE")]
#[repr(C)]
pub enum VizioRemoteAction {
    Keypress,
    Keydown,
    Keyup,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct VizioRemoteEvent {
    pub code_set: u16,
    pub code: u16,
    pub action: VizioRemoteAction,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[facet(rename_all = "SCREAMING_SNAKE_CASE")]
#[repr(C)]
pub enum VizioRemoteKey {
    SeekFwd,
    SeekBack,
    Pause,
    Play,
    Down,
    Left,
    Ok,
    Right,
    Up,
    Back,
    Smartcast,
    CcToggle,
    Info,
    Menu,
    Home,
    VolDown,
    VolUp,
    MuteOff,
    MuteOn,
    MuteToggle,
    PicMode,
    PicSize,
    InputNext,
    ChDown,
    ChUp,
    ChPrev,
    Exit,
    PowOff,
    PowOn,
    PowToggle,
}

impl VizioRemoteKey {
    pub fn event(&self, action: VizioRemoteAction) -> VizioRemoteEvent {
        let (code_set, code) = match self {
            Self::SeekFwd => (2, 0),
            Self::SeekBack => (2, 1),
            Self::Pause => (2, 2),
            Self::Play => (2, 3),
            Self::Down => (3, 0),
            Self::Left => (3, 1),
            Self::Ok => (3, 2),
            Self::Right => (3, 7),
            Self::Up => (3, 8),
            Self::Back => (4, 0),
            Self::Smartcast => (4, 3),
            Self::CcToggle => (4, 4),
            Self::Info => (4, 6),
            Self::Menu => (4, 8),
            Self::Home => (4, 15),
            Self::VolDown => (5, 0),
            Self::VolUp => (5, 1),
            Self::MuteOff => (5, 2),
            Self::MuteOn => (5, 3),
            Self::MuteToggle => (5, 4),
            Self::PicMode => (6, 0),
            Self::PicSize => (6, 2),
            Self::InputNext => (7, 1),
            Self::ChDown => (8, 0),
            Self::ChUp => (8, 1),
            Self::ChPrev => (8, 2),
            Self::Exit => (9, 0),
            Self::PowOff => (11, 0),
            Self::PowOn => (11, 1),
            Self::PowToggle => (11, 2),
        };
        VizioRemoteEvent {
            code_set,
            code,
            action,
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct VizioPairingChallenge {
    pub challenge_type: u16,
    pub token: u64,
}

impl std::fmt::Debug for VizioPairingChallenge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VizioPairingChallenge")
            .field("challenge_type", &self.challenge_type)
            .field("token", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct VizioAppConfig {
    pub app_id: String,
    pub name_space: u16,
    pub message: Option<String>,
}

impl std::fmt::Debug for VizioAppConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VizioAppConfig")
            .field("app_id", &self.app_id)
            .field("name_space", &self.name_space)
            .field("message", &self.message.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct VizioInputInfo {
    pub cname: String,
    pub name: String,
    pub meta_name: String,
    pub current: bool,
    pub hash_value: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct VizioDiscoveryCandidate {
    pub host: String,
    pub port: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet, PartialEq)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
#[repr(C)]
pub enum VizioTransportSupport {
    Native,
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct VizioPlatformSupport {
    pub protocol_available: bool,
    pub transport: VizioTransportSupport,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet, PartialEq)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
#[repr(C)]
pub enum VizioControllerOutputKind {
    Request,
    Complete,
    Error,
}

#[derive(Clone, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct VizioControllerOutput {
    pub kind: VizioControllerOutputKind,
    pub request_id: Option<u32>,
    pub request: Option<VizioRequest>,
    pub result: Option<JsonObject>,
    pub error: Option<VizioFailure>,
    pub credential_changed: bool,
}

impl std::fmt::Debug for VizioControllerOutput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VizioControllerOutput")
            .field("kind", &self.kind)
            .field("request_id", &self.request_id)
            .field("request", &self.request.as_ref().map(|_| "<redacted>"))
            .field("result", &self.result.as_ref().map(|_| "<redacted>"))
            .field("error", &self.error)
            .field("credential_changed", &self.credential_changed)
            .finish()
    }
}
