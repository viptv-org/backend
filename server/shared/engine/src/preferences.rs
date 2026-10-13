use crate::Error;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SourceSelection {
    Ask,
    Best,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProfilePreferences {
    pub preferred_audio_language: Option<String>,
    pub preferred_subtitle_language: Option<String>,
    pub subtitles_enabled: bool,
    pub source_selection: SourceSelection,
    pub max_quality_height: Option<u32>,
    #[serde(default = "default_scale")]
    pub subtitle_font_scale: f64,
    #[serde(default = "default_text")]
    pub subtitle_text_color: String,
    #[serde(default = "default_background")]
    pub subtitle_background_color: String,
}
impl Default for ProfilePreferences {
    fn default() -> Self {
        Self {
            preferred_audio_language: None,
            preferred_subtitle_language: None,
            subtitles_enabled: true,
            source_selection: SourceSelection::Ask,
            max_quality_height: None,
            subtitle_font_scale: 1.0,
            subtitle_text_color: default_text(),
            subtitle_background_color: default_background(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreferencesSnapshot {
    pub revision: u64,
    pub preferences: ProfilePreferences,
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
    pub fn validate(&self) -> Result<(), Error> {
        if !self.subtitle_font_scale.is_finite()
            || !(0.5..=2.0).contains(&self.subtitle_font_scale)
            || !color(&self.subtitle_text_color, false)
            || !color(&self.subtitle_background_color, true)
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
/// Quality and spoken-audio metadata come from plugins, never presentation-label parsing.
pub fn rank_sources(
    candidates: &mut [crate::stremio::SourceCandidate],
    preferences: &ProfilePreferences,
) {
    candidates.sort_by_key(|candidate| {
        let quality_class = match (preferences.max_quality_height, candidate.quality_height) {
            (Some(cap), Some(height)) if height > cap => 2,
            (_, None) => 1,
            _ => 0,
        };
        let language = preferences
            .preferred_audio_language
            .as_ref()
            .is_none_or(|tag| {
                candidate.audio_languages.iter().any(|language| {
                    language.eq_ignore_ascii_case(tag)
                        || language.eq_ignore_ascii_case(tag.split('-').next().unwrap_or(tag))
                })
            });
        (
            quality_class,
            !language,
            candidate.quality_height.is_none(),
            std::cmp::Reverse(candidate.quality_height.unwrap_or(0)),
        )
    });
}
