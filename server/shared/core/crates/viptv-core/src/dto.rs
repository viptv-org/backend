//! Shared normalized presentation data. Unknown metadata stays JSON and is sanitized.
use facet::Facet;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Clone, Debug, Serialize, Facet, PartialEq)]
#[serde(untagged)]
#[facet(untagged)]
#[repr(C)]
pub enum JsonValue {
    Null,
    Boolean(bool),
    Number(f64),
    String(String),
    Array(Vec<JsonValue>),
    Object(BTreeMap<String, JsonValue>),
}

// Untagged derive retries every preceding variant (including error allocation)
// for each metadata value. JSON already tells us which variant it contains.
impl<'de> Deserialize<'de> for JsonValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct JsonVisitor;
        impl<'de> serde::de::Visitor<'de> for JsonVisitor {
            type Value = JsonValue;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a JSON value")
            }
            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(JsonValue::Null)
            }
            fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
                Ok(JsonValue::Boolean(value))
            }
            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
                Ok(JsonValue::Number(value as f64))
            }
            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
                Ok(JsonValue::Number(value as f64))
            }
            fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E> {
                Ok(JsonValue::Number(value))
            }
            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
                Ok(JsonValue::String(value.to_owned()))
            }
            fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
                Ok(JsonValue::String(value))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut items: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = items.next_element()? {
                    values.push(value);
                }
                Ok(JsonValue::Array(values))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut fields: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = BTreeMap::new();
                while let Some((key, value)) = fields.next_entry()? {
                    values.insert(key, value);
                }
                Ok(JsonValue::Object(values))
            }
        }
        deserializer.deserialize_any(JsonVisitor)
    }
}
pub type JsonObject = BTreeMap<String, JsonValue>;
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "lowercase")]
#[facet(rename_all = "lowercase")]
#[repr(C)]
pub enum MediaKind {
    Movie,
    Series,
    Live,
    Episode,
}
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct CatalogExtra {
    pub name: String,
    pub required: bool,
    pub options: Vec<String>,
    pub default_value: Option<String>,
    pub options_limit: Option<f64>,
}
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct Catalog {
    pub id: String,
    pub name: String,
    /// Addons define their own catalog namespaces independently of media kinds.
    pub r#type: String,
    pub addon_name: Option<String>,
    pub addon_id: Option<f64>,
    pub addon_key: Option<String>,
    pub supports_search: bool,
    pub supports_skip: bool,
    pub extras: Vec<CatalogExtra>,
    pub genres: Vec<String>,
    pub raw: JsonObject,
}
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct MediaItem {
    pub id: String,
    pub r#type: MediaKind,
    pub name: String,
    pub title: String,
    pub poster: Option<String>,
    pub background: Option<String>,
    pub thumbnail: Option<String>,
    pub title_logo: Option<String>,
    pub imdb_rating: Option<String>,
    pub credits: Option<String>,
    pub poster_shape: Option<String>,
    pub updated_at_millis: Option<f64>,
    pub released_at_millis: Option<f64>,
    #[serde(default)]
    pub episodes: Vec<MediaItem>,
    pub description: Option<String>,
    pub year: Option<f64>,
    pub runtime: Option<String>,
    pub genres: Vec<String>,
    pub position: Option<f64>,
    pub duration: Option<f64>,
    pub watched: Option<bool>,
    pub resume_active: Option<bool>,
    pub watch_date_known: Option<bool>,
    pub completion_only: Option<bool>,
    pub season: Option<f64>,
    pub episode: Option<f64>,
    pub episode_title: Option<String>,
    pub series_id: Option<String>,
    pub queue_status: Option<String>,
    pub previous_episode: Option<Box<MediaItem>>,
    pub source_addon_id: Option<String>,
    pub source_name: Option<String>,
    pub source_fingerprint: Option<String>,
    pub source_binge_group: Option<String>,
    pub source_release_group: Option<String>,
    pub source_quality: Option<String>,
    pub source_audio: Option<String>,
    pub raw: JsonObject,
}
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct MediaSource {
    pub provider: Option<String>,
    pub description: Option<String>,
    pub source_fingerprint: Option<String>,
    pub id: String,
    pub name: String,
    pub title: Option<String>,
    pub filename: Option<String>,
    pub source_addon_id: Option<String>,
    pub source_name: Option<String>,
    pub quality: Option<String>,
    pub audio: Option<String>,
    pub raw: JsonObject,
}
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct SourceFailure {
    pub source: String,
    pub code: Option<String>,
    pub message: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct SourcesPollState {
    pub after: f64,
    pub sources: Vec<MediaSource>,
    pub polls: u32,
    pub errors: Option<Vec<SourceFailure>>,
    #[serde(default)]
    pub producers: Vec<SourceProducerOutcome>,
}
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct SourcesPollStep {
    pub state: SourcesPollState,
    pub sources: Vec<MediaSource>,
    pub done: bool,
    #[serde(default)]
    pub producers: Vec<SourceProducerOutcome>,
}
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct MediaTrack {
    pub input_index: f64,
    pub codec: Option<String>,
    pub language: Option<String>,
    pub language_status: String,
    pub title: String,
    pub selected: bool,
    pub supported: bool,
    pub selectable: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct PlaybackSession {
    pub delivery_kind: Option<PlaybackDeliveryKind>,
    pub preferred_audio_language: Option<String>,
    pub preferred_subtitle_language: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    pub id: String,
    pub url: String,
    pub format: String,
    pub mode: String,
    pub video_mode: String,
    pub audio_mode: String,
    pub position: f64,
    pub live: bool,
    pub duration: f64,
    pub audio_tracks: Vec<MediaTrack>,
    pub subtitle_tracks: Vec<MediaTrack>,
    pub subtitles_supported: bool,
    pub authorization: Option<PlaybackAuthorization>,
}
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "lowercase")]
#[facet(rename_all = "lowercase")]
#[repr(C)]
pub enum PlaybackDeliveryKind {
    Direct,
    Gateway,
}
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "lowercase")]
#[facet(rename_all = "lowercase")]
#[repr(C)]
pub enum PlaybackLeaseStatus {
    Starting,
    Ready,
    Failed,
    Expired,
    Released,
}
/// Validated protocol support only; no account admission, grants or qualification.
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct PlaybackProtocol {
    pub version: u32,
    pub native_torrent_versions: Vec<u32>,
}

