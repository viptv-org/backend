use crate::Error;
use facet::Facet;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub fn stream_url(manifest_url: &str, manifest: &Value, kind: &str, id: &str) -> Option<String> {
    if id.len() > 512
        || !["movie", "series"].contains(&kind)
        || !viptv_provider::extras::supports(manifest, "stream", kind, id)
    {
        return None;
    }
    viptv_provider::discover::addon_endpoint(manifest_url, &["stream", kind, &format!("{id}.json")])
        .ok()
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
pub struct SourceCandidate {
    pub id: String,
    pub provider_id: String,
    pub label: String,
    pub detail: Option<String>,
    #[serde(default)]
    pub quality_height:Option<u32>,
    #[serde(default)]
    pub audio_languages:Vec<String>,
}

#[derive(Clone, Debug)]
pub struct PlayableSource {
    pub label: String,
    pub detail: Option<String>,
    pub quality_height:Option<u32>,
    pub audio_languages:Vec<String>,
    pub url: String,
    pub headers: Vec<(String, String)>,
}

fn text(v: &Value, key: &str, max: usize) -> Option<String> {
    v[key]
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.chars().count() <= max)
        .map(str::to_owned)
}

fn token_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

pub fn read_streams(raw: &str) -> Result<Vec<PlayableSource>, Error> {
    if raw.len() > 2 * 1024 * 1024 {
        return Err(Error::InvalidQuery);
    }
    let result: Value = serde_json::from_str(raw).map_err(|_| Error::InvalidQuery)?;
    let streams = result["streams"].as_array().ok_or(Error::InvalidQuery)?;
    Ok(streams
        .iter()
        .take(100)
        .filter_map(|source| {
            let uri = text(source, "url", 4096)?;
            let parsed = viptv_provider::validate_url(&uri).ok()?;
            let mut headers = Vec::new();
            if let Some(values) = source["behaviorHints"]["proxyHeaders"]["request"].as_object() {
                for (name, value) in values.iter().take(16) {
                    let Some(v) = value.as_str() else { return None };
                    if !token_name(name)
                        || [
                            "host",
                            "connection",
                            "content-length",
                            "transfer-encoding",
                            "proxy-authorization",
                        ]
                        .iter()
                        .any(|reserved| name.eq_ignore_ascii_case(reserved))
                        || v.len() > 2048
                        || v.chars().any(|ch| ch.is_control())
                    {
                        return None;
                    }
                    headers.push((name.clone(), v.into()));
                }
            }
            Some(PlayableSource {
                label: text(source, "name", 256)
                    .or_else(|| text(source, "title", 256))
                    .unwrap_or_else(|| "Stream".into()),
                detail: text(source, "description", 1000),
                url: parsed.into(),
                headers,
                quality_height:source["quality_height"].as_u64().and_then(|h|u32::try_from(h).ok()).filter(|h|(144..=4320).contains(h)),
                audio_languages:source["audio_languages"].as_array().map(|languages|languages.iter().take(16).filter_map(|v|v.as_str().filter(|s|crate::preferences::language_tag(s)).map(str::to_owned)).collect()).unwrap_or_default(),
            })
        })
        .take(80)
        .collect())
}
