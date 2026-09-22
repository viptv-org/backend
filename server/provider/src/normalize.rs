//! Pure Xtream-provider normalization and validation primitives.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::Value;
use unicode_normalization::{char::is_combining_mark, UnicodeNormalization};
use url::Url;

pub const MAX_ITEMS: usize = 300_000;

pub fn validate_url(raw: &str) -> Result<Url, String> {
    let u = Url::parse(raw).map_err(|_| "Invalid HTTP URL".to_string())?;
    if !matches!(u.scheme(), "http" | "https")
        || u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.fragment().is_some()
    {
        return Err("Only HTTP(S) URLs without userinfo or fragments are supported".into());
    }
    Ok(u)
}

pub fn optional_bool(value: &Value, key: &str) -> Result<Option<bool>, String> {
    value
        .get(key)
        .map(|v| v.as_bool().ok_or_else(|| format!("{key} must be boolean")))
        .transpose()
}

pub fn action_kind(action: &str) -> &'static str {
    match action {
        "get_vod_streams" | "get_vod_info" => "movie",
        "get_series" | "get_series_info" => "series",
        _ => "live",
    }
}

pub fn required_string(value: &Value, key: &str, max: usize) -> Result<String, String> {
    let s = value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Missing {key}"))?;
    if s.trim().is_empty() || s.len() > max || s.chars().any(char::is_control) {
        return Err(format!("Invalid {key}"));
    }
    Ok(s.to_owned())
}

pub fn base_url(raw: &str) -> Result<Url, String> {
    // Validation errors are deliberately redacted, too: user input can contain secrets.
    let mut url = validate_url(raw).map_err(|_| "Invalid provider HTTP(S) URL".to_string())?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Provider URL must not contain credentials, a query, or a fragment".into());
    }
    let path = url
        .path()
        .trim_end_matches('/')
        .trim_end_matches("/player_api.php")
        .to_owned();
    url.set_path(&format!("{path}/"));
    Ok(url)
}

pub fn endpoint(base: &str, endpoint: &str) -> Result<Url, String> {
    base_url(base)?
        .join(endpoint)
        .map_err(|_| "Invalid provider URL".into())
}

pub fn media_url(
    base: &str,
    username: &str,
    password: &str,
    kind: &str,
    id: &str,
    ext: &str,
) -> Result<String, String> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return Err("Invalid provider stream ID".into());
    }
    let mut url = base_url(base)?;
    {
        let mut path = url
            .path_segments_mut()
            .map_err(|_| "Invalid provider URL".to_string())?;
        path.pop_if_empty()
            .push(kind)
            .push(username)
            .push(password)
            .push(&format!("{id}.{}", extension(Some(ext))));
    }
    Ok(url.to_string())
}

pub fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}
pub fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_owned()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}
pub fn stream_id(value: &Value, key: &str) -> Option<String> {
    scalar(value.get(key)?)
        .filter(|s| !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_digit()))
}
pub fn array(value: &Value) -> Result<&Vec<Value>, String> {
    let result = value
        .as_array()
        .ok_or("Provider returned an invalid index (check credentials)")?;
    if result.len() > MAX_ITEMS {
        return Err("Provider index exceeds the item limit".into());
    }
    Ok(result)
}
pub fn episode_extension(value: Option<&Value>) -> String {
    // Xtream episodes without a declared container use the transport route. This
    // does not change the trusted series kind or infer the media's actual container.
    match value {
        None | Some(Value::Null) => "ts".into(),
        Some(Value::String(value)) if value.trim().is_empty() => "ts".into(),
        _ => extension(value.and_then(Value::as_str)),
    }
}

pub fn extension(value: Option<&str>) -> String {
    let ext = value
        .unwrap_or("mp4")
        .trim()
        .trim_start_matches('.')
        .to_ascii_lowercase();
    if !ext.is_empty() && ext.len() <= 10 && ext.bytes().all(|b| b.is_ascii_alphanumeric()) {
        ext
    } else {
        "mp4".into()
    }
}
pub fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}
pub fn timestamp(v: &Value) -> Option<i64> {
    scalar(v)?.parse::<i64>().ok().filter(|n| *n >= 0)
}
pub fn decode_epg(v: Option<&Value>) -> String {
    let s = v.and_then(Value::as_str).unwrap_or_default();
    STANDARD
        .decode(s)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .filter(|s| {
            !s.chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t' && c != '\r')
        })
        .unwrap_or_else(|| s.to_owned())
}
pub fn valid_year(v: &Value) -> Option<i64> {
    let s = scalar(v)?;
    let year: i64 = s.get(..4)?.parse().ok()?;
    // Only a standalone year or a recognizable release date, never arbitrary digits.
    if s.len() != 4 && !s.get(4..5).is_some_and(|c| c == "-") {
        return None;
    }
    (1870..=2200).contains(&year).then_some(year)
}

/// Remove only a trailing year. Do not guess away language, resolution, or editions.
pub fn title_year(name: &str) -> (String, Option<i64>) {
    let name = name.trim();
    for (open, close) in [('(', ')'), ('[', ']')] {
        if name.ends_with(close) {
            if let Some(start) = name.rfind(open) {
                let year_text = &name[start + 1..name.len() - 1];
                if year_text.len() == 4 {
                    if let Some(year) = valid_year(&Value::String(year_text.to_string())) {
                        return (name[..start].trim().to_owned(), Some(year));
                    }
                }
            }
        }
    }
    if let Some((title, last)) = name.rsplit_once(' ') {
        if !title.trim().is_empty() && last.len() == 4 {
            if let Some(year) = valid_year(&Value::String(last.to_owned())) {
                return (title.trim().to_owned(), Some(year));
            }
        }
    }
    (name.to_owned(), None)
}
pub fn normalize(name: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for c in name
        .nfkd()
        .filter(|c| !is_combining_mark(*c))
        .flat_map(char::to_lowercase)
    {
        if c.is_alphanumeric() {
            if space && !out.is_empty() {
                out.push(' ');
            }
            out.push(c);
            space = false;
        } else {
            space = true;
        }
    }
    out
}
pub fn imdb_id(v: Option<&Value>) -> Option<String> {
    let s = scalar(v?)?.to_ascii_lowercase();
    let digits = s.strip_prefix("imdb:").unwrap_or(&s).strip_prefix("tt")?;
    if (5..=12).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit()) {
        Some(format!("tt{digits}"))
    } else {
        None
    }
}
pub fn tmdb_id(v: Option<&Value>) -> Option<String> {
    let s = scalar(v?)?;
    let s = s.strip_prefix("tmdb:").unwrap_or(&s);
    if s.is_empty() || s.len() > 12 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let number = s.parse::<u64>().ok()?;
    (number > 0).then(|| format!("tmdb:{number}"))
}
pub fn canonical_id(s: &str) -> String {
    let value = Value::String(s.trim().to_owned());
    imdb_id(Some(&value))
        .or_else(|| {
            s.trim()
                .starts_with("tmdb:")
                .then(|| tmdb_id(Some(&value)))
                .flatten()
        })
        .unwrap_or_else(|| s.trim().to_owned())
}
