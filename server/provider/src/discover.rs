//! Addon catalog discovery planning and aggregation, plus episode artwork
//! enrichment, shared by the backend and fat clients.
use crate::extras::{catalog_extras, extra_name};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use url::Url;

/// The wire shape of a catalog browse/search request. Fat clients serialize
/// this into the native core bridge; the backend constructs it from the API.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DiscoveryRequest {
    pub kind: String,
    pub catalog: Option<String>,
    pub addon: Option<i64>,
    pub skip: usize,
    pub search: Option<String>,
    pub genre: Option<String>,
    pub extras: HashMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DiscoveryPlan {
    pub endpoints: Vec<String>,
    pub pageable: bool,
    pub single_catalog: bool,
    pub aggregated: bool,
}

/// Negotiate which addon catalogs can satisfy a request and build the fetch
/// list. `entries` are `(addon_id, manifest_url, manifest)` rows in
/// installation order, matching the backend's enabled-addon selection.
pub fn plan_discovery(
    entries: &[(i64, String, Value)],
    request: &DiscoveryRequest,
) -> Result<DiscoveryPlan, String> {
    let DiscoveryRequest {
        kind,
        catalog,
        addon,
        skip,
        search,
        genre,
        extras: custom,
    } = request.clone();
    if kind.is_empty()
        || kind.len() > 64
        || !kind
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
    {
        return Err("Unsupported catalog type".into());
    }
    if custom.len() > 16
        || custom.iter().any(|(k, v)| {
            extra_name(&json!(k)).is_none()
                || ["search", "genre", "skip"].contains(&k.as_str())
                || v.chars().count() > 1024
        })
    {
        return Err("Invalid catalog options".into());
    }
    if !custom.is_empty() && (catalog.is_none() || addon.is_none()) {
        return Err("Choose a catalog for custom options".into());
    }
    let search = search
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let genre = genre
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if genre
        .as_ref()
        .is_some_and(|value| value.chars().count() > crate::extras::MAX_EXTRA_OPTION)
    {
        return Err("Genre too long".into());
    }
    let aggregated = search.is_some() && catalog.is_none();
    if aggregated && skip > 0 {
        return Err("Aggregated search does not support pagination".into());
    }
    if skip > 10000 {
        return Err("Catalog skip must not exceed 10000".into());
    }
    let mut tasks = vec![];
    let mut pageable = false;
    let mut genre_unsupported = false;
    let mut genre_invalid = false;
    'addons: for (id, u, m) in entries {
        if addon.is_some_and(|a| a != *id) {
            continue;
        }
        for c in m["catalogs"].as_array().into_iter().flatten() {
            if c["type"] != kind {
                continue;
            }
            let cid = c["id"].as_str().unwrap_or("");
            if cid.is_empty() || catalog.as_deref().is_some_and(|wanted| wanted != cid) {
                continue;
            }
            let declared_extras = catalog_extras(c);
            if search.is_some() && !declared_extras.iter().any(|extra| extra.name == "search") {
                continue;
            }
            if let Some(requested) = &genre {
                let Some(capability) = declared_extras.iter().find(|extra| extra.name == "genre")
                else {
                    genre_unsupported = true;
                    continue;
                };
                if !capability.options.is_empty()
                    && !capability.options.iter().any(|option| option == requested)
                {
                    genre_invalid = true;
                    continue;
                }
            }
            if custom.iter().any(|(key, value)| {
                !declared_extras
                    .iter()
                    .any(|e| e.name == *key && (e.options.is_empty() || e.options.contains(value)))
            }) {
                return Err("Option is not advertised by the selected catalog".into());
            }
            // Every required extra must be one this request can actually supply.
            if declared_extras.iter().any(|extra| {
                extra.required
                    && match extra.name.as_str() {
                        "skip" => false,
                        "search" => search.is_none(),
                        "genre" => genre.is_none(),
                        _ => custom.get(&extra.name).is_none_or(|v| v.trim().is_empty()),
                    }
            }) {
                continue;
            }
            let mut extras = Vec::new();
            let supports_skip = declared_extras.iter().any(|extra| extra.name == "skip");
            if supports_skip {
                extras.push(format!("skip={}", skip.min(10000)));
            } else if skip > 0 {
                return Err("Selected catalog does not support pagination".into());
            }
            if let Some(s) = &search {
                extras.push(format!(
                    "search={}",
                    url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>()
                ));
            }
            if let Some(value) = &genre {
                extras.push(format!(
                    "genre={}",
                    url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>()
                ));
            }
            for (name, value) in &custom {
                if !value.trim().is_empty() {
                    extras.push(format!(
                        "{}={}",
                        name,
                        url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>()
                    ));
                }
            }
            let endpoint = if extras.is_empty() {
                addon_endpoint(u, &["catalog", &kind, &format!("{cid}.json")])?
            } else {
                addon_extra_endpoint(u, &kind, cid, &extras.join("&"))?
            };
            tasks.push(endpoint);
            pageable = supports_skip;
            if !aggregated || tasks.len() == 32 {
                break 'addons;
            }
        }
    }
    if tasks.is_empty() {
        if genre_invalid {
            return Err("Genre is not one of the catalog's advertised options".into());
        }
        if genre_unsupported {
            return Err("Selected catalog does not advertise genre filtering".into());
        }
    }
    Ok(DiscoveryPlan {
        single_catalog: !aggregated && tasks.len() == 1,
        pageable,
        aggregated,
        endpoints: tasks,
    })
}

