//! Strict catalog-scoped selection validation before any network request.
use crate::extras::{CatalogExtra, extra_name};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Same JSON shape as the engine's canonical QueryValue, without an engine dependency.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtraValues {
    pub id: String,
    pub values: Vec<String>,
}

pub fn validate_input(
    values: &[ExtraValues],
    custom: &HashMap<String, String>,
    genre: Option<&str>,
) -> Result<(), String> {
    if values.len() + custom.len() > 16 {
        return Err("Too many catalog options".into());
    }
    let mut seen = HashSet::new();
    for selection in values {
        if extra_name(&serde_json::json!(selection.id)).as_deref() != Some(&selection.id)
            || matches!(selection.id.as_str(), "search" | "skip")
            || !seen.insert(&selection.id)
            || custom.contains_key(&selection.id)
            || (selection.id == "genre" && genre.is_some())
            || selection.values.len() > 64
            || selection
                .values
                .iter()
                .any(|v| v.trim().is_empty() || v.len() > 1024)
            || selection.values.iter().collect::<HashSet<_>>().len() != selection.values.len()
        {
            return Err("Invalid or conflicting catalog selections".into());
        }
    }
    Ok(())
}

pub fn resolve(
    declared: &[CatalogExtra],
    values: &[ExtraValues],
    custom: &HashMap<String, String>,
    genre: Option<&str>,
    search: Option<&str>,
) -> Result<BTreeMap<String, Vec<String>>, String> {
    let mut supplied: BTreeMap<String, Vec<String>> = custom
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                if v.trim().is_empty() {
                    vec![]
                } else {
                    vec![v.clone()]
                },
            )
        })
        .collect();
    if let Some(genre) = genre {
        supplied.insert("genre".into(), vec![genre.into()]);
    }
    for value in values {
        supplied.insert(value.id.clone(), value.values.clone());
    }
    for name in supplied.keys() {
        if !declared.iter().any(|e| e.name == *name) {
            return Err(if name == "genre" {
                "Selected catalog does not advertise genre filtering"
            } else {
                "Option is not advertised by the selected catalog"
            }
            .into());
        }
    }
    for extra in declared {
        if extra.name == "skip" {
            continue;
        }
        if extra.name == "search" {
            if search.is_none() {
                if let Some(default) = &extra.default {
                    if extra.options_limit == Some(0) {
                        return Err("Catalog search is disabled".into());
                    }
                    supplied.insert(extra.name.clone(), vec![default.clone()]);
                } else if extra.required {
                    return Err("Required catalog search is missing".into());
                }
            }
            continue;
        }
        let selected = supplied.entry(extra.name.clone()).or_default();
        if selected.is_empty() && extra.options_limit != Some(0) {
            selected.extend(extra.default.clone());
        }
        let limit = extra.options_limit.unwrap_or(1).min(64);
        if selected.len() > limit {
            return Err("Too many selections for the advertised catalog limit".into());
        }
        if selected
            .iter()
            .any(|v| !extra.options.is_empty() && !extra.options.contains(v))
        {
            return Err("Selection is not one of the catalog's advertised options".into());
        }
        if extra.required && selected.is_empty() {
            return Err("No catalog source matched: required catalog selection is missing".into());
        }
    }
    supplied.retain(|_, v| !v.is_empty());
    Ok(supplied)
}
