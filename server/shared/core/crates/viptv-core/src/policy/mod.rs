//! Pure cross-platform presentation and request policy. Shells execute effects only.
use crate::CoreError;
use serde_json::{Value, json};

/// Returns a fixed policy pattern compiled once per process (each call site
/// owns its own lazily initialised `Regex`).
macro_rules! policy_regex {
    ($pattern:literal) => {{
        static PATTERN: std::sync::LazyLock<regex::Regex> =
            std::sync::LazyLock::new(|| regex::Regex::new($pattern).expect("policy regex"));
        &*PATTERN
    }};
}

mod catalog;
mod layout;
pub mod playback_control;
mod presentation;
mod progress;
mod requests;
pub mod shell_lifecycle;
pub(crate) use requests::{valid_live_cursor, validate_playback_v2};
mod sources;

type Result = std::result::Result<Value, CoreError>;
fn text<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}
pub(super) fn num(v: &Value, k: &str) -> f64 {
    v[k].as_f64().unwrap_or(0.0)
}
pub(super) fn image(v: &Value, k: &str) -> Value {
    v[k].as_str()
        .filter(|s| !s.trim().is_empty())
        .map_or(Value::Null, |s| json!(s))
}
/// Candidate key for the derived Cinemeta poster.
pub(super) const IMDB_POSTER: &str = "\0imdbPoster";
/// The item's poster, or Cinemeta's poster for its IMDb title when it has
/// none. Only a validated IMDb id (`tt` + 5-12 digits) derives a URL.
pub(super) fn poster_image(v: &Value) -> Value {
    let own = image(v, "poster");
    if !own.is_null() {
        return own;
    }
    ["imdbId", "seriesId", "id"]
        .iter()
        .filter_map(|k| v[*k].as_str())
        .map(|id| id.split(':').next().unwrap_or(""))
        .find(|id| {
            id.strip_prefix("tt").is_some_and(|d| {
                (5..=12).contains(&d.len()) && d.bytes().all(|b| b.is_ascii_digit())
            })
        })
        .map_or(Value::Null, |id| {
            json!(format!(
                "https://images.metahub.space/poster/medium/{id}/img"
            ))
        })
}
pub(super) fn watched(v: &Value) -> bool {
    v["watched"]
        .as_bool()
        .unwrap_or(num(v, "duration") > 0.0 && num(v, "position") / num(v, "duration") >= 0.95)
}
pub(super) fn item_request(v: &Value) -> Value {
    let mut o = json!({});
    for (a, b) in [
        ("id", "id"),
        ("type", "type"),
        ("name", "name"),
        ("poster", "poster"),
        ("year", "year"),
        ("season", "season"),
        ("episode", "episode"),
        ("seriesId", "series_id"),
        ("sourceAddonId", "source_addon_id"),
        ("sourceName", "source_name"),
        ("sourceFingerprint", "source_fingerprint"),
        ("sourceBingeGroup", "source_binge_group"),
        ("sourceReleaseGroup", "source_release_group"),
        ("sourceQuality", "source_quality"),
        ("sourceAudio", "source_audio"),
    ] {
        o[b] = v[a].clone();
    }
    o
}
pub(super) fn snake(v: &Value) -> Value {
    let mut out = json!({});
    if let Some(obj) = v.as_object() {
        for (k, v) in obj {
            let mut key = String::new();
            for c in k.chars() {
                if c.is_uppercase() {
                    key.push('_');
                    key.extend(c.to_lowercase());
                } else {
                    key.push(c);
                }
            }
            out[&key] = if v.is_object() { snake(v) } else { v.clone() };
        }
    }
    out
}
pub(super) fn enc(v: &str) -> String {
    url::form_urlencoded::byte_serialize(v.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}
pub fn normalize(kind: &str, v: &Value) -> Result {
    Ok(match kind {
        "presentation" => presentation::presentation(v)?,
        "cardPresentation" => presentation::card_presentation(v)?,
        "homeActions" => presentation::home_actions(v),
        "episodeWatching" => presentation::episode_watching(v),
        "phonePresentation" => presentation::phone_presentation(v),
        "shellLifecycle" => shell_lifecycle::shell_lifecycle(v)?,
        "playbackControl" => playback_control::normalize(v)?,
        "itemRequest" => item_request(v),
        "playbackRequest" | "preferencesRequest" => snake(v),
        "request" => requests::request(v)?,
        "metadataTargets" => requests::metadata_targets(v),
        "homeLayout" => layout::home_layout(v)?,
        "searchPlan" => layout::search_plan(v)?,
        "playbackV2Intent" => requests::playback_v2_intent(v)?,
        "enrichDetail" => progress::enrich_detail(v),
        "mergeEpisodeProgress" => progress::merge_episode_progress(v)?,
        "initialEpisode" => progress::initial_episode(v)?,
        "exactResume" | "exactResumeSource" => progress::exact_resume(v)?,
        "enrichHome" => progress::enrich_home(v),
        "resume" => progress::resume(v)?,
        "autoNext" => sources::auto_next(v),
        "subtitleSelection" => sources::subtitle_selection(v),
        "sourceMatch" => sources::source_match(v),
        "sourceRanks" => sources::source_ranks(v),
        "sourceProducerLabels" => sources::source_producer_labels(v),
        "sourceDisplay" => sources::source_display(v),
        "sourceIdentity" => sources::source_identity(v),
        "continuationSource" => sources::continuation_source(v)?,
        "catalogFilters" | "catalogDefaults" => catalog::catalog_filters(kind, v),
        "discoverPolicy" => catalog::discover_policy(v),
        "artworkUrl" => catalog::artwork_url(v)?,
        _ => return Err(CoreError::InvalidInput),
    })
}
