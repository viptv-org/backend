use super::*;

pub(super) fn catalog_filters(kind: &str, v: &Value) -> Value {
    {
        let mut filters = v["extras"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|e| e["name"] != "skip")
            .collect::<Vec<_>>();
        if v["supportsSearch"] == true && !filters.iter().any(|f| f["name"] == "search") {
            filters.push(json!({"name":"search","required":false,"options":[]}));
        }
        if kind == "catalogFilters" {
            json!(filters)
        } else {
            let mut out = json!({});
            for f in filters.iter().filter(|f| f["required"] == true) {
                out[text(f, "name")] = f
                    .get("defaultValue")
                    .filter(|v| !v.is_null())
                    .or_else(|| f["options"].as_array().and_then(|a| a.first()))
                    .cloned()
                    .unwrap_or(json!(""));
            }
            out
        }
    }
}

pub(super) fn type_group(kind: &str) -> &'static str {
    match kind {
        "movie" => "movie",
        "series" => "series",
        "anime" => "anime",
        _ if kind.starts_with("anime.") => "anime",
        _ => "other",
    }
}

pub(super) fn group_label(group: &str) -> &'static str {
    match group {
        "movie" => "Movies",
        "series" => "Series",
        "anime" => "Anime",
        _ => "Other",
    }
}

pub(super) fn discover_policy(v: &Value) -> Value {
    let group = type_group(text(v, "type"));
    let catalogs = v["catalogs"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let index = catalogs
        .iter()
        .position(|c| text(c, "type") != "live" && type_group(text(c, "type")) == group)
        .or_else(|| catalogs.iter().position(|c| text(c, "type") != "live"))
        .or_else(|| (!catalogs.is_empty()).then_some(0));
    let mut defaults = catalog_filters("catalogDefaults", &v["catalog"]);
    // Empty defaults are not submitted as a required filter selection.
    if let Some(values) = defaults.as_object_mut() {
        values.retain(|_, value| value.as_str().is_some_and(|s| !s.trim().is_empty()));
    }
    json!({"group":group,"groupLabel":group_label(group),"firstCatalogIndex":index,"defaults":defaults})
}

pub(super) fn artwork_url(v: &Value) -> Result {
    Ok({
        let original = text(v, "original");
        if original.is_empty() {
            Value::Null
        } else {
            let mut uri = original.to_owned();
            if uri.starts_with("https://wsrv.nl/?")
                && let Ok(u) = url::Url::parse(&uri)
                && let Some((_, inner)) = u.query_pairs().find(|(k, _)| k == "url")
            {
                uri = inner.into_owned();
            }
            let width = num(v, "width");
            if policy_regex!(
                r"^https://(image\.tmdb\.org|artworks\.thetvdb\.com|episodes\.metahub\.space|images\.metahub\.space|live\.metahub\.space|assets\.fanart\.tv|i\.imgur\.com)/[^?#@]+$"
            )
            .is_match(&uri)
            {
                let size = if width > 1280.0 {
                    "original"
                } else if width > 500.0 {
                    "w1280"
                } else {
                    "w500"
                };
                uri = policy_regex!(r"^https://image\.tmdb\.org/t/p/(w[0-9]+|original)/")
                    .replace(&uri, format!("https://image.tmdb.org/t/p/{size}/"))
                    .into_owned();
            }
            // Every remote image routes through wsrv so one CDN cache and
            // resize pipeline serves all artwork, whatever the origin host.
            // data: and relative sources stay at origin; clients fall back
            // to the origin URL when wsrv cannot fetch an image.
            if uri.starts_with("http://") || uri.starts_with("https://") {
                json!(format!(
                    "https://wsrv.nl/?url={}&w={}&h={}&fit={}&output={}&q={}&we",
                    enc(&uri),
                    width,
                    num(v, "height"),
                    if v["logo"] == true { "inside" } else { "cover" },
                    if v["logo"] == true { "png" } else { "jpg" },
                    if v["large"] == true { 95 } else { 85 }
                ))
            } else {
                json!(original)
            }
        }
    })
}
