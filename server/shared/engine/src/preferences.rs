use crate::Error;
use serde::{Deserialize, Serialize};
mod source_reuse;
pub use source_reuse::{RememberedSource, ReusableSources, automatic_source_ids, reusable_sources, reusable_source_context, preferred_source_memory, reuse_audio_language, preferred_supplier_id, preferred_source_decision, PreferredSourceDecision, PREFERRED_SOURCE_BUDGET_MS};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SourceSelection {
    Ask,
    Best,
}
/// `foreign` shows subtitles only when the audio is not the preferred language.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SubtitleMode {
    Always,
    Foreign,
    Never,
}
/// High dynamic range ordering; `any` leaves ranking unchanged.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HdrPreference {
    #[default]
    Any,
    Prefer,
    Avoid,
}
/// UI text scale. `default` is the designed size; small/large are fixed steps.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TextSize {
    Small,
    #[default]
    Default,
    Large,
}
/// `follow` defers to the device's own reduced-motion setting.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReduceMotion {
    #[default]
    Follow,
    On,
    Off,
}
/// Profile-scoped display settings: accent, ratings, type scale, motion,
/// screen saver and overscan inset. These are presentation facts only;
/// playback admission never reads them.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DisplayPreferences {
    /// One of the four palette hexes projected in the controls inventory.
    pub accent_color: String,
    pub show_ratings: bool,
    pub text_size: TextSize,
    pub reduce_motion: ReduceMotion,
    /// 0 disables the screen saver; otherwise idle minutes before it starts.
    pub screen_saver_minutes: u32,
    /// Overscan compensation, percent of the viewport inset on each edge.
    pub screen_inset: u8,
}
impl Default for DisplayPreferences {
    fn default() -> Self {
        Self {
            accent_color: "#F5C542".into(),
            show_ratings: true,
            text_size: TextSize::Default,
            reduce_motion: ReduceMotion::Follow,
            screen_saver_minutes: 5,
            screen_inset: 0,
        }
    }
}
/// The designed accent palette; the controls inventory projects the same set.
pub const ACCENT_COLORS: [&str; 4] = ["#F5C542", "#FF8B5C", "#62D9BC", "#A3BCFF"];
/// Screen-saver choices projected in the controls inventory, in minutes.
pub const SCREEN_SAVER_MINUTES: [u32; 4] = [0, 2, 5, 10];
pub const MAX_SCREEN_INSET: u8 = 20;
impl DisplayPreferences {
    pub fn validate(&self) -> Result<(), Error> {
        let accent = self.accent_color.to_ascii_uppercase();
        if !ACCENT_COLORS.contains(&accent.as_str()) {
            return Err(Error::InvalidQuery);
        }
        if !SCREEN_SAVER_MINUTES.contains(&self.screen_saver_minutes)
            || self.screen_inset > MAX_SCREEN_INSET
        {
            return Err(Error::InvalidQuery);
        }
        Ok(())
    }
    /// Stored form keeps the palette's canonical casing.
    pub fn normalized(mut self) -> Self {
        self.accent_color = self.accent_color.to_ascii_uppercase();
        self
    }
}
/// `subtitles_enabled == (subtitle_mode != never)` always holds after reading.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(from = "StoredPreferences")]
pub struct ProfilePreferences {
    pub preferred_audio_language: Option<String>,
    pub preferred_subtitle_language: Option<String>,
    pub subtitles_enabled: bool,
    pub subtitle_mode: SubtitleMode,
    pub source_selection: SourceSelection,
    pub max_quality_height: Option<u32>,
    pub subtitle_font_scale: f64,
    pub subtitle_text_color: String,
    pub subtitle_background_color: String,
    pub hdr_preference: HdrPreference,
    /// Show the Next episode action during credits; never auto-plays.
    pub next_episode_button: bool,
    /// Skip marked intros/recaps when the content provides markers.
    pub skip_intros: bool,
    /// Switch the display to the video frame rate where the platform allows it.
    pub match_frame_rate: bool,
    /// Reuse the source the viewer last picked for a title; off shows the picker.
    pub remembered_source_reuse: bool,
}
/// Wire/stored form: rows saved before `subtitle_mode` derive it from
/// `subtitles_enabled`; a sent `subtitle_mode` decides `subtitles_enabled`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredPreferences {
    preferred_audio_language: Option<String>,
    preferred_subtitle_language: Option<String>,
    subtitles_enabled: bool,
    #[serde(default)]
    subtitle_mode: Option<SubtitleMode>,
    source_selection: SourceSelection,
    max_quality_height: Option<u32>,
    #[serde(default = "default_scale")]
    subtitle_font_scale: f64,
    #[serde(default = "default_text")]
    subtitle_text_color: String,
    #[serde(default = "default_background")]
    subtitle_background_color: String,
    #[serde(default)]
    hdr_preference: HdrPreference,
    #[serde(default = "default_next_episode_button")]
    next_episode_button: bool,
    #[serde(default)]
    skip_intros: bool,
    #[serde(default = "default_match_frame_rate")]
    match_frame_rate: bool,
    #[serde(default = "default_remembered_source_reuse")]
    remembered_source_reuse: bool,
}
impl From<StoredPreferences> for ProfilePreferences {
    fn from(value: StoredPreferences) -> Self {
        let subtitle_mode = value.subtitle_mode.unwrap_or(if value.subtitles_enabled {
            SubtitleMode::Always
        } else {
            SubtitleMode::Never
        });
        Self {
            preferred_audio_language: value.preferred_audio_language,
            preferred_subtitle_language: value.preferred_subtitle_language,
            subtitles_enabled: subtitle_mode != SubtitleMode::Never,
            subtitle_mode,
            source_selection: value.source_selection,
            max_quality_height: value.max_quality_height,
            subtitle_font_scale: value.subtitle_font_scale,
            subtitle_text_color: value.subtitle_text_color,
            subtitle_background_color: value.subtitle_background_color,
            hdr_preference: value.hdr_preference,
            next_episode_button: value.next_episode_button,
            skip_intros: value.skip_intros,
            match_frame_rate: value.match_frame_rate,
            remembered_source_reuse: value.remembered_source_reuse,
        }
    }
}
impl Default for ProfilePreferences {
    fn default() -> Self {
        Self {
            preferred_audio_language: None,
            preferred_subtitle_language: None,
            subtitles_enabled: true,
            subtitle_mode: SubtitleMode::Always,
            source_selection: SourceSelection::Ask,
            max_quality_height: None,
            subtitle_font_scale: 1.0,
            subtitle_text_color: default_text(),
            subtitle_background_color: default_background(),
            hdr_preference: HdrPreference::Any,
            next_episode_button: true,
            skip_intros: false,
            match_frame_rate: true,
            remembered_source_reuse: true,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreferencesSnapshot {
    pub revision: u64,
    pub preferences: ProfilePreferences,
    #[serde(default)]
    pub display: DisplayPreferences,
}
pub fn language_tag(value: &str) -> bool {
    value.len() <= 32
        && !value.is_empty()
        && value.split('-').enumerate().all(|(i, part)| {
            !part.is_empty()
                && part.len() <= 8
                && part.bytes().all(|b| {
                    if i == 0 {
                        b.is_ascii_alphabetic()
                    } else {
                        b.is_ascii_alphanumeric()
                    }
                })
        })
        && (2..=3).contains(&value.split('-').next().unwrap_or_default().len())
}
fn default_scale() -> f64 {
    1.0
}
fn default_next_episode_button() -> bool {
    true
}
fn default_match_frame_rate() -> bool {
    true
}
fn default_remembered_source_reuse() -> bool {
    true
}
fn default_text() -> String {
    "#FFFFFF".into()
}
fn default_background() -> String {
    "#00000000".into()
}
fn color(value: &str, alpha: bool) -> bool {
    value.starts_with('#')
        && (value.len() == 7 || (alpha && value.len() == 9))
        && value[1..].bytes().all(|b| b.is_ascii_hexdigit())
}
impl ProfilePreferences {
    /// Parse a full replacement. A client that only knows on/off sends no
    /// `subtitle_mode`; keep an existing `foreign` while subtitles stay on.
    pub fn from_update(
        value: serde_json::Value,
        previous: &ProfilePreferences,
    ) -> Result<Self, Error> {
        let sent_mode = value
            .get("subtitle_mode")
            .is_some_and(|mode| !mode.is_null());
        let mut preferences: Self =
            serde_json::from_value(value).map_err(|_| Error::InvalidQuery)?;
        if !sent_mode
            && preferences.subtitles_enabled
            && previous.subtitle_mode == SubtitleMode::Foreign
        {
            preferences.subtitle_mode = SubtitleMode::Foreign;
        }
        Ok(preferences)
    }
    pub fn validate(&self) -> Result<(), Error> {
        if !self.subtitle_font_scale.is_finite()
            || !(0.5..=2.0).contains(&self.subtitle_font_scale)
            || !color(&self.subtitle_text_color, false)
            || !color(&self.subtitle_background_color, true)
            || self.subtitles_enabled != (self.subtitle_mode != SubtitleMode::Never)
            || self
                .preferred_audio_language
                .iter()
                .chain(self.preferred_subtitle_language.iter())
                .any(|tag| !language_tag(tag))
            || self
                .max_quality_height
                .is_some_and(|height| ![240, 360, 480, 720, 1080, 1440, 2160].contains(&height))
        {
            return Err(Error::InvalidQuery);
        }
        Ok(())
    }
}

fn primary(tag: &str) -> &str {
    tag.split(['-', '_']).next().unwrap_or(tag)
}
fn same_language(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b) || primary(a).eq_ignore_ascii_case(primary(b))
}

/// SRC-BEST-MATCH-001 language tier (1 best). `requested` is the profile's
/// preferred audio (None = original language); `original` is the title's.
pub fn language_tier(
    candidate: &crate::stremio::SourceCandidate,
    requested: Option<&str>,
    original: Option<&str>,
) -> u8 {
    let Some(language) = requested.or(original) else {
        // Original language requested but unknown: no language ranking.
        return 1;
    };
    let requested_original = original.map(|original| same_language(original, language));
    let flag = |name: &str| candidate.audio_flags.iter().any(|f| f == name);
    let dual_multi = flag("dual") || flag("multi");
    let dubbed = flag("dubbed");
    let listed = &candidate.audio_languages;
    if listed.iter().any(|tag| same_language(tag, language)) {
        return 1;
    }
    // Dual/multi audio carries a dub beside the original track, known or not.
    if dual_multi {
        return 2;
    }
    if listed.is_empty() && dubbed {
        return match requested_original {
            Some(false) => 2,
            Some(true) => 4,
            None => 2,
        };
    }
    if listed.is_empty() {
        // Unmarked releases usually carry the original audio.
        return if requested_original == Some(true) {
            1
        } else {
            3
        };
    }
    4
}

const RELEASE_RANK: [(&str, u8); 7] = [
    ("web_dl", 0),
    ("bluray", 0),
    ("webrip", 1),
    ("remux", 3),
    ("hdtv", 4),
    ("dvd", 4),
    ("other_rip", 4),
];

/// One client-reported decode answer. `unknown` leaves ranking unchanged.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DecodeSupport {
    #[default]
    Unknown,
    Supported,
    Unsupported,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct VideoCodecSupport {
    /// `avc`, `hevc`, `av1` or `vp9`; names compare case-insensitively.
    pub codec: String,
    pub support: DecodeSupport,
    /// Highest height the device decodes in hardware; absent is unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_height: Option<u32>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct AudioCodecSupport {
    /// `ac3`, `eac3`, `truehd` or `dts`.
    pub codec: String,
    pub support: DecodeSupport,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct HdrSupport {
    /// `hdr10`, `hdr10_plus`, `hlg` or `dv`.
    pub hdr: String,
    pub support: DecodeSupport,
}

/// What the client can decode and show, reported with every ranking request.
/// Every part defaults to unknown, which ranks exactly as before.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct DeviceCapability {
    /// Display height the client snapped to 2160/1440/1080/720/480.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_height: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub video_codecs: Vec<VideoCodecSupport>,
    #[serde(skip_serializing_if = "is_unknown")]
    pub dolby_vision: DecodeSupport,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub display_hdr: Vec<HdrSupport>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub audio_codecs: Vec<AudioCodecSupport>,
}

/// The last source this profile started for this series, remembered by the
/// add-on's binge group so later episodes can prefer the same release family.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct SeriesMemory {
    pub provider_id: String,
    pub binge_group: String,
}

/// Everything one ranking request carries besides the rows and the profile.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct RankingContext {
    /// The title's original language; absent means unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_language: Option<String>,
    pub device: DeviceCapability,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series_memory: Option<SeriesMemory>,
}

