//! Shared option negotiation for local and server catalog consumers.
use crate::{
    Error,
    query::{Choice, ControlKind, QueryControl},
};
use serde_json::Value;
use viptv_provider::{
    discover::{DiscoveryRequest, plan_discovery},
    extras::catalog_extras,
};

pub use viptv_provider::discover::{
    DiscoveryPlan as CatalogPlan, DiscoveryRequest as CatalogQuery,
};
pub use viptv_provider::discovery_values::ExtraValues;
pub fn discovery_plan(
    manifest_url: &str,
    manifest: &Value,
    query: &DiscoveryRequest,
) -> Result<CatalogPlan, Error> {
    plan_discovery(&[(1, manifest_url.into(), manifest.clone())], query)
        .map_err(|_| Error::InvalidQuery)
}
pub fn discovery_urls(
    manifest_url: &str,
    manifest: &Value,
    query: &DiscoveryRequest,
) -> Result<Vec<String>, Error> {
    plan_discovery(&[(1, manifest_url.into(), manifest.clone())], query)
        .map(|p| p.endpoints)
        .map_err(|_| Error::InvalidQuery)
}
pub fn catalog_controls(catalog: &Value) -> Vec<QueryControl> {
    catalog_extras(catalog)
        .into_iter()
        .filter(|e| e.name != "skip" && e.options_limit != Some(0))
        .map(|e| QueryControl {
            id: e.name.clone(),
            label: e.name,
            kind: if e.options.is_empty() {
                ControlKind::Text
            } else if e.options_limit.unwrap_or(1) > 1 {
                ControlKind::MultiChoice
            } else {
                ControlKind::SingleChoice
            },
            required: e.required && e.default.is_none(),
            choices: e
                .options
                .iter()
                .map(|v| Choice {
                    label: v.clone(),
                    value: v.clone(),
                })
                .collect(),
            minimum: None,
            maximum: None,
            maximum_selections: Some(if e.options.is_empty() {
                1
            } else {
                e.options_limit.unwrap_or(1).min(64) as u32
            }),
        })
        .collect()
}

pub fn catalog_page(
    raw: &str,
    provider: &str,
    skip: u32,
    pageable: bool,
) -> Result<crate::presentation::CatalogPage, Error> {
    if raw.len() > 2 * 1024 * 1024 || skip > 10_000 {
        return Err(Error::InvalidQuery);
    }
    let value: Value = serde_json::from_str(raw).map_err(|_| Error::InvalidQuery)?;
    let values = value["metas"].as_array().ok_or(Error::InvalidQuery)?;
    if values.len() > 200 {
        return Err(Error::InvalidQuery);
    }
    let mut cards = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for chunk in values.chunks(24) {
        for card in super::cards(&serde_json::json!({"metas":chunk}).to_string(), provider)? {
            if seen.insert((card.identity.kind.clone(), card.identity.id.clone())) {
                cards.push(card);
            }
        }
    }
    let next = u64::from(skip) + values.len() as u64;
    let more = pageable && !values.is_empty() && next <= 10_000;
    Ok(crate::presentation::CatalogPage {
        cards,
        next_cursor: more.then(|| next.to_string()),
        complete: !more,
    })
}
pub fn catalog_pageable(catalog: &Value) -> bool {
    catalog_extras(catalog).iter().any(|e| e.name == "skip")
}