#[derive(Clone, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct PlaybackLease {
    pub id: String,
    pub status: PlaybackLeaseStatus,
    /// Unix time in milliseconds; platform clock checks expiry before use.
    pub expires_at: f64,
    pub renew_after_seconds: u32,
    pub session: Option<PlaybackSession>,
    pub error_code: Option<String>,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, Facet, PartialEq)]
#[serde(rename_all = "snake_case")]
#[facet(rename_all = "snake_case")]
#[repr(C)]
pub enum PlaybackPlatform {
    Android,
    AndroidTv,
    Desktop,
    Web,
    Tizen,
    Webos,
    Roku,
    Vizio,
}
/// Support advertisement only; negotiation and qualification remain observed facts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[facet(rename_all = "camelCase")]
pub struct NativeTorrentCapability {
    pub version: u32,
    pub network_policy: String,
}

fn native_capability<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<NativeTorrentCapability>, D::Error> {
    NativeTorrentCapability::deserialize(d).map(Some)
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[facet(rename_all = "camelCase")]
pub struct PlaybackClient {
    pub platform: PlaybackPlatform,
    #[serde(default)]
    pub can_play_direct: bool,
    pub max_width: u32,
    pub max_height: u32,
    pub video_codecs: Vec<String>,
    pub audio_codecs: Vec<String>,
    #[serde(
        default,
        deserialize_with = "native_capability",
        skip_serializing_if = "Option::is_none"
    )]
    pub native_torrent: Option<NativeTorrentCapability>,
}
#[derive(Clone, Debug, Serialize, Deserialize, Facet, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[facet(rename_all = "camelCase")]
pub struct PlaybackV2Request {
    #[serde(default)]
    pub conversion: PlaybackConversion,
    pub request_id: String,
    pub stream_id: String,
    pub client: PlaybackClient,
    #[serde(default)]
    pub position: f64,
    #[serde(default)]
    pub force_gateway: bool,
    pub audio_track: Option<u32>,
    pub subtitle_track: Option<u32>,
    pub audio_language: Option<String>,
    pub preferred_audio_language: Option<String>,
    pub preferred_subtitle_language: Option<String>,
    #[serde(default)]
    pub subtitles_off: bool,
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, Facet, PartialEq)]
#[serde(rename_all = "snake_case")]
#[facet(rename_all = "snake_case")]
#[repr(C)]
pub enum PlaybackConversion {
    #[default]
    Auto,
    Audio,
    Video,
    AudioVideo,
}
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct PlaybackAuthorization {
    pub cookie: Option<String>,
    pub user_agent: Option<String>,
    pub headers: Option<BTreeMap<String, String>>,
}
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct DiscoverPage {
    pub items: Vec<MediaItem>,
    pub unsupported_count: Option<u32>,
    pub has_more: bool,
    pub next_skip: Option<f64>,
}

