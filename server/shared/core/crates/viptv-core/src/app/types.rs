use crux_core::{capability::Operation, macros::effect, render::RenderOperation};
use crux_http::protocol::{HttpRequest, HttpResult};
use facet::Facet;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct Session {
    pub session_id: String,
    pub account_id: String,
    pub profile_id: Option<String>,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct Profile {
    #[serde(default)]
    pub raw: crate::dto::JsonObject,
    pub id: String,
    pub name: String,
    pub avatar: Option<String>,
    pub primary: Option<bool>,
    pub avatar_style: Option<String>,
    pub avatar_choice: Option<f64>,
    pub kid: Option<bool>,
    pub setup_complete: Option<bool>,
}
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
pub struct Account {
    pub id: String,
    pub username: String,
    pub name: String,
    pub role: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct Identity {
    pub account: Account,
    pub profiles: Vec<Profile>,
    pub profile_id: Option<String>,
    pub restricted: bool,
    pub profile_setup_required: bool,
}
#[derive(Clone, Default, Debug, Serialize, Deserialize, Facet, PartialEq)]
#[repr(C)]
pub enum Phase {
    #[default]
    Starting,
    Restoring,
    Checking,
    Selecting,
    Ready,
    Profiles,
    Pairing,
    Error,
}
#[derive(Clone, Default, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct ViewModel {
    pub phase: Phase,
    pub identity: Option<Identity>,
    pub selected_profile_id: Option<String>,
    pub error: Option<String>,
    pub error_status: Option<u16>,
}
#[derive(Serialize, Deserialize, Facet)]
#[repr(C)]
pub enum Event {
    Begin {
        origin: String,
        #[serde(rename = "allowInsecurePreview")]
        #[facet(rename = "allowInsecurePreview")]
        allow_insecure_preview: bool,
    },
    Retry,
    SelectProfile {
        #[serde(rename = "profileId")]
        #[facet(rename = "profileId")]
        profile_id: String,
    },
    AdoptSession {
        #[serde(rename = "tokensJson")]
        #[facet(rename = "tokensJson")]
        tokens_json: String,
    },
    SignOut,
    #[serde(skip)]
    #[facet(skip)]
    StorageCompleted(u64, StoragePurpose, StorageResult),
    #[serde(skip)]
    #[facet(skip)]
    HttpCompleted(u64, HttpPurpose, HttpResult),
}
#[derive(Clone, Serialize, Deserialize, Facet)]
#[repr(C)]
pub enum StorageOperation {
    Load,
    Save(String),
    Clear,
}
#[derive(Clone, Serialize, Deserialize, Facet)]
#[repr(C)]
pub enum StorageResult {
    Ok(Option<String>),
    Err(String),
}
impl Operation for StorageOperation {
    type Output = StorageResult;
}
#[effect(facet_typegen)]
pub enum Effect {
    Render(RenderOperation),
    Http(HttpRequest),
    Storage(StorageOperation),
}
#[derive(Clone, Copy, Facet)]
#[repr(C)]
pub enum StoragePurpose {
    Load,
    Refresh,
    Profile,
    Adopt,
    Clear,
}
#[derive(Clone, Facet)]
#[repr(C)]
pub enum HttpPurpose {
    Identity { refreshed: bool },
    Refresh,
    Select(String),
    Logout,
}
#[derive(Default)]
pub struct Model {
    pub view: ViewModel,
    pub(super) origin: String,
    pub(super) tokens: Option<Session>,
    pub(super) epoch: u64,
    pub(super) refresh_next: Option<HttpPurpose>,
    pub(super) refresh_attempted: bool,
    // Bound the post-selection refresh: an inconsistent identity must not
    // silently repeat a successful profile mutation. Reset on external actions.
    pub(super) accepted_profile: Option<String>,
}
#[derive(Default)]
pub struct Viptv;
