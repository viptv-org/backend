//! Catalog "extra" capability negotiation shared by the backend and fat clients.
use serde_json::{json, Value};
use std::collections::HashSet;

const MAX_CATALOG_EXTRAS: usize = 16;
const MAX_EXTRA_OPTIONS: usize = 256;
const MAX_EXTRA_NAME: usize = 64;
pub const MAX_EXTRA_OPTION: usize = 128;

#[derive(Clone, Debug, PartialEq)]
pub struct CatalogExtra {
    pub name: String,
    pub required: bool,
    pub options: Vec<String>,
    pub options_limit: Option<usize>,
    pub default: Option<String>,
}
impl CatalogExtra {
    pub fn wire(&self) -> Value {
        json!({
            "name":self.name,
            "is_required":self.required,
            "options":self.options,
            "options_limit":self.options_limit,
            "default":self.default,
        })
    }
}
pub fn bounded_text(value: &Value, limit: usize) -> Option<String> {
    let value = value.as_str()?.trim();
    if value.is_empty() {
        return None;
    }
    Some(value.chars().take(limit).collect())
}
pub fn bounded_exact_text(value: &Value, limit: usize) -> Option<String> {
    let value = value.as_str()?.trim();
    (!value.is_empty() && value.chars().count() <= limit).then(|| value.to_owned())
}
pub fn extra_name(value: &Value) -> Option<String> {
    let name = bounded_exact_text(value, MAX_EXTRA_NAME)?;
    name.chars()
        .all(|character| character.is_ascii_alphanumeric() || "_-".contains(character))
        .then_some(name)
}
fn bounded_options(value: &Value) -> Vec<String> {
    let mut seen = HashSet::new();
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|option| bounded_exact_text(option, MAX_EXTRA_OPTION))
        .filter(|option| seen.insert(option.clone()))
        .take(MAX_EXTRA_OPTIONS)
        .collect()
}
fn merge_extra(
    extras: &mut Vec<CatalogExtra>,
    name: String,
    required: bool,
    options: Vec<String>,
    limit: Option<usize>,
) {
    if let Some(existing) = extras.iter_mut().find(|extra| extra.name == name) {
        existing.required |= required;
        for option in options {
            if existing.options.len() == MAX_EXTRA_OPTIONS {
                break;
            }
            if !existing.options.contains(&option) {
                existing.options.push(option);
            }
        }
        if existing.options_limit.is_none() {
            existing.options_limit = limit;
        }
    } else if extras.len() < MAX_CATALOG_EXTRAS {
        extras.push(CatalogExtra {
            name,
            required,
            options,
            options_limit: limit,
            default: None,
        });
    }
}
pub fn catalog_extras(catalog: &Value) -> Vec<CatalogExtra> {
    let mut extras = Vec::<CatalogExtra>::new();
    let declared = match &catalog["extra"] {
        Value::Array(values) => values.iter().collect::<Vec<_>>(),
        Value::Object(_) | Value::String(_) => vec![&catalog["extra"]],
        _ => Vec::new(),
    };
    for value in declared {
        let (name, required, options, limit) = if let Some(name) = value.as_str() {
            (
                extra_name(&Value::String(name.into())),
                false,
                Vec::new(),
                None,
            )
        } else {
            (
                extra_name(&value["name"]),
                value["isRequired"].as_bool().unwrap_or(false),
                bounded_options(&value["options"]),
                value["optionsLimit"]
                    .as_u64()
                    .map(|number| number.min(1000) as usize),
            )
        };
        if let Some(name) = name {
            merge_extra(&mut extras, name.clone(), required, options, limit);
            if let Some(default) = bounded_exact_text(&value["default"], MAX_EXTRA_OPTION) {
                if let Some(extra) = extras.iter_mut().find(|e| e.name == name) {
                    if extra.options.is_empty() || extra.options.contains(&default) {
                        extra.default = Some(default);
                    }
                }
            }
        }
    }
    for value in catalog["extraSupported"].as_array().into_iter().flatten() {
        if let Some(name) = extra_name(value) {
            merge_extra(&mut extras, name, false, Vec::new(), None);
        }
    }
    for value in catalog["extraRequired"].as_array().into_iter().flatten() {
        if let Some(name) = extra_name(value) {
            merge_extra(&mut extras, name, true, Vec::new(), None);
        }
    }
    let legacy_genres = bounded_options(&catalog["genres"]);
    if !legacy_genres.is_empty() && extras.iter().any(|extra| extra.name == "genre") {
        merge_extra(&mut extras, "genre".into(), false, legacy_genres, None);
    }
    extras
}
pub fn catalog_extra(catalog: &Value, name: &str) -> bool {
    catalog_extras(catalog)
        .iter()
        .any(|extra| extra.name == name)
}
pub fn supports(m: &Value, resource: &str, kind: &str, id: &str) -> bool {
    if m["idPrefixes"].as_array().is_some_and(|prefixes| {
        !prefixes
            .iter()
            .any(|p| p.as_str().is_some_and(|p| id.starts_with(p)))
    }) {
        return false;
    }
    m["resources"]
        .as_array()
        .map(|r| {
            r.iter().any(|r| {
                if r.as_str() == Some(resource) {
                    return m["types"]
                        .as_array()
                        .map(|t| t.iter().any(|t| t == kind))
                        .unwrap_or(true);
                }
                r["name"] == resource
                    && r["types"]
                        .as_array()
                        .map(|t| t.iter().any(|t| t == kind))
                        .unwrap_or(true)
                    && r["idPrefixes"]
                        .as_array()
                        .map(|p| p.iter().any(|p| id.starts_with(p.as_str().unwrap_or("!"))))
                        .unwrap_or(true)
            })
        })
        .unwrap_or(false)
}