/// Raw provider order; no synchronous totals or client-side playlist index.
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct LiveCatalogPage {
    pub catalog_id: Option<String>,
    pub generation: Option<String>,
    pub items: Vec<MediaItem>,
    pub next_cursor: Option<String>,
    pub previous_cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct LiveCatalogCategory {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct LiveCatalogCategories {
    pub catalog_id: Option<String>,
    pub generation: Option<String>,
    pub items: Vec<LiveCatalogCategory>,
    pub next_cursor: Option<String>,
    pub previous_cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct MediaPresentation {
    pub hero_image: Option<String>,
    pub poster_image: Option<String>,
    pub episode_image: Option<String>,
    pub title_logo: Option<String>,
    pub title: String,
    pub episode_label: String,
    pub progress: f64,
    pub primary_action: String,
    pub primary_action_label: String,
    pub resume_eligible: bool,
    pub can_auto_next: bool,
}
/// Complete shelf-card projection. Shells render these fields without selecting
/// artwork, interpreting progress, or deciding continuation intent.
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct CardPresentation {
    pub image: Option<String>,
    pub image_role: String,
    pub title: String,
    pub subtitle: String,
    pub progress: Option<f64>,
    pub primary_action: String,
    pub primary_action_label: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
pub struct ApiRequest {
    pub method: String,
    pub path: String,
    pub body: Option<JsonObject>,
}

/// Safe source labels shared by native and web renderers, separate from source identity.
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct SourcePresentation {
    pub title: String,
    pub body: String,
    pub provider_key: String,
    pub provider_label: String,
}

/// Home actions are semantic intents; shells execute navigation and player effects.
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct HomeActions {
    pub can_manage: bool,
    pub manage_previous: bool,
    pub can_resume: bool,
    pub has_resolved_next: bool,
    pub opens_queue_manage: bool,
    pub opens_sources_from_hero: bool,
    pub card_primary_action: String,
    pub hero_primary_action: String,
    pub hero_primary_action_label: String,
    pub show_hero_progress: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct EpisodeWatching {
    pub watching: bool,
    pub progress: f64,
}

/// Phone copy only; geometry, artwork and focus remain renderer responsibilities.
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct PhonePresentation {
    pub shelf_heading: String,
    pub card_context: String,
    pub content_type_label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct SourceRank {
    pub rank: f64,
    pub likely: bool,
    pub best: bool,
}

/// One rank per input source and a stable display order; never a playback choice.
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct SourceRanks {
    pub ranks: Vec<SourceRank>,
    pub ordered_indices: Vec<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct SourceProducerOutcome {
    pub source_id: String,
    pub label: String,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
#[serde(rename_all = "camelCase")]
#[facet(rename_all = "camelCase")]
pub struct DiscoverPolicyProjection {
    pub group: String,
    pub group_label: String,
    pub first_catalog_index: Option<u32>,
    pub defaults: BTreeMap<String, String>,
}
