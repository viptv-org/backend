use super::*;

pub fn catalog(v: &Value) -> Result<Value> {
    let mut out = catalog_fields(v)?;
    out["raw"] = clean(v);
    Ok(out)
}

pub(super) fn catalog_owned(mut v: Value) -> Result<Value> {
    let mut out = catalog_fields(&v)?;
    super::identity::clean_in_place(&mut v);
    out["raw"] = v;
    Ok(out)
}

fn catalog_fields(v: &Value) -> Result<Value> {
    let catalog_type = v["type"].as_str().unwrap_or("movie");
    if catalog_type.trim().is_empty() || catalog_type.len() > 64 {
        return Err(invalid());
    }
    // Move owned values into the object. json! borrows and serializes expressions,
    // which would copy strings and vectors before dropping the originals.
    let mut out = Value::Object(Map::from_iter([
        ("id".into(), Value::String(id(v, "id")?)),
        (
            "name".into(),
            Value::String(viptv_simkl::display_text(&fallback(v, &["name", "id"], "Catalog"))),
        ),
        ("type".into(), Value::from(catalog_type)),
        (
            "supportsSearch".into(),
            Value::Bool(v["supports_search"].as_bool().unwrap_or(false)),
        ),
        (
            "supportsSkip".into(),
            Value::Bool(v["supports_skip"].as_bool().unwrap_or(false)),
        ),
        ("genres".into(), Value::from(strings(v, "genres"))),
        ("extras".into(), Value::Array(Vec::new())),
    ]));
    optional_string(v, &mut out, "addon_name", "addonName");
    optional_number(v, &mut out, "addon_id", "addonId");
    if let Ok(key) = id(v, "addon_id") {
        out["addonKey"] = Value::String(key);
    }
    out["extras"] = Value::Array(
        v["extra"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| {
                let name = e["name"].as_str().filter(|s| !s.is_empty())?;
                let mut extra = Value::Object(Map::from_iter([
                    ("name".into(), Value::from(name)),
                    (
                        "required".into(),
                        Value::Bool(e["is_required"].as_bool().unwrap_or(false)),
                    ),
                    ("options".into(), Value::from(strings(e, "options"))),
                ]));
                optional_string(e, &mut extra, "default", "defaultValue");
                optional_number(e, &mut extra, "options_limit", "optionsLimit");
                Some(extra)
            })
            .collect(),
    );
    Ok(out)
}
pub fn media(v: &Value) -> Result<Value> {
    let mut raw = clean(v);
    if let Some(fields) = raw.as_object_mut() {
        // These potentially large fields already have typed representations.
        // Retaining them in raw duplicates entire series catalogs and synopses
        // each time a normalized DTO crosses the bounded native/WASM bridge.
        for key in ["videos", "episodes", "description", "overview"] {
            fields.remove(key);
        }
    }
    let mut out = json!({"id":id(v,"id")?,"type":kind(&v["type"])?,"name":fallback(v,&["name","title"],"Untitled"),"title":fallback(v,&["title","name"],"Untitled"),"genres":strings(v,"genres"),"raw":raw});
    out["name"] = json!(viptv_simkl::display_text(out["name"].as_str().unwrap_or("")));
    out["title"] = json!(viptv_simkl::display_text(out["title"].as_str().unwrap_or("")));
    // Generic provider logos describe title artwork for on-demand media, but
    // live-channel logos are station identity and must not replace title text.
    let logo_keys = [
        "titleLogo",
        "title_logo",
        "clearLogo",
        "clearlogo",
        "clear_logo",
    ];
    let title_logo = logo_keys
        .iter()
        .find_map(|key| v[*key].as_str().filter(|s| !s.trim().is_empty()))
        .or_else(|| {
            (v["type"] != "live")
                .then(|| v["logo"].as_str().filter(|s| !s.trim().is_empty()))
                .flatten()
        });
    if let Some(logo) = title_logo {
        out["titleLogo"] = json!(logo);
    }
    for (from, to) in [
        ("poster", "poster"),
        ("runtime", "runtime"),
        ("series_id", "seriesId"),
        ("queue_status", "queueStatus"),
        ("source_addon_id", "sourceAddonId"),
        ("source_name", "sourceName"),
        ("source_fingerprint", "sourceFingerprint"),
        ("source_binge_group", "sourceBingeGroup"),
        ("source_release_group", "sourceReleaseGroup"),
        ("source_quality", "sourceQuality"),
        ("source_audio", "sourceAudio"),
    ] {
        optional_string(v, &mut out, from, to);
    }
    for (keys, to) in [
        (["background", "backdrop"], "background"),
        (["description", "overview"], "description"),
        (["episodeTitle", "episode_title"], "episodeTitle"),
    ] {
        if let Some(s) = keys.iter().find_map(|k| v[*k].as_str()) {
            out[to] = json!(s);
        }
    }
    for (keys, to) in [
        (vec!["thumbnail", "landscape", "image"], "thumbnail"),
        (vec!["imdbRating", "imdb_rating", "rating"], "imdbRating"),
        (vec!["posterShape", "poster_shape"], "posterShape"),
        (vec!["credits"], "credits"),
    ] {
        if let Some(value) = keys.iter().find_map(|k| {
            v[*k]
                .as_str()
                .map(str::to_owned)
                .or_else(|| v[*k].as_f64().map(|n| n.to_string()))
        }) {
            out[to] = json!(value);
        }
    }
    if out["credits"].is_null() {
        let mut credits = vec![];
        for (key, prefix) in [("director", "Directed by "), ("cast", "Starring ")] {
            let names = v[key].as_str().map(str::to_owned).unwrap_or_else(|| {
                strings(v, key)
                    .into_iter()
                    .take(4)
                    .collect::<Vec<_>>()
                    .join(", ")
            });
            if !names.is_empty() {
                credits.push(format!("{prefix}{names}"));
            }
        }
        if !credits.is_empty() {
            out["credits"] = json!(credits.join("\n"));
        }
    }
    for (keys, to) in [
        (vec!["updated_at", "updatedAt"], "updatedAtMillis"),
        (
            vec!["released", "released_at", "releaseDate"],
            "releasedAtMillis",
        ),
    ] {
        if let Some(n) = keys.iter().find_map(|k| timestamp(&v[*k])) {
            out[to] = json!(n);
        }
    }
    if let Some(year) = v["year"]
        .as_str()
        .or_else(|| v["releaseInfo"].as_str())
        .and_then(|s| s.get(..4))
        .and_then(|s| s.parse::<f64>().ok())
    {
        out["year"] = json!(year);
    }
    out["episodes"] = json!([]);
    if let Some(episodes) = v["videos"].as_array().or_else(|| v["episodes"].as_array()) {
        out["episodes"] = Value::Array(
            episodes
                .iter()
                .filter_map(|ep| {
                    let mut ep = ep.clone();
                    if ep["type"].is_null() {
                        ep["type"] = v["type"].clone();
                    }
                    if ep["series_id"].is_null() {
                        ep["series_id"] = v["id"].clone();
                    }
                    let mut item = media(&ep).ok()?;
                    if item["episodeTitle"].is_null() && item["name"] != out["name"] {
                        item["episodeTitle"] = item["name"].clone();
                    }
                    item["name"] = out["name"].clone();
                    if item["titleLogo"].is_null() && !out["titleLogo"].is_null() {
                        item["titleLogo"] = out["titleLogo"].clone();
                    }
                    Some(item)
                })
                .collect(),
        );
    }
    for key in ["year", "position", "duration", "season", "episode"] {
        optional_number(v, &mut out, key, key);
    }
    if v["watch_state"] == "watched" {
        out["watched"] = json!(true);
    }
    if v["watched"].is_boolean() {
        copy(v, &mut out, "watched", "watched");
    }
    for (snake, camel) in [
        ("resume_active", "resumeActive"),
        ("watch_date_known", "watchDateKnown"),
        ("completion_only", "completionOnly"),
    ] {
        if let Some(value) = v[snake].as_bool().or_else(|| v[camel].as_bool()) {
            out[camel] = json!(value);
        }
    }
    if v["previous_episode"].is_object() {
        out["previousEpisode"] = media(&v["previous_episode"])?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Keep the old constructor as an independent ownership-optimization oracle.
    // It can be removed (with the comparisons below, replaced by fixed expected
    // JSON) once only one of `catalog`/`catalog_owned` remains, or once the
    // legacy addon catalog shape (`supports_search`, `extra`, `is_required`)
    // is no longer normalized.
    fn legacy_catalog(v: &Value) -> Result<Value> {
        let catalog_type = v["type"].as_str().unwrap_or("movie");
        if catalog_type.trim().is_empty() || catalog_type.len() > 64 {
            return Err(invalid());
        }
        let mut out = json!({"id":id(v,"id")?,"name":fallback(v,&["name","id"],"Catalog"),"type":catalog_type,"supportsSearch":v["supports_search"].as_bool().unwrap_or(false),"supportsSkip":v["supports_skip"].as_bool().unwrap_or(false),"genres":strings(v,"genres"),"extras":[],"raw":clean(v)});
        optional_string(v, &mut out, "addon_name", "addonName");
        optional_number(v, &mut out, "addon_id", "addonId");
        if let Ok(key) = id(v, "addon_id") {
            out["addonKey"] = json!(key);
        }
        out["extras"] = Value::Array(v["extra"].as_array().into_iter().flatten().filter_map(|e| {
            let name = e["name"].as_str().filter(|s|!s.is_empty())?;
            let mut extra = json!({"name":name,"required":e["is_required"].as_bool().unwrap_or(false),"options":strings(e,"options")});
            optional_string(e,&mut extra,"default","defaultValue"); optional_number(e,&mut extra,"options_limit","optionsLimit"); Some(extra)
        }).collect());
        Ok(out)
    }
    #[test]
    fn owned_catalog_matches_original_for_boundaries_and_nested_metadata() {
        let ids = [
            Value::Null,
            json!(""),
            json!("映画 Café 🎬"),
            json!(0),
            json!(-42),
            json!(9007199254740991i64),
            json!(9007199254740992i64),
            json!(u64::MAX),
            json!(1.25),
        ];
        let types = [
            Value::Null,
            json!("anime.series"),
            json!(""),
            json!(" "),
            json!("x".repeat(64)),
            json!("x".repeat(65)),
        ];
        for id_value in &ids {
            for type_value in &types {
                for name in [Value::Null, json!(""), json!("Name\n\"quoted\""), json!(42)] {
                    let input = json!({
                        "id": id_value, "type": type_value, "name": name,
                        "addon_id": id_value, "addon_name": "Addon",
                        "supports_search": true, "supports_skip": 1,
                        "genres": ["One", null, 2, "", "映画"],
                        "extra": [null, {}, {"name":""}, {"name":"search", "is_required":true, "default":"", "options_limit":4.5, "options":["x",null,2,"y"]}],
                        "metadata": {"max":u64::MAX,"min":i64::MIN,"float":-0.0,"items":[null,true,{"safe":"keep","access_TOKEN":"remove","P-r-o-x-y":"remove"}]},
                        "Authorization": "remove"
                    });
                    let stringify = |result: Result<Value>| {
                        result
                            .map(|value| serde_json::to_string(&value).unwrap())
                            .map_err(|error| error.to_string())
                    };
                    assert_eq!(
                        stringify(catalog(&input)),
                        stringify(legacy_catalog(&input)),
                        "{input}"
                    );
                    assert_eq!(
                        stringify(catalog_owned(input.clone())),
                        stringify(legacy_catalog(&input)),
                        "{input}"
                    );
                }
            }
        }
    }
}
