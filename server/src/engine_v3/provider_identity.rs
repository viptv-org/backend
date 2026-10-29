use super::*;
use viptv_engine::identity::{ExternalId, MediaIdentity};

const FIELDS: [(&str, &[&str]); 7] = [
    ("imdb", &["imdb_id", "imdbId", "imdbID", "_imdbId"]),
    ("tmdb", &["tmdb_id", "tmdbId", "moviedb_id", "_tmdbId"]),
    ("tvdb", &["tvdb_id", "tvdbId", "_tvdbId", "tvdb"]),
    ("mal", &["mal_id", "malId", "_malId"]),
    ("kitsu", &["kitsu_id", "kitsuId", "_kitsuId"]),
    ("anilist", &["anilist_id", "anilistId", "_anilistId"]),
    ("anidb", &["anidb_id", "anidbId", "_anidbId"]),
];

fn bounded(namespace: &str, value: &Value) -> Option<ExternalId> {
    let text = value
        .as_str()
        .filter(|value| value.len() <= 32)
        .map(str::to_owned)
        .or_else(|| value.as_u64().map(|value| value.to_string()))?;
    let valid = if namespace == "imdb" {
        text.strip_prefix("tt").is_some_and(|digits| {
            !digits.is_empty()
                && digits.len() <= 16
                && digits.bytes().all(|byte| byte.is_ascii_digit())
        })
    } else {
        text.parse::<u64>()
            .is_ok_and(|number| number > 0 && number.to_string() == text)
    };
    valid.then(|| ExternalId {
        namespace: namespace.into(),
        value: text,
    })
}

pub(crate) fn ids(meta: &Value) -> Vec<ExternalId> {
    let mut found: Vec<ExternalId> = Vec::new();
    for (namespace, aliases) in FIELDS {
        let values = aliases.iter().map(|field| &meta[*field]).chain(
            ["ids", "external_ids"].into_iter().flat_map(|container| {
                std::iter::once(&meta[container][namespace])
                    .chain(aliases.iter().map(move |field| &meta[container][*field]))
            }),
        );
        for value in values {
            let Some(id) = bounded(namespace, value) else {
                continue;
            };
            if found.len() == 32 {
                return found;
            }
            if !found.contains(&id) {
                found.push(id);
            }
        }
    }
    found
}

fn key(id: &ExternalId) -> String {
    if id.namespace == "imdb" {
        id.value.clone()
    } else {
        format!("{}:{}", id.namespace, id.value)
    }
}

fn namespace(raw: &str) -> (&str, &str) {
    if raw.strip_prefix("tt").is_some_and(|digits| {
        !digits.is_empty()
            && digits.len() <= 16
            && digits.bytes().all(|byte| byte.is_ascii_digit())
    }) {
        return ("imdb", raw);
    }
    match raw.split_once(':') {
        Some((namespace, value)) if FIELDS.iter().any(|(known, _)| *known == namespace) => {
            (namespace, value)
        }
        _ => ("opaque", raw),
    }
}

pub(crate) fn same_title(meta: &Value, lookup: &str) -> bool {
    if meta["id"].as_str() == Some(lookup) {
        return true;
    }
    let (lookup_namespace, lookup_value) = namespace(lookup);
    if let Some(returned) = meta["id"].as_str() {
        let (returned_namespace, returned_value) = namespace(returned);
        if returned_namespace == lookup_namespace && returned_value != lookup_value {
            return false;
        }
    }
    ids(meta).iter().any(|id| key(id) == lookup)
}

pub(crate) fn merge(identity: &mut MediaIdentity, meta: &Value) {
    for id in ids(meta) {
        if identity.ids.len() == 32 {
            return;
        }
        if identity
            .ids
            .iter()
            .any(|existing| existing.namespace == id.namespace)
        {
            continue;
        }
        identity.ids.push(id);
    }
}
