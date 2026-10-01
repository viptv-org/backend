//! Closed projection of gateway responses. Never forward arbitrary upstream JSON.
use super::client;
use serde::Deserialize;
use serde_json::{json, Map, Value};
type Result<T> = std::result::Result<T, &'static str>;

/// One closed mapping for control-API errors and asynchronous session failures.
/// HTTP status alone is never evidence of an upstream connection limit.
pub(crate) fn failure_code(code: Option<&str>) -> Option<&'static str> {
    Some(match code? {
        "unauthorized" => "gateway_key_rejected",
        "forbidden" => "gateway_scope_missing",
        "viewer_capacity" | "input_capacity" | "output_capacity" | "session_capacity" => {
            "gateway_capacity"
        }
        "input_cleanup_pending" => "gateway_cleanup_pending",
        "processing_failed" => "gateway_processing_failed",
        "source_connection_limit" => "provider_connection_limit",
        "source_preparation_failed" | "source_unavailable" => "source_unavailable",
        "unsupported_output" | "unsupported_media" => "delivery_unsupported",
        "startup_timeout" => "gateway_startup_timeout",
        "session_expired" | "session_not_found" | "media_unauthorized" => "playback_expired",
        "idempotency_conflict" => "playback_conflict",
        _ => return None,
    })
}
pub(crate) fn identifier(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}
#[derive(Deserialize)]
pub(crate) struct Session {
    pub id: String,
    pub status: String,
    pub expires_at: u64,
    pub error_code: Option<String>,
    pub playback: Option<Playback>,
}
#[derive(Deserialize)]
pub(crate) struct Playback {
    pub id: String,
    pub url: String,
    pub format: String,
    pub mode: String,
    pub video_mode: String,
    pub audio_mode: String,
    pub position: f64,
    pub duration: f64,
    pub live: bool,
    #[serde(default)]
    pub audio_tracks: Vec<Value>,
    #[serde(default)]
    pub subtitle_tracks: Vec<Value>,
    pub selected_audio: Option<Value>,
    pub selected_subtitle: Option<Value>,
    #[serde(default)]
    pub subtitles_supported: bool,
}
fn track(value: &Value) -> Result<Value> {
    let input = value.as_object().ok_or("gateway_protocol_invalid")?;
    let mut out = Map::new();
    for field in [
        "input_index",
        "output_index",
        "output_audio_ordinal",
        "output_stream_index",
    ] {
        if let Some(value) = input.get(field) {
            if !value.as_u64().is_some_and(|index| index <= 65535) {
                return Err("gateway_protocol_invalid");
            }
            out.insert(field.into(), value.clone());
        }
    }
    for field in ["codec", "language", "language_status", "title"] {
        if let Some(value) = input.get(field) {
            if !value.is_null()
                && !value
                    .as_str()
                    .is_some_and(|text| text.len() <= 512 && !text.chars().any(char::is_control))
            {
                return Err("gateway_protocol_invalid");
            }
            out.insert(field.into(), value.clone());
        }
    }
    for field in ["selected", "supported", "selectable"] {
        if let Some(value) = input.get(field) {
            if !value.is_boolean() {
                return Err("gateway_protocol_invalid");
            }
            out.insert(field.into(), value.clone());
        }
    }
    if let Some(value) = input.get("disposition") {
        if value.is_null() {
            out.insert("disposition".into(), Value::Null);
        } else {
            let fields = value.as_object().ok_or("gateway_protocol_invalid")?;
            let mut disposition = Map::new();
            for key in [
                "default",
                "commentary",
                "hearing_impaired",
                "visual_impaired",
                "forced",
            ] {
                if let Some(value) = fields.get(key) {
                    if !value.is_boolean() {
                        return Err("gateway_protocol_invalid");
                    }
                    disposition.insert(key.into(), value.clone());
                }
            }
            out.insert("disposition".into(), Value::Object(disposition));
        }
    }
    Ok(Value::Object(out))
}
impl Session {
    pub(crate) fn parse(value: Value) -> Result<Self> {
        let session: Self =
            serde_json::from_value(value).map_err(|_| "gateway_protocol_invalid")?;
        if !identifier(&session.id)
            || !matches!(
                session.status.as_str(),
                "starting" | "ready" | "failed" | "released" | "expired"
            )
        {
            return Err("gateway_protocol_invalid");
        }
        Ok(session)
    }
    pub(crate) fn delivery(&self, endpoint: &str) -> Result<Option<Value>> {
        if self.status != "ready" {
            return Ok(None);
        }
        let media = self.playback.as_ref().ok_or("gateway_protocol_invalid")?;
        if media.id != self.id
            || media.format != "hls"
            || !matches!(media.mode.as_str(), "direct" | "remux" | "transcode")
            || !matches!(media.video_mode.as_str(), "copy" | "encode")
            || !matches!(media.audio_mode.as_str(), "copy" | "encode" | "none")
            || !media.position.is_finite()
            || media.position < 0.0
            || !media.duration.is_finite()
            || media.duration < 0.0
            || media.audio_tracks.len() > 32
            || media.subtitle_tracks.len() > 32
        {
            return Err("gateway_protocol_invalid");
        }
        let parts: Vec<_> = media.url.split('/').collect();
        if parts.len() != 5
            || !parts[0].is_empty()
            || parts[1] != "media"
            || parts[2] != self.id
            || !identifier(parts[3])
            || parts[4].is_empty()
            || parts[4].len() > 160
            || parts[4].contains("..")
            || !parts[4]
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err("gateway_protocol_invalid");
        }
        let url = client::endpoint(endpoint)?
            .join(media.url.trim_start_matches('/'))
            .map_err(|_| "gateway_protocol_invalid")?;
        let audio = media
            .audio_tracks
            .iter()
            .map(track)
            .collect::<Result<Vec<_>>>()?;
        let subtitles = media
            .subtitle_tracks
            .iter()
            .map(track)
            .collect::<Result<Vec<_>>>()?;
        Ok(Some(
            json!({"kind":"gateway","url":url.as_str(),"format":media.format,"mode":media.mode,"video_mode":media.video_mode,"audio_mode":media.audio_mode,"position":media.position,"duration":media.duration,"live":media.live,"audio_tracks":audio,"subtitle_tracks":subtitles,"selected_audio":media.selected_audio.as_ref().map(track).transpose()?,"selected_subtitle":media.selected_subtitle.as_ref().map(track).transpose()?,"subtitles_supported":media.subtitles_supported}),
        ))
    }
    pub(crate) fn failure(&self) -> &'static str {
        failure_code(self.error_code.as_deref()).unwrap_or({
            if matches!(self.status.as_str(), "released" | "expired") {
                "playback_expired"
            } else {
                "gateway_processing_failed"
            }
        })
    }
}
