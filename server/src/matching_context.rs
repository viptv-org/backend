use super::*;

// Additive migration: existing progress rows and old clients remain valid.
pub(crate) fn init_progress_context(db: &Connection) -> rusqlite::Result<()> {
    let mut query = db.prepare("PRAGMA table_info(progress)")?;
    let columns = query
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !columns.iter().any(|column| column == "context") {
        db.execute_batch("ALTER TABLE progress ADD COLUMN context TEXT NOT NULL DEFAULT '{}'")?;
    }
    Ok(())
}

pub(crate) const CONTEXT_FIELDS: [&str; 15] = [
    "series_id",
    "season",
    "episode",
    "year",
    "releaseInfo",
    "imdb_id",
    "tmdb_id",
    "source_addon_id",
    "source_name",
    "source_fingerprint",
    "source_binge_group",
    "source_release_group",
    "source_quality",
    "source_audio",
    "audio_language",
];

pub(crate) fn matching_context(value: &Value) -> Result<Value, ApiError> {
    let mut context = serde_json::Map::new();
    for key in CONTEXT_FIELDS {
        let field = &value[key];
        if field.is_null() {
            continue;
        }
        let invalid = || ApiError::from(format!("Invalid {key}"));
        let clean = match key {
            "season" | "episode" | "year" => {
                let number = field.as_i64().ok_or_else(invalid)?;
                let range = if key == "year" {
                    1870..=2200
                } else {
                    0..=100_000
                };
                if !range.contains(&number) {
                    return Err(invalid());
                }
                json!(number)
            }
            "tmdb_id" => {
                let text = if let Some(text) = field.as_str() {
                    text.trim().to_owned()
                } else if let Some(n) = field.as_u64() {
                    n.to_string()
                } else if let Some(n) = field.as_f64().filter(|n| {
                    n.is_finite() && n.fract() == 0.0 && *n > 0.0 && *n <= 2_147_483_647.0
                }) {
                    (n as u64).to_string()
                } else {
                    return Err(invalid());
                };
                let digits = text.strip_prefix("tmdb:").unwrap_or(&text);
                if digits.is_empty()
                    || digits.len() > 12
                    || !digits.bytes().all(|b| b.is_ascii_digit())
                {
                    return Err(invalid());
                }
                let number = digits.parse::<u64>().map_err(|_| invalid())?;
                if number == 0 || number > 2_147_483_647 {
                    return Err(invalid());
                }
                json!(number.to_string())
            }
            _ => {
                let text = field.as_str().ok_or_else(invalid)?.trim();
                let limit = match key {
                    "releaseInfo" | "source_addon_id" => 128,
                    "source_name" => 256,
                    "source_fingerprint" => 64,
                    _ => 512,
                };
                if text.is_empty() || text.len() > limit || text.chars().any(char::is_control) {
                    return Err(invalid());
                }
                if key == "imdb_id" && text.chars().any(char::is_whitespace) {
                    return Err(invalid());
                }
                if key == "imdb_id" {
                    let text = text.to_ascii_lowercase();
                    let digits = text
                        .strip_prefix("imdb:")
                        .unwrap_or(&text)
                        .strip_prefix("tt")
                        .ok_or_else(invalid)?;
                    if !(5..=12).contains(&digits.len())
                        || !digits.bytes().all(|b| b.is_ascii_digit())
                    {
                        return Err(invalid());
                    }
                    json!(format!("tt{digits}"))
                } else {
                    json!(text)
                }
            }
        };
        context.insert(key.to_owned(), clean);
    }
    Ok(Value::Object(context))
}

pub(crate) fn blank_name(value: &Value) -> bool {
    value["name"]
        .as_str()
        .is_none_or(|name| name.trim().is_empty())
}

pub(crate) fn enrichment_id(value: &Value, kind: &str, id: &str) -> String {
    if kind == "series" {
        if let Some(parent) = value["series_id"].as_str() {
            return parent.to_owned();
        }
        let parts = id.rsplitn(3, ':').collect::<Vec<_>>();
        if parts.len() == 3 && parts[0].parse::<u32>().is_ok() && parts[1].parse::<u32>().is_ok() {
            return parts[2].to_owned();
        }
    }
    id.to_owned()
}

pub(crate) fn release_year(value: &Value) -> Option<Value> {
    let year = value.as_str()?.get(..4)?.parse::<i64>().ok()?;
    (1870..=2200).contains(&year).then(|| json!(year))
}

pub(crate) fn enrich_matching(request: &mut Value, meta: &Value) {
    if blank_name(request) {
        if let Some(name) = meta["name"].as_str().map(str::trim).filter(|name| {
            !name.is_empty() && name.len() <= 512 && !name.chars().any(char::is_control)
        }) {
            request["name"] = json!(name);
        }
    }
    // Supplied context wins. Ignore malformed optional addon fields independently.
    if request["year"].is_null() {
        if let Some(year) = release_year(&request["releaseInfo"]) {
            request["year"] = year;
        }
    }
    for key in ["year", "releaseInfo", "imdb_id", "tmdb_id"] {
        if request[key].is_null() && !meta[key].is_null() {
            if let Ok(clean) = matching_context(&json!({key:meta[key]})) {
                request[key] = clean[key].clone();
            }
        }
    }
    if request["year"].is_null() {
        if let Some(year) = release_year(&request["releaseInfo"]) {
            request["year"] = year;
        }
    }
}

// Summarize newest activity per title before filtering completion. An older
// unfinished episode must not resurrect a show whose newest episode is finished.
pub(crate) fn legacy_series_id(id: &str) -> &str {
    // Only the documented IMDb episode shape is safe to infer. Addon IDs are opaque.
    let parts: Vec<_> = id.split(':').collect();
    if parts.len() == 3
        && parts[0]
            .strip_prefix("tt")
            .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        && parts[1..]
            .iter()
            .all(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
    {
        parts[0]
    } else {
        id
    }
}
