use super::{
    DeviceCapability, ProfilePreferences, RankingContext, fails_hard_filter, same_language,
};
use crate::{
    Error,
    stremio::{SourceCandidate, source_audio_languages},
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Durable observations from a successful manual pick. No expiring IDs/URLs.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RememberedSource {
    pub provider_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_name: Option<String>,
    #[serde(default)]
    pub audio_languages: Vec<String>,
}
impl RememberedSource {
    pub fn validate(&self) -> Result<(), Error> {
        let text = |s: &str| {
            !s.trim().is_empty()
                && s.len() <= 256
                && !s.chars().any(char::is_control)
                && !s.contains("://")
        };
        if !text(&self.provider_id)
            || self.provider_name.as_deref().is_some_and(|s| !text(s))
            || self.audio_languages.len() > 8
            || self.audio_languages.iter().any(|s| {
                s.len() > 32
                    || source_audio_languages(&serde_json::json!({"audio_languages":[s]})).len()
                        != 1
            })
        {
            return Err(Error::InvalidQuery);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReusableSources {
    pub ids: Vec<String>,
    pub preferred: bool,
    pub fully_matches: bool,
}

/// A title's explicit choice is never replaced by a cross-title preference.
pub fn reusable_source_context(
    rows: &[SourceCandidate],
    title: Option<&RememberedSource>,
    across_titles: Option<&RememberedSource>,
    preferences: &ProfilePreferences,
    context: &RankingContext,
) -> Result<ReusableSources, Error> {
    match title.or(across_titles) {
        Some(memory) => reusable_sources(rows, memory, preferences, context),
        None => Ok(ReusableSources::default()),
    }
}

/// Existing ranked order is authoritative; eligibility does not follow UI filters.
pub fn automatic_source_ids(rows: &[SourceCandidate], device: &DeviceCapability) -> Vec<String> {
    rows.iter()
        .filter(|row| !row.id.is_empty() && !fails_hard_filter(row, device))
        .map(|row| row.id.clone())
        .collect()
}

/// Prefer the selected add-on/supplier, then any row in the remembered language.
/// Unknown/different audio never substitutes for a known remembered language.
pub fn reusable_sources(
    rows: &[SourceCandidate],
    memory: &RememberedSource,
    preferences: &ProfilePreferences,
    context: &RankingContext,
) -> Result<ReusableSources, Error> {
    memory.validate()?;
    if !preferences.remembered_source_reuse {
        return Ok(ReusableSources::default());
    }
    let languages =
        source_audio_languages(&serde_json::json!({"audio_languages":memory.audio_languages}));
    let eligible = rows
        .iter()
        .filter(|row| !row.id.is_empty() && !fails_hard_filter(row, &context.device))
        .filter(|row| {
            languages.is_empty() || {
                let audio = source_audio_languages(
                    &serde_json::json!({"audio_languages":row.audio_languages}),
                );
                audio
                    .iter()
                    .any(|a| languages.iter().any(|b| same_language(a, b)))
            }
        })
        .collect::<Vec<_>>();
    let preferred = |row: &SourceCandidate| {
        row.provider_id == memory.provider_id
            && memory.provider_name.as_deref().is_none_or(|wanted| {
                row.presentation
                    .as_ref()
                    .and_then(|p| p.provider_name.as_deref())
                    .is_some_and(|name| name.eq_ignore_ascii_case(wanted))
            })
    };
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for tier in 0..3 {
        for row in &eligible {
            let matches = match tier {
                0 => preferred(row),
                1 => row.provider_id == memory.provider_id,
                _ => !languages.is_empty(),
            };
            if matches && seen.insert(row.id.as_str()) {
                ids.push(row.id.clone());
            }
        }
    }
    let cap = match (preferences.max_quality_height, context.device.max_height) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    let first = ids
        .first()
        .and_then(|id| rows.iter().find(|row| row.id == *id));
    Ok(ReusableSources {
        preferred: first.is_some_and(preferred),
        fully_matches: first.is_some_and(|row| {
            super::best_match_fully_matches(
                row,
                preferences,
                context.original_language.as_deref(),
                cap,
            )
        }),
        ids,
    })
}
