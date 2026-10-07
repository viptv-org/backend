use super::*;

fn identifier(v: &Value, positive: bool) -> Result<String> {
    let number = v
        .as_u64()
        .filter(|n| *n <= 9_007_199_254_740_991 && (!positive || *n > 0))
        .ok_or_else(invalid)?;
    Ok(number.to_string())
}

pub(super) fn page(v: &Value, categories: bool) -> Result<Value> {
    let fields = obj(v)?;
    if [
        "catalog_id",
        "generation",
        "items",
        "next_cursor",
        "previous_cursor",
    ]
    .iter()
    .any(|key| !fields.contains_key(*key))
    {
        return Err(invalid());
    }
    let items = array(v, "items")?;
    if items.len() > 200 {
        return Err(invalid());
    }
    let catalog = if v["catalog_id"].is_null() {
        None
    } else {
        Some(identifier(&v["catalog_id"], true)?)
    };
    let generation = if v["generation"].is_null() {
        None
    } else {
        Some(identifier(&v["generation"], false)?)
    };
    let cursor = match &v["next_cursor"] {
        Value::Null => None,
        Value::String(value) if crate::policy::valid_live_cursor(value) => Some(value.clone()),
        _ => return Err(invalid()),
    };
    let previous = match &v["previous_cursor"] {
        Value::Null => None,
        Value::String(value) if crate::policy::valid_live_cursor(value) => Some(value.clone()),
        _ => return Err(invalid()),
    };
    if categories && items.is_empty() && (cursor.is_some() || previous.is_some()) {
        return Err(invalid());
    }
    let mut seen = std::collections::HashSet::new();
    if catalog.is_some() != generation.is_some()
        || (catalog.is_none() && (!items.is_empty() || cursor.is_some() || previous.is_some()))
    {
        return Err(invalid());
    }
    let items = items
        .iter()
        .map(|value| {
            obj(value)?;
            let id = string(value, "id")?;
            let name = string(value, "name")?;
            if id.len() > 256
                || id.chars().any(char::is_control)
                || name.len() > 1024
                || !seen.insert(id.clone())
                || categories && (id.trim().is_empty() || name.trim().is_empty())
            {
                return Err(invalid());
            }
            if categories {
                return Ok(json!({"id":id,"name":name}));
            }
            let mut raw = json!({"id":id,"type":"live","name":name});
            for field in ["logo", "category_id", "category", "epg_channel_id"] {
                match &value[field] {
                    Value::Null => {}
                    Value::String(value) if value.len() <= 4096 => raw[field] = json!(value),
                    _ => return Err(invalid()),
                }
            }
            raw["poster"] = raw["logo"].clone();
            media(&raw)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(
        json!({"catalogId":catalog,"generation":generation,"items":items,"nextCursor":cursor,"previousCursor":previous}),
    )
}

pub(super) fn source(v: &Value) -> Result<Value> {
    let value = &v["source"];
    obj(value)?;
    let id = string(value, "id")?;
    let producer = string(value, "source_addon_id")?;
    let provider = producer
        .strip_prefix("iptv:")
        .and_then(|id| id.parse::<u64>().ok())
        .filter(|id| *id > 0)
        .ok_or_else(invalid)?;
    if id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        || producer != format!("iptv:{provider}")
        || value["source"] != producer
        || ["url", "headers", "authorization", "playback_url"]
            .iter()
            .any(|field| !value[*field].is_null())
    {
        return Err(invalid());
    }
    let mut raw = json!({"id":id,"source":producer,"source_addon_id":producer});
    for (field, bound) in [
        ("name", 256),
        ("source_name", 256),
        ("title", 1024),
        ("description", 2048),
        ("source_fingerprint", 128),
    ] {
        match &value[field] {
            Value::Null => {}
            Value::String(value) if value.len() <= bound => raw[field] = json!(value),
            _ => return Err(invalid()),
        }
    }
    playback::source(&raw)
}
