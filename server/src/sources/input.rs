//! Private transport normalization; discovery never fetches media or publishes it.
use crate::util;
use serde_json::Value;
use url::Url;

pub(super) struct Input {
    pub url: String,
    pub info_hash: Option<String>,
    pub file_index: Option<u32>,
    pub trackers: Vec<String>,
    pub requires_gateway: bool,
    pub private_values: Vec<String>,
}

impl Input {
    pub fn parse(raw: &Value, addon: bool, live: bool) -> Option<Self> {
        if [
            "externalUrl",
            "ytId",
            "rarUrls",
            "zipUrls",
            "7zipUrls",
            "tgzUrls",
            "tarUrls",
            "nzbUrl",
            "servers",
            "fileMustInclude",
        ]
        .iter()
        .any(|key| raw.get(key).is_some())
        {
            return None;
        }
        let file_index = match raw.get("fileIdx") {
            None => None,
            Some(value) => Some(
                u32::try_from(value.as_u64()?)
                    .ok()
                    .filter(|index| *index <= 65535)?,
            ),
        };
        let mut private_values = Vec::new();
        let mut trackers = Vec::new();
        let (url, hash, torrent, archive) = if let Some(value) = raw.get("infoHash") {
            if !addon {
                return None;
            }
            let hash = value.as_str()?;
            if hash.len() != 40 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            private_values.push(hash.into());
            let hash = hash.to_ascii_lowercase();
            let url = if let Some(raw_url) = raw.get("url") {
                let raw_url = raw_url.as_str()?;
                let parsed = util::validate_url(raw_url).ok()?;
                if !parsed.path().to_ascii_lowercase().ends_with(".torrent") {
                    return None;
                }
                raw_url.to_owned()
            } else {
                format!("magnet:?xt=urn:btih:{hash}")
            };
            (url, Some(hash), true, false)
        } else {
            let url = raw["url"].as_str()?;
            if url.starts_with("magnet:") {
                if !addon {
                    return None;
                }
                let hash = magnet_hash(url)?;
                (url.into(), Some(hash), true, false)
            } else {
                let parsed = util::validate_url(url).ok()?;
                let extension = parsed
                    .path()
                    .rsplit_once('.')
                    .map(|(_, extension)| extension.to_ascii_lowercase());
                let torrent = extension.as_deref() == Some("torrent");
                let archive = extension.as_deref() == Some("rar");
                if (torrent || archive) && !addon {
                    return None;
                }
                (url.into(), None, torrent, archive)
            }
        };
        if ((torrent || archive) && live) || (file_index.is_some() && !torrent) {
            return None;
        }
        if let Some(hash) = &hash {
            private_values.extend([
                hash.clone(),
                hash.to_ascii_lowercase(),
                hash.to_ascii_uppercase(),
            ]);
        }
        if let Some(value) = raw.get("sources") {
            let hints = value.as_array()?;
            if hints.len() > 32 || (!torrent && !hints.is_empty()) {
                return None;
            }
            for value in hints {
                let hint = value.as_str()?;
                if hint.len() > 2048 || hint.chars().any(char::is_control) {
                    return None;
                }
                if let Some(tracker) = hint.strip_prefix("tracker:") {
                    validate_tracker(tracker)?;
                    let tracker = Url::parse(tracker).ok()?.to_string();
                    if !trackers.contains(&tracker) {
                        trackers.push(tracker)
                    }
                } else {
                    let dht = hint.strip_prefix("dht:")?;
                    if !hash
                        .as_ref()
                        .is_some_and(|hash| dht.eq_ignore_ascii_case(hash))
                    {
                        return None;
                    }
                }
                private_values.push(hint.into());
            }
        }
        let url = if torrent || archive {
            private_values.push(url.clone());
            canonical_url(&url)?
        } else {
            url
        };
        if torrent && url.starts_with("magnet:") {
            for (key, value) in Url::parse(&url).ok()?.query_pairs() {
                if key == "tr" {
                    validate_tracker(&value)?;
                    let tracker = Url::parse(&value).ok()?.to_string();
                    if !trackers.contains(&tracker) {
                        trackers.push(tracker)
                    }
                }
            }
        }
        if trackers.len() > 32 {
            return None;
        }
        Some(Self {
            url,
            info_hash: hash,
            file_index,
            trackers,
            requires_gateway: torrent || archive,
            private_values,
        })
    }
}

fn canonical_url(value: &str) -> Option<String> {
    let mut url = Url::parse(value).ok()?;
    if url.scheme() == "magnet" {
        let mut pairs: Vec<_> = url.query_pairs().into_owned().collect();
        for (key, value) in &mut pairs {
            if key == "xt" {
                *value = value.to_ascii_lowercase();
            } else if key == "tr" {
                *value = Url::parse(value).ok()?.to_string();
            }
        }
        pairs.sort();
        url.query_pairs_mut().clear().extend_pairs(pairs);
    }
    Some(url.to_string())
}

fn validate_tracker(value: &str) -> Option<()> {
    let url = Url::parse(value).ok()?;
    (matches!(url.scheme(), "http" | "https" | "udp")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none())
    .then_some(())
}

fn magnet_hash(value: &str) -> Option<String> {
    if value.len() > 8192 || value.chars().any(char::is_control) {
        return None;
    }
    let url = Url::parse(value).ok()?;
    if url.scheme() != "magnet"
        || !url.path().is_empty()
        || url.host_str().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let mut hash = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "xt" => {
                if hash.is_some() {
                    return None;
                }
                let value = value.strip_prefix("urn:btih:")?;
                if !(value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit())
                    || value.len() == 32
                        && value
                            .bytes()
                            .all(|b| b.is_ascii_alphabetic() || (b'2'..=b'7').contains(&b)))
                {
                    return None;
                }
                hash = Some(value.to_owned());
            }
            "tr" => validate_tracker(&value)?,
            "dn" => {}
            _ => return None,
        }
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn tracker_hints_are_preserved_without_entering_presentation_values() {
        let parsed=Input::parse(&json!({"infoHash":"1111111111111111111111111111111111111111",
            "sources":["tracker:udp://tracker.fixture.invalid:6969/announce","dht:1111111111111111111111111111111111111111"]}),true,false).unwrap();
        assert_eq!(
            parsed.trackers,
            vec!["udp://tracker.fixture.invalid:6969/announce"]
        );
        assert!(parsed
            .private_values
            .iter()
            .any(|value| value.starts_with("tracker:")));
        assert!(!parsed.url.contains("dht:"));
    }
}
