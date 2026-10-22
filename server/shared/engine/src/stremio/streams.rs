use super::release_facts::{ReleaseFacts, source_release_facts, stream_delivery};
use super::source_metadata::{source_audio_languages, source_detail, source_quality_height};
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

/// Provider-declared display metadata; none of these fields grants playback authority.
#[derive(Clone, Debug, Default, Serialize, Deserialize, Facet)]
pub struct SourcePresentation {
    pub provider_name: Option<String>,
    pub source_id: Option<String>,
    pub delivery_format: Option<String>,
    pub container: Option<String>,
    pub quality: Option<String>,
    pub video_codec: Option<String>,
    #[serde(default)]
    pub audio_codecs: Vec<String>,
    pub size_bytes: Option<u64>,
    pub size_label: Option<String>,
}
impl SourcePresentation {
    /// Accept structured declarations only, with bounded display text.
    pub fn read(value: &Value) -> Option<Self> {
        if !value.is_object() {
            return None;
        }
        let field = |key| {
            text(value, key, 128).filter(|s| !s.chars().any(char::is_control) && !s.contains("://"))
        };
        Some(Self {
            provider_name: field("provider_name"),
            source_id: field("source_id"),
            delivery_format: field("delivery_format")
                .filter(|v| matches!(v.as_str(), "hls" | "progressive")),
            container: field("container"),
            quality: field("quality"),
            video_codec: field("video_codec"),
            audio_codecs: value["audio_codecs"]
                .as_array()
                .into_iter()
                .flatten()
                .take(16)
                .filter_map(|v| {
                    v.as_str()
                        .filter(|s| {
                            !s.is_empty()
                                && s.len() <= 64
                                && !s.chars().any(char::is_control)
                                && !s.contains("://")
                        })
                        .map(str::to_owned)
                })
                .collect(),
            size_bytes: value["size_bytes"].as_u64().filter(|n| *n > 0),
            size_label: field("size_label"),
        })
    }
}

/// Release facts (SRC-BEST-MATCH-001) are omitted when unknown so earlier
/// payloads still parse; see `release_facts` for the value vocabularies.
#[derive(Clone, Debug, Default, Serialize, Deserialize, Facet)]
pub struct SourceCandidate {
    pub id: String,
    pub provider_id: String,
    pub label: String,
    pub detail: Option<String>,
    #[serde(default)]
    pub quality_height: Option<u32>,
    #[serde(default)]
    pub audio_languages: Vec<String>,
    #[serde(default)]
    pub presentation: Option<SourcePresentation>,
    /// `web_dl`, `webrip`, `bluray`, `remux`, `hdtv`, `dvd`, `other_rip` or `cam`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_type: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub three_d: bool,
    /// Any of `dv`, `hdr10_plus`, `hdr10`, `hdr`, `hlg`; empty is unknown, not SDR.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dynamic_range: Vec<String>,
    /// Any of `dual`, `multi`, `dubbed`; never audio languages.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio_flags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seeders: Option<u32>,
    /// `direct` or `torrent`, from the stream transport only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<String>,
}
impl SourceCandidate {
    /// Copy normalized release facts onto this candidate.
    pub fn with_release_facts(mut self, facts: ReleaseFacts) -> Self {
        self.release_type = facts.release_type;
        self.three_d = facts.three_d;
        self.dynamic_range = facts.dynamic_range;
        self.audio_flags = facts.audio_flags;
        self.seeders = facts.seeders;
        self
    }
}

#[derive(Clone, Debug)]
pub struct PlayableSource {
    pub label: String,
    pub detail: Option<String>,
    pub quality_height: Option<u32>,
    pub audio_languages: Vec<String>,
    pub presentation: Option<SourcePresentation>,
    pub release_facts: ReleaseFacts,
    /// `direct` or `torrent`, from the transport.
    pub delivery: &'static str,
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
                detail: source_detail(source),
                presentation: SourcePresentation::from_stream(source),
                url: parsed.into(),
                headers,
                quality_height: source_quality_height(source),
                audio_languages: source_audio_languages(source),
                release_facts: source_release_facts(source),
                delivery: stream_delivery(source),
            })
        })
        .take(80)
        .collect())
}
