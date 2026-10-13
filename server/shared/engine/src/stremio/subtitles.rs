use crate::{Error, presentation::RowState, profile::Scope};
use facet::Facet;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SubtitleExtras {
    #[serde(rename = "videoHash")]
    pub video_hash: Option<String>,
    #[serde(rename = "videoSize")]
    pub video_size: Option<u64>,
    pub filename: Option<String>,
}

impl SubtitleExtras {
    pub fn validate(&self) -> Result<(), Error> {
        if self
            .video_hash
            .as_ref()
            .is_some_and(|v| v.len() != 16 || !v.bytes().all(|c| c.is_ascii_hexdigit()))
            || self.video_size == Some(0)
            || self
                .filename
                .as_ref()
                .is_some_and(|v| v.is_empty() || v.len() > 512 || v.chars().any(char::is_control))
        {
            return Err(Error::InvalidQuery);
        }
        Ok(())
    }
}

pub fn subtitle_url(
    manifest_url: &str,
    manifest: &Value,
    kind: &str,
    id: &str,
    extras: &SubtitleExtras,
) -> Result<Option<String>, Error> {
    extras.validate()?;
    if id.is_empty()
        || id.len() > 512
        || id.chars().any(char::is_control)
        || !["movie", "series"].contains(&kind)
    {
        return Err(Error::InvalidQuery);
    }
    if !viptv_provider::extras::supports(manifest, "subtitles", kind, id) {
        return Ok(None);
    }
    let mut params = url::form_urlencoded::Serializer::new(String::new());
    if let Some(value) = &extras.video_hash {
        params.append_pair("videoHash", value);
    }
    if let Some(value) = extras.video_size {
        params.append_pair("videoSize", &value.to_string());
    }
    if let Some(value) = &extras.filename {
        params.append_pair("filename", value);
    }
    let encoded = params.finish();
    let endpoint = if encoded.is_empty() {
        viptv_provider::discover::addon_endpoint(
            manifest_url,
            &["subtitles", kind, &format!("{id}.json")],
        )
    } else {
        let base = viptv_provider::discover::addon_endpoint(manifest_url, &["subtitles", kind, id])
            .map_err(|_| Error::InvalidQuery)?;
        let mut endpoint = url::Url::parse(&base).map_err(|_| Error::InvalidQuery)?;
        endpoint.set_path(&format!("{}/{}.json", endpoint.path(), encoded));
        Ok(endpoint.to_string())
    };
    endpoint.map(Some).map_err(|_| Error::InvalidQuery)
}

/// Provider URL stays in the host; opaque candidates may safely enter UI state.
#[derive(Clone, Debug)]
pub struct SubtitleFile {
    pub language: String,
    pub url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
pub struct SubtitleCandidate {
    pub id: String,
    pub provider_id: String,
    pub language: String,
    pub label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
pub struct SubtitleProvider {
    pub provider_id: String,
    pub state: RowState,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
pub struct SubtitleSnapshot {
    pub scope: Scope,
    pub candidates: Vec<SubtitleCandidate>,
    pub providers: Vec<SubtitleProvider>,
    pub complete: bool,
}

pub fn read_subtitles(raw: &str) -> Result<Vec<SubtitleFile>, Error> {
    if raw.len() > 2 * 1024 * 1024 {
        return Err(Error::InvalidQuery);
    }
    let value: Value = serde_json::from_str(raw).map_err(|_| Error::InvalidQuery)?;
    let values = value["subtitles"].as_array().ok_or(Error::InvalidQuery)?;
    let mut seen = HashSet::new();
    Ok(values
        .iter()
        .take(200)
        .filter_map(|entry| {
            let language = entry["lang"].as_str()?.trim().to_ascii_lowercase();
            let mut parts = language.split('-');
            let first = parts.next()?;
            if !(2..=3).contains(&first.len())
                || !first.bytes().all(|b| b.is_ascii_alphabetic())
                || language.len() > 35
                || parts.any(|part| {
                    part.is_empty()
                        || part.len() > 8
                        || !part.bytes().all(|b| b.is_ascii_alphanumeric())
                })
            {
                return None;
            }
            let raw_url = entry["url"].as_str()?;
            if raw_url.len() > 4096 || raw_url.chars().any(char::is_control) {
                return None;
            }
            let url = viptv_provider::validate_url(raw_url).ok()?.to_string();
            if !seen.insert((language.clone(), url.clone())) {
                return None;
            }
            Some(SubtitleFile { language, url })
        })
        .take(80)
        .collect())
}
