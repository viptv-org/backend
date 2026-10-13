use crate::Error;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SourceSelection {
    Ask,
    Best,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProfilePreferences {
    pub preferred_audio_language: Option<String>,
    pub preferred_subtitle_language: Option<String>,
    pub subtitles_enabled: bool,
    pub source_selection: SourceSelection,
    pub max_quality_height: Option<u32>,
}
impl Default for ProfilePreferences {
    fn default() -> Self {
        Self {
            preferred_audio_language: None,
            preferred_subtitle_language: None,
            subtitles_enabled: true,
            source_selection: SourceSelection::Ask,
            max_quality_height: None,
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
impl ProfilePreferences {
    pub fn validate(&self) -> Result<(), Error> {
        if self
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
        let quality_class=match (preferences.max_quality_height,candidate.quality_height){(Some(cap),Some(height)) if height>cap=>2,(_,None)=>1,_=>0};
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