/// Combine already-fetched catalog responses (in plan order) into one page,
/// keeping the backend's deduplication and 200-item cap semantics.
pub fn aggregate_discovery(
    responses: &[Value],
    plan: &DiscoveryPlan,
    skip: usize,
) -> Result<Value, String> {
    let mut metas = vec![];
    let mut seen = HashSet::new();
    let mut success = false;
    let mut raw_count = 0;
    for v in responses {
        let Some(raw) = v["metas"].as_array() else {
            continue;
        };
        success = true;
        raw_count += raw.len();
        for m in raw {
            if metas.len() == 200 {
                break;
            }
            let key = format!("{}:{}", m["type"], m["id"]);
            if seen.insert(key) {
                metas.push(m.clone());
            }
        }
    }
    if !success {
        return Err("No catalog source succeeded or matched this request".into());
    }
    // Stremio supplies no total/page-size contract: a nonempty raw page is
    // potentially followed by another page; an empty page terminates traversal.
    let has_more = plan.single_catalog
        && plan.pageable
        && raw_count > 0
        && skip.saturating_add(raw_count) <= 10000;
    let next_skip = has_more.then(|| skip + raw_count);
    Ok(
        json!({"metas":metas,"has_more":has_more,"next_skip":next_skip,
    "aggregated":plan.aggregated,"max_catalogs":if plan.aggregated {32} else {1},"max_results":200}),
    )
}

pub fn addon_endpoint(base: &str, parts: &[&str]) -> Result<String, String> {
    let mut u = crate::normalize::validate_url(base)?;
    {
        let mut path = u
            .path_segments_mut()
            .map_err(|_| "Invalid addon base URL")?;
        path.pop();
        for p in parts {
            path.push(p);
        }
    }
    Ok(u.to_string())
}

pub fn addon_extra_endpoint(
    base: &str,
    kind: &str,
    catalog: &str,
    encoded_extras: &str,
) -> Result<String, String> {
    let mut url =
        crate::normalize::validate_url(&addon_endpoint(base, &["catalog", kind, catalog])?)?;
    // Values were encoded exactly once above. Unlike path_segments_mut.push,
    // set_path preserves '%' escapes, while the fixed '&'/'=' delimiters remain intact.
    url.set_path(&format!("{}/{}.json", url.path(), encoded_extras));
    Ok(url.into())
}

// Only public artwork hosts are sent to the optional resizing service. Never
// forward configured addon/CDN tokens, arbitrary URLs, or local-provider images.
fn public_episode_art(value: &Value) -> Option<String> {
    let mut url = Url::parse(value.as_str()?).ok()?;
    if url.host_str() == Some("api.top-posters.com") {
        let fallback = url
            .query_pairs()
            .find(|(key, _)| key == "fallback_url")?
            .1
            .into_owned();
        url = Url::parse(&fallback).ok()?;
    }
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || !matches!(
            url.host_str(),
            Some("artworks.thetvdb.com" | "image.tmdb.org" | "episodes.metahub.space")
        )
        || url.query().is_some()
    {
        return None;
    }
    Some(url.to_string())
}
pub fn episode_art_url(value: &Value) -> Option<String> {
    let original = public_episode_art(value)?;
    let mut proxy = Url::parse("https://wsrv.nl/").ok()?;
    proxy
        .query_pairs_mut()
        .append_pair("url", &original)
        .append_pair("w", "512")
        .append_pair("h", "288")
        .append_pair("fit", "cover")
        .append_pair("output", "jpg");
    Some(proxy.to_string())
}
pub fn enrich_episode_art(primary: &mut Value, alternate: &Value) {
    let same_series = primary["id"].as_str().is_some() && primary["id"] == alternate["id"];
    if !same_series {
        return;
    }
    let Some(videos) = primary["videos"].as_array_mut() else {
        return;
    };
    let Some(other) = alternate["videos"].as_array() else {
        return;
    };
    for video in videos.iter_mut().take(2000) {
        let current = video["thumbnail"].as_str().unwrap_or("");
        if !current.is_empty() && !current.contains("episodes.metahub.space") {
            continue;
        }
        if let Some(candidate) = other.iter().take(2000).find(|candidate| {
            let dates_agree = video["released"]
                .as_str()
                .zip(candidate["released"].as_str())
                .is_some_and(|(a, b)| a.get(..10).is_some() && a.get(..10) == b.get(..10));
            candidate["id"] == video["id"]
                || (dates_agree
                    && !video["season"].is_null()
                    && !video["episode"].is_null()
                    && candidate["season"] == video["season"]
                    && candidate["episode"] == video["episode"])
        }) {
            if let Some(image) = public_episode_art(&candidate["thumbnail"]) {
                if !image.contains("episodes.metahub.space") {
                    video["thumbnail"] = json!(image);
                }
            }
        }
    }
}
