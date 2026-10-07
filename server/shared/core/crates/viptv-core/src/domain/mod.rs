//! Backend wire normalization. Unknown/add-on data never gains transport authority.
use crate::CoreError;
use serde_json::{Map, Value, json};

mod api_error;
mod identity;
mod live_v2;
mod media;
mod playback;
pub(crate) mod playback_protocol;
mod streams;

pub use api_error::api_error;
pub use identity::{clean, identity, profile, tokens};
pub use media::{catalog, media};
pub use playback::playback;

type Result<T> = std::result::Result<T, CoreError>;

pub(super) fn timestamp(v: &Value) -> Option<f64> {
    v.as_f64()
        .map(|n| if n < 1e12 { n * 1000.0 } else { n })
        .or_else(|| {
            v.as_str()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.timestamp_millis() as f64)
        })
}
pub(super) fn invalid() -> CoreError {
    CoreError::InvalidInput
}
pub(super) fn obj(v: &Value) -> Result<&Map<String, Value>> {
    v.as_object().ok_or_else(invalid)
}
pub(super) fn string(v: &Value, key: &str) -> Result<String> {
    v[key]
        .as_str()
        .filter(|x| !x.is_empty())
        .map(str::to_owned)
        .ok_or_else(invalid)
}
pub(super) fn id(v: &Value, key: &str) -> Result<String> {
    if let Some(s) = v[key].as_str().filter(|s| !s.is_empty()) {
        return Ok(s.into());
    }
    if let Some(n) = v[key]
        .as_i64()
        .filter(|n| n.unsigned_abs() <= 9_007_199_254_740_991)
    {
        return Ok(n.to_string());
    }
    Err(invalid())
}
pub(super) fn boolean(v: &Value, key: &str) -> Result<bool> {
    v[key].as_bool().ok_or_else(invalid)
}
pub(super) fn number(v: &Value, key: &str) -> Result<f64> {
    v[key].as_f64().ok_or_else(invalid)
}
fn array<'a>(v: &'a Value, key: &str) -> Result<&'a Vec<Value>> {
    v[key].as_array().ok_or_else(invalid)
}
pub(super) fn strings(v: &Value, key: &str) -> Vec<String> {
    v[key]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}
pub(super) fn copy(v: &Value, out: &mut Value, from: &str, to: &str) {
    if !v[from].is_null() {
        out[to] = v[from].clone();
    }
}
pub(super) fn optional_string(v: &Value, out: &mut Value, from: &str, to: &str) {
    if v[from].is_string() {
        out[to] = v[from].clone();
    }
}
pub(super) fn optional_number(v: &Value, out: &mut Value, from: &str, to: &str) {
    if v[from].is_number() {
        out[to] = v[from].clone();
    }
}
pub(super) fn kind(v: &Value) -> Result<&str> {
    v.as_str()
        .filter(|s| matches!(*s, "movie" | "series" | "live" | "episode"))
        .ok_or_else(invalid)
}
pub(super) fn fallback(v: &Value, keys: &[&str], default: &str) -> String {
    keys.iter()
        .find_map(|k| v[*k].as_str())
        .unwrap_or(default)
        .into()
}
// BrightScript limits labels per function; keep dispatch outside its caller.
#[cfg_attr(target_os = "wasi", inline(never))]
pub fn normalize_value(kind_name: &str, v: &Value, origin: &str) -> Result<Value> {
    match kind_name {
        "apiError" => Ok(api_error(v)),
        "pairing" => streams::pairing(v),
        "androidPreferences" => streams::android_preferences(v, origin),
        "preferences" => streams::preferences(v),
        "page" => streams::page(v),
        "discoverResponse" => streams::discover_response(v),
        "detailResponse" => streams::detail_response(v),
        "streamPoll" => streams::stream_poll(v),
        "sourcesPollStep" => streams::sources_poll_step(v),
        "live" => streams::live(v),
        "liveCategories" => streams::live_categories(v),
        "liveCatalogV2" => live_v2::page(v, false),
        "liveCategoriesV2" => live_v2::page(v, true),
        "liveSourceV2" => live_v2::source(v),
        "guide" => streams::guide(v),
        "profile" => profile(v),
        "identity" => identity(v),
        "tokens" => tokens(v),
        "catalog" => catalog(v),
        "media" => media(v),
        "source" => playback::source(v),
        "mediaArray" => streams::media_array(v),
        "preferencesRequest" => crate::policy::normalize(kind_name, v),
        "playback" => playback(v, origin),
        "playbackV2" => playback::playback_v2(v, origin),
        "clean" => Ok(clean(v)),
        "catalogs" => streams::catalogs(v),
        "discover" => streams::discover(v),
        "container" => streams::container(v),
        _ => crate::policy::normalize(kind_name, v),
    }
}

// The string bridge owns its parsed input. Retain sanitized catalog metadata in
// place instead of allocating a second tree solely to drop the first one.
#[cfg_attr(target_os = "wasi", inline(never))]
pub(crate) fn normalize_owned(kind_name: &str, mut v: Value, origin: &str) -> Result<Value> {
    match kind_name {
        "clean" => {
            identity::clean_in_place(&mut v);
            Ok(v)
        }
        "catalog" => media::catalog_owned(v),
        "catalogs" => match v {
            Value::Array(items) => Ok(Value::Array(
                items
                    .into_iter()
                    .filter_map(|v| media::catalog_owned(v).ok())
                    .collect(),
            )),
            _ => Err(invalid()),
        },
        _ => normalize_value(kind_name, &v, origin),
    }
}
