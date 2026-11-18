use serde_json::{Map, Value};

pub(super) const HEARTBEAT_BODY_LIMIT: usize = 2048;

#[derive(Debug, Default, PartialEq)]
pub(super) struct BufferingStats {
    pub rebuffer_count: Option<u64>,
    pub rebuffer_ms: Option<u64>,
    pub buffered_ahead_seconds: Option<f64>,
    pub throughput_kbps: Option<u64>,
    pub source_bitrate_kbps: Option<u64>,
    pub video_decoder: Option<String>,
    pub audio_decoder: Option<String>,
    pub dropped_frames: Option<u64>,
}

impl BufferingStats {
    pub const EMPTY: Self = Self {
        rebuffer_count: None,
        rebuffer_ms: None,
        buffered_ahead_seconds: None,
        throughput_kbps: None,
        source_bitrate_kbps: None,
        video_decoder: None,
        audio_decoder: None,
        dropped_frames: None,
    };
}

pub(super) enum HeartbeatBody {
    Empty,
    EmptyObject,
    Buffering(BufferingStats),
}

pub(super) fn parse(body: &[u8]) -> Option<HeartbeatBody> {
    if body.is_empty() {
        return Some(HeartbeatBody::Empty);
    }
    let Value::Object(mut fields) = serde_json::from_slice(body).ok()? else {
        return None;
    };
    let buffering = fields.remove("buffering");
    if !fields.is_empty() {
        return None;
    }
    match buffering {
        None => Some(HeartbeatBody::EmptyObject),
        Some(Value::Object(stats)) => Some(HeartbeatBody::Buffering(BufferingStats::from(&stats))),
        Some(_) => None,
    }
}

impl From<&Map<String, Value>> for BufferingStats {
    fn from(stats: &Map<String, Value>) -> Self {
        let count = |key: &str, max: u64| stats.get(key)?.as_u64().filter(|n| *n <= max);
        Self {
            rebuffer_count: count("rebuffer_count", 10_000),
            rebuffer_ms: count("rebuffer_ms", 3_600_000),
            buffered_ahead_seconds: stats
                .get("buffered_ahead_seconds")
                .and_then(Value::as_f64)
                .filter(|s| s.is_finite() && (0.0..=86_400.0).contains(s)),
            throughput_kbps: count("throughput_kbps", 10_000_000),
            source_bitrate_kbps: count("source_bitrate_kbps", 1_000_000),
            video_decoder: decoder(stats.get("video_decoder")),
            audio_decoder: decoder(stats.get("audio_decoder")),
            dropped_frames: count("dropped_frames", 10_000_000),
        }
    }
}

const MEDIA_EXTENSIONS: [&str; 12] = [
    ".mkv", ".mp4", ".m4v", ".avi", ".ts", ".m2ts", ".m3u8", ".mpd", ".webm", ".mov", ".wmv", ".iso",
];

fn decoder(value: Option<&Value>) -> Option<String> {
    let name = value?.as_str()?.trim();
    let lower = name.to_ascii_lowercase();
    let allowed = !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b' '))
        && !MEDIA_EXTENSIONS.iter().any(|ext| lower.ends_with(ext));
    allowed.then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn stats(value: Value) -> BufferingStats {
        match parse(json!({ "buffering": value }).to_string().as_bytes()) {
            Some(HeartbeatBody::Buffering(stats)) => stats,
            _ => panic!("buffering body refused"),
        }
    }

    #[test]
    fn accepts_every_field_in_range_and_ignores_unknown_ones() {
        let parsed = stats(json!({
            "rebuffer_count": 2,
            "rebuffer_ms": 1500,
            "buffered_ahead_seconds": 12.5,
            "throughput_kbps": 24000,
            "source_bitrate_kbps": 8000,
            "video_decoder": "c2.android.hevc.decoder",
            "audio_decoder": "OMX.google.aac.decoder",
            "dropped_frames": 3,
            "future_field": "kept out"
        }));
        assert_eq!(
            parsed,
            BufferingStats {
                rebuffer_count: Some(2),
                rebuffer_ms: Some(1500),
                buffered_ahead_seconds: Some(12.5),
                throughput_kbps: Some(24000),
                source_bitrate_kbps: Some(8000),
                video_decoder: Some("c2.android.hevc.decoder".into()),
                audio_decoder: Some("OMX.google.aac.decoder".into()),
                dropped_frames: Some(3),
            }
        );
    }

    #[test]
    fn drops_wrong_types_out_of_range_values_and_unsafe_names() {
        let parsed = stats(json!({
            "rebuffer_count": -1,
            "rebuffer_ms": "1500",
            "buffered_ahead_seconds": 1e9,
            "throughput_kbps": 1.5,
            "source_bitrate_kbps": 2_000_000,
            "video_decoder": "https://host/movie",
            "audio_decoder": "Some.Movie.2024.1080p.mkv",
            "dropped_frames": null
        }));
        assert_eq!(parsed, BufferingStats::default());
        for name in ["", "a/b", "user:pass@host", "x".repeat(65).as_str(), "trailer.MP4"] {
            assert_eq!(decoder(Some(&json!(name))), None, "{name}");
        }
    }

    #[test]
    fn keeps_the_heartbeat_envelope_strict() {
        assert!(matches!(parse(b""), Some(HeartbeatBody::Empty)));
        assert!(matches!(parse(b"{}"), Some(HeartbeatBody::EmptyObject)));
        for body in [
            json!([]),
            json!(0),
            json!({"extra": true}),
            json!({"buffering": 1}),
            json!({"buffering": {}, "extra": true}),
        ] {
            assert!(parse(body.to_string().as_bytes()).is_none(), "{body}");
        }
    }
}