fn is_unknown(support: &DecodeSupport) -> bool {
    *support == DecodeSupport::Unknown
}

const MAX_CAPABILITY_ENTRIES: usize = 8;
const MAX_SERIES_MEMORY_TEXT: usize = 128;

fn capability_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 16
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn memory_text(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= MAX_SERIES_MEMORY_TEXT
        && !value.chars().any(char::is_control)
}

impl DeviceCapability {
    pub fn validate(&self) -> Result<(), Error> {
        let height =
            |value: &Option<u32>| value.is_some_and(|height| !(144..=4320).contains(&height));
        if height(&self.max_height)
            || self.video_codecs.len() > MAX_CAPABILITY_ENTRIES
            || self.display_hdr.len() > MAX_CAPABILITY_ENTRIES
            || self.audio_codecs.len() > MAX_CAPABILITY_ENTRIES
            || self
                .video_codecs
                .iter()
                .any(|entry| !capability_name(&entry.codec) || height(&entry.max_height))
            || self
                .display_hdr
                .iter()
                .any(|entry| !capability_name(&entry.hdr))
            || self
                .audio_codecs
                .iter()
                .any(|entry| !capability_name(&entry.codec))
        {
            return Err(Error::InvalidQuery);
        }
        Ok(())
    }
}

impl RankingContext {
    pub fn for_device_height(device_max_height: Option<u32>) -> Self {
        Self {
            device: DeviceCapability {
                max_height: device_max_height,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    pub fn validate(&self) -> Result<(), Error> {
        self.device.validate()?;
        if self
            .original_language
            .as_deref()
            .is_some_and(|tag| !language_tag(tag))
            || self
                .series_memory
                .as_ref()
                .is_some_and(|memory| {
                    !memory_text(&memory.provider_id) || !memory_text(&memory.binge_group)
                })
        {
            return Err(Error::InvalidQuery);
        }
        Ok(())
    }
}

/// Supported, unknown, risky or unsupported on this device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeClass {
    Supported,
    Unknown,
    Risky,
    Unsupported,
}

/// Risky is a Dolby Vision row with no known HDR10/SDR base layer on a device
/// without a Dolby Vision decoder, or a codec the device only decodes in
/// software at this resolution.
pub fn decode_class(
    candidate: &crate::stremio::SourceCandidate,
    capability: &DeviceCapability,
) -> DecodeClass {
    let declared = candidate.video_codec.as_deref().and_then(|codec| {
        capability
            .video_codecs
            .iter()
            .find(|entry| entry.codec.eq_ignore_ascii_case(codec))
    });
    if declared.is_some_and(|entry| entry.support == DecodeSupport::Unsupported) {
        return DecodeClass::Unsupported;
    }
    let dv_without_base_layer = candidate.dynamic_range.iter().any(|range| range == "dv")
        && !matches!(candidate.dv_base_layer.as_deref(), Some("hdr10" | "sdr"));
    if dv_without_base_layer && capability.dolby_vision == DecodeSupport::Unsupported {
        return DecodeClass::Risky;
    }
    match declared {
        Some(entry) if entry.support == DecodeSupport::Supported => {
            match (candidate.quality_height, entry.max_height) {
                (Some(height), Some(max)) if height > max => DecodeClass::Risky,
                _ => DecodeClass::Supported,
            }
        }
        _ => DecodeClass::Unknown,
    }
}

fn preferred_dynamic_range(
    candidate: &crate::stremio::SourceCandidate,
    capability: &DeviceCapability,
) -> bool {
    candidate.dynamic_range.iter().any(|range| {
        range != "dv" || capability.dolby_vision != DecodeSupport::Unsupported
    })
}

/// The Best matches group is the top-ranked rows that pass every hard filter,
/// at most this many; rows failing a filter sort after every other row.
pub const BEST_MATCHES_LIMIT: usize = 5;

/// The Best matches group alongside the ranked rows, in ranking order.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceRanking {
    pub best_match_ids: Vec<String>,
    pub best_match_id: Option<String>,
    pub best_match_fully_matches: bool,
}

/// Cam, 3D, torrents reporting fewer than five seeders and rows the device
/// cannot decode never join the Best matches group; an unknown seeder count is
/// not filtered.
pub fn fails_hard_filter(
    candidate: &crate::stremio::SourceCandidate,
    capability: &DeviceCapability,
) -> bool {
    candidate.release_type.as_deref() == Some("cam")
        || candidate.three_d
        || (candidate.delivery.as_deref() == Some("torrent")
            && candidate.seeders.is_some_and(|seeders| seeders < 5))
        || decode_class(candidate, capability) == DecodeClass::Unsupported
}

/// Quality and spoken-audio metadata come from plugins and the shared release
/// facts normalizer, never presentation-label parsing at ranking time.
/// `device.max_height` lowers the profile cap to what the device shows.
pub fn rank_sources(
    candidates: &mut [crate::stremio::SourceCandidate],
    preferences: &ProfilePreferences,
    context: &RankingContext,
) -> SourceRanking {
    let capability = &context.device;
    let original_language = context.original_language.as_deref();
    let memory = context.series_memory.as_ref();
    let cap = match (preferences.max_quality_height, capability.max_height) {
        (Some(profile), Some(device)) => Some(profile.min(device)),
        (profile, device) => profile.or(device),
    };
    let requested = preferences.preferred_audio_language.as_deref();
    candidates.sort_by_key(|candidate| {
        let torrent = candidate.delivery.as_deref() == Some("torrent");
        let excluded = fails_hard_filter(candidate, capability);
        let decode = match decode_class(candidate, capability) {
            DecodeClass::Supported => 0,
            DecodeClass::Unknown => 1,
            DecodeClass::Risky => 2,
            DecodeClass::Unsupported => 3,
        };
        let quality_class = match (cap, candidate.quality_height) {
            (Some(cap), Some(height)) if height > cap => 2,
            (_, None) => 1,
            _ => 0,
        };
        let remembered = memory.is_some_and(|memory| {
            candidate.provider_id == memory.provider_id
                && candidate.binge_group.as_deref() == Some(memory.binge_group.as_str())
        });
        let release = candidate.release_type.as_deref().map_or(2, |kind| {
            RELEASE_RANK
                .iter()
                .find(|(known, _)| *known == kind)
                .map_or(2, |(_, rank)| *rank)
        });
        let hdr = match preferences.hdr_preference {
            HdrPreference::Any => 0,
            HdrPreference::Prefer => u8::from(!preferred_dynamic_range(candidate, capability)),
            HdrPreference::Avoid => u8::from(!candidate.dynamic_range.is_empty()),
        };
        let seeders = if torrent {
            (
                candidate.seeders.is_none(),
                std::cmp::Reverse(candidate.seeders.unwrap_or(0)),
            )
        } else {
            (false, std::cmp::Reverse(0))
        };
        (
            excluded,
            decode,
            language_tier(candidate, requested, original_language),
            quality_class,
            !remembered,
            // Highest first within the cap; over the cap, closest to it first.
            if quality_class == 2 {
                i64::from(candidate.quality_height.unwrap_or(0))
            } else {
                -i64::from(candidate.quality_height.unwrap_or(0))
            },
            release,
            hdr,
            torrent,
            seeders,
        )
    });
    let best_match_ids = candidates
        .iter()
        .filter(|candidate| !fails_hard_filter(candidate, capability))
        .take(BEST_MATCHES_LIMIT)
        .map(|candidate| candidate.id.clone())
        .collect::<Vec<_>>();
    let best_match_id = best_match_ids.first().cloned();
    let best_match_fully_matches = candidates
        .iter()
        .find(|candidate| !fails_hard_filter(candidate, capability))
        .is_some_and(|candidate| {
            best_match_fully_matches(candidate, preferences, original_language, cap)
        });
    SourceRanking {
        best_match_ids,
        best_match_id,
        best_match_fully_matches,
    }
}

fn best_match_fully_matches(
    candidate: &crate::stremio::SourceCandidate,
    preferences: &ProfilePreferences,
    original_language: Option<&str>,
    cap: Option<u32>,
) -> bool {
    let quality = match cap {
        None => candidate.quality_height.is_some(),
        Some(cap) => candidate.quality_height == Some(cap),
    };
    let language = match preferences
        .preferred_audio_language
        .as_deref()
        .or(original_language)
    {
        None => true,
        Some(target) => {
            candidate.audio_languages.is_empty()
                || candidate
                    .audio_languages
                    .iter()
                    .any(|tag| same_language(tag, target))
        }
    };
    let has_hdr = !candidate.dynamic_range.is_empty();
    let hdr = match preferences.hdr_preference {
        HdrPreference::Any => true,
        HdrPreference::Prefer => has_hdr,
        HdrPreference::Avoid => !has_hdr,
    };
    let delivered = candidate.delivery.as_deref() != Some("torrent");
    quality && language && hdr && delivered
}
