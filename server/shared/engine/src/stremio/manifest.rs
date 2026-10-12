//! Bounded Stremio manifest and catalog projections shared by local and server hosts.
use crate::{Error, capabilities::Capability};
use serde_json::Value;
use viptv_provider::{
    discover::{DiscoveryRequest, addon_endpoint, plan_discovery},
    extras::{bounded_exact_text, catalog_extras},
    validate_url,
};

#[derive(Clone, Debug)]
pub struct Catalog {
    pub kind: String,
    pub id: String,
    pub name: String,
    pub search: bool,
    pub required_extras: bool,
}

#[derive(Clone, Debug)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub capabilities: Vec<Capability>,
    pub catalogs: Vec<Catalog>,
}

pub fn manifest_url(raw: &str) -> Result<String, Error> {
    if raw.len() > 4096 {
        return Err(Error::InvalidQuery);
    }
    let url = validate_url(raw).map_err(|_| Error::InvalidQuery)?;
    if !url.path().ends_with("/manifest.json") {
        return Err(Error::InvalidQuery);
    }
    Ok(url.into())
}

pub fn parse_manifest(raw: &str) -> Result<Manifest, Error> {
    if raw.len() > 2 * 1024 * 1024 {
        return Err(Error::InvalidQuery);
    }
    let data: Value = serde_json::from_str(raw).map_err(|_| Error::InvalidQuery)?;
    let name = bounded_exact_text(&data["name"], 128).ok_or(Error::InvalidQuery)?;
    let version = bounded_exact_text(&data["version"], 64).ok_or(Error::InvalidQuery)?;
    bounded_exact_text(&data["id"], 128).ok_or(Error::InvalidQuery)?;
    let resources = data["resources"].as_array().ok_or(Error::InvalidQuery)?;
    let has = |role: &str| {
        resources
            .iter()
            .any(|r| r.as_str() == Some(role) || r["name"].as_str() == Some(role))
    };
    let mut capabilities = vec![];
    if has("meta") {
        capabilities.push(Capability::Metadata);
    }
    if has("stream") {
        capabilities.push(Capability::Sources);
    }
    if has("subtitles") {
        capabilities.push(Capability::Subtitles);
    }
    let catalogs = data["catalogs"]
        .as_array()
        .into_iter()
        .flatten()
        .take(64)
        .filter_map(|c| {
            let kind = bounded_exact_text(&c["type"], 64)?;
            let id = bounded_exact_text(&c["id"], 128)?;
            if !kind
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || "._-".contains(ch))
            {
                return None;
            }
            let extras = catalog_extras(c);
            Some(Catalog {
                kind,
                id,
                name: bounded_exact_text(&c["name"], 128).unwrap_or_else(|| name.clone()),
                search: extras.iter().any(|e| e.name == "search"),
                required_extras: extras
                    .iter()
                    .any(|e| e.required && e.default.is_none() && e.name != "skip"),
            })
        })
        .collect::<Vec<_>>();
    if has("catalog") && !catalogs.is_empty() {
        capabilities.push(Capability::Discovery);
    }
    if catalogs.iter().any(|c| c.search) {
        capabilities.push(Capability::Search);
    }
    if capabilities.is_empty() {
        return Err(Error::InvalidQuery);
    }
    Ok(Manifest {
        name,
        version,
        capabilities,
        catalogs,
    })
}

pub fn catalog_url(manifest_url: &str, catalog: &Catalog) -> Result<String, Error> {
    let tail = format!("{}.json", catalog.id);
    addon_endpoint(manifest_url, &["catalog", &catalog.kind, &tail])
        .map_err(|_| Error::InvalidQuery)
}

pub fn search_urls(
    manifest_url: &str,
    manifest: &Value,
    kind: &str,
    term: &str,
) -> Result<Vec<String>, Error> {
    let query = DiscoveryRequest {
        kind: kind.into(),
        catalog: None,
        addon: None,
        skip: 0,
        search: Some(term.into()),
        genre: None,
        extras: Default::default(),
    };
    Ok(
        plan_discovery(&[(1, manifest_url.into(), manifest.clone())], &query)
            .map_err(|_| Error::InvalidQuery)?
            .endpoints,
    )
}

pub fn meta_url(manifest_url: &str, manifest: &Value, kind: &str, id: &str) -> Option<String> {
    if id.len() > 512
        || !["movie", "series"].contains(&kind)
        || !viptv_provider::extras::supports(manifest, "meta", kind, id)
    {
        return None;
    }
    addon_endpoint(manifest_url, &["meta", kind, &format!("{id}.json")]).ok()
}
