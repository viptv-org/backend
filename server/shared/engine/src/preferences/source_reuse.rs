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
    pub source_id: Option<String>,
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
            || self.source_id.as_deref().is_some_and(|s| !text(s))
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
    pub ready: bool,
}

/// A title's explicit choice is never replaced by a cross-title preference.
pub fn reusable_source_context(
    rows: &[SourceCandidate],
    title: Option<&RememberedSource>,
    across_titles: Option<&RememberedSource>,
    preferences: &ProfilePreferences,
    context: &RankingContext,
) -> Result<ReusableSources, Error> {
    match preferred_source_memory(title, across_titles, preferences)? {
        Some(memory) => reusable_sources(rows, &memory, preferences, context),
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
    let Some(effective) = preferred_source_memory(Some(memory), None, preferences)? else {
        return Ok(ReusableSources::default());
    };
    let memory = &effective;
    let languages =
        source_audio_languages(&serde_json::json!({"audio_languages":memory.audio_languages}));
    let cap = match (preferences.max_quality_height, context.device.max_height) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
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
            && memory.source_id.as_deref().is_none_or(|wanted| {
                row.presentation
                    .as_ref()
                    .and_then(|p| p.source_id.as_deref())
                    == Some(wanted)
            })
            && (memory.source_id.is_some()
                || memory.provider_name.as_deref().is_none_or(|wanted| {
                    row.presentation
                        .as_ref()
                        .and_then(|p| p.provider_name.as_deref())
                        .is_some_and(|name| name.eq_ignore_ascii_case(wanted))
                }))
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
    let first = ids
        .first()
        .and_then(|id| rows.iter().find(|row| row.id == *id));
    Ok(ReusableSources {
        preferred: first.is_some_and(preferred),
        ready: first.is_some(),
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

pub const PREFERRED_SOURCE_BUDGET_MS: u64 = 5000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreferredSourceDecision {
    Wait,
    Start,
    Fallback,
    Cancel,
}

pub fn preferred_source_decision(
    ready: bool,
    finished: bool,
    elapsed_ms: u64,
    cancelled: bool,
) -> PreferredSourceDecision {
    if cancelled {
        PreferredSourceDecision::Cancel
    } else if elapsed_ms >= PREFERRED_SOURCE_BUDGET_MS {
        PreferredSourceDecision::Fallback
    } else if ready {
        PreferredSourceDecision::Start
    } else if finished {
        PreferredSourceDecision::Fallback
    } else {
        PreferredSourceDecision::Wait
    }
}

pub fn preferred_source_memory(
    title: Option<&RememberedSource>,
    across: Option<&RememberedSource>,
    preferences: &ProfilePreferences,
) -> Result<Option<RememberedSource>, Error> {
    let Some(memory) = title.or(across) else {
        return Ok(None);
    };
    memory.validate()?;
    if !preferences.remembered_source_reuse {
        return Ok(None);
    }
    let mut memory = memory.clone();
    memory.audio_languages =
        source_audio_languages(&serde_json::json!({"audio_languages":memory.audio_languages}));
    if let Some(language) = &preferences.preferred_audio_language {
        memory.audio_languages =
            source_audio_languages(&serde_json::json!({"audio_languages":[language]}));
        if memory.audio_languages.is_empty() {
            return Ok(None);
        }
    }
    Ok(Some(memory))
}

pub fn preferred_supplier_id(
    memory: &RememberedSource,
    enabled: &[(String, String)],
) -> Option<String> {
    if let Some(id) = &memory.source_id {
        return enabled
            .iter()
            .find(|(known, _)| known == id)
            .map(|(id, _)| id.clone());
    }
    let label = memory.provider_name.as_deref()?;
    let mut matches = enabled
        .iter()
        .filter(|(_, name)| name.eq_ignore_ascii_case(label));
    let first = matches.next()?;
    matches.next().is_none().then(|| first.0.clone())
}

pub fn reuse_audio_language(
    candidate: &SourceCandidate,
    memory: Option<&RememberedSource>,
    preferences: &ProfilePreferences,
) -> Result<Option<String>, Error> {
    let Some(memory) = preferred_source_memory(memory, None, preferences)? else {
        return Ok(None);
    };
    let available =
        source_audio_languages(&serde_json::json!({"audio_languages":candidate.audio_languages}));
    Ok(memory.audio_languages.iter().find_map(|wanted| {
        available
            .iter()
            .find(|actual| same_language(actual, wanted))
            .cloned()
    }))
}
