use serde_json::{Value, json};
use viptv_core::normalize;

fn run(kind: &str, value: &Value) -> Result<Value, viptv_core::CoreError> {
    normalize(
        kind.into(),
        value.to_string(),
        "https://backend.example".into(),
    )
    .map(|text| serde_json::from_str(&text).unwrap())
}
fn lease() -> Value {
    json!({"id":"pb2_fixture","status":"ready","expires_at":1800000060_u64,"renew_after_seconds":20,
      "delivery":{"kind":"gateway","url":"https://gateway.example/base/media/viewer/cap/index.m3u8","format":"hls","mode":"direct","video_mode":"copy","audio_mode":"copy","position":120,"duration":3600,"live":false,"audio_tracks":[],"subtitle_tracks":[],"subtitles_supported":false}})
}

#[test]
fn gateway_delivery_keeps_authority_timeline_and_lease_separate() {
    let mut value = lease();
    value["delivery"]["preferences"] = json!({"quality":"1080p"});
    value["integration_key"] = json!("must-not-escape");
    let out = run("playbackV2", &value).unwrap();
    assert_eq!(out["expiresAt"], 1800000060000_u64);
    assert_eq!(out["renewAfterSeconds"], 20);
    assert_eq!(out["session"]["id"], "pb2_fixture");
    assert_eq!(out["session"]["deliveryKind"], "gateway");
    assert_eq!(out["session"]["mode"], "direct");
    assert_eq!(out["session"]["position"], 120.0);
    assert_eq!(out["session"]["duration"], 3600.0);
    assert!(out["session"]["maximumHeight"].is_null());
    assert!(out["session"]["authorization"].is_null());
    assert!(!out.to_string().contains("must-not-escape"));
    let typed: viptv_core::dto::PlaybackLease = serde_json::from_value(out).unwrap();
    assert!(typed.session.is_some());
}

#[test]
fn direct_http_keeps_only_valid_source_headers() {
    let mut value = lease();
    value["delivery"] = json!({"kind":"direct","url":"http://provider.example/movie/user/pass/1.mp4","format":"original","position":30,"live":false,"headers":{"Cookie":"source=session","User-Agent":"Native client","Authorization":"Bearer upstream","Referer":"http://provider.example/"}});
    let out = run("playbackV2", &value).unwrap();
    assert_eq!(out["session"]["deliveryKind"], "direct");
    assert_eq!(out["session"]["authorization"]["cookie"], "source=session");
    assert_eq!(
        out["session"]["authorization"]["headers"]["Authorization"],
        "Bearer upstream"
    );
    for headers in [
        json!({"Host":"backend.example"}),
        json!({"X-Test":"a\r\nInjected: x"}),
        json!({"Connection":"keep-alive"}),
        json!({"Cookie":false}),
    ] {
        value["delivery"]["headers"] = headers;
        assert!(run("playbackV2", &value).is_err());
    }
}

#[test]
fn pending_and_terminal_leases_never_expose_delivery_or_raw_errors() {
    for status in ["starting", "failed", "expired", "released"] {
        let mut value = lease();
        value["status"] = json!(status);
        value["error"] = json!("http://provider.example/private-token");
        value["error_code"] = if status == "failed" {
            json!("provider_connection_limit")
        } else {
            Value::Null
        };
        let out = run("playbackV2", &value).unwrap();
        assert!(out["session"].is_null());
        assert!(!out.to_string().contains("private-token"));
        if status == "failed" {
            assert!(out["error"].as_str().unwrap().contains("connection limit"));
        }
        if status == "expired" {
            assert_eq!(out["errorCode"], "playback_expired");
        }
    }
}

#[test]
fn malformed_gateway_delivery_and_lease_fields_fail_closed() {
    for url in [
        "http://gateway.example/media/cap/index.m3u8",
        "/media/cap/index.m3u8",
        "https://user:pass@gateway.example/media/cap/index.m3u8",
        "file:///etc/passwd",
        "https://gateway.example/media/index.m3u8#secret",
    ] {
        let mut value = lease();
        value["delivery"]["url"] = json!(url);
        assert!(run("playbackV2", &value).is_err());
    }
    for (field, bad) in [
        ("status", json!("unknown")),
        ("renew_after_seconds", json!(0)),
        ("expires_at", json!(-1)),
        ("id", json!("../../private")),
    ] {
        let mut value = lease();
        value[field] = bad;
        assert!(run("playbackV2", &value).is_err());
    }
    for (field, bad) in [
        ("kind", json!("proxy")),
        ("position", json!(-1)),
        ("duration", json!(-1)),
        ("headers", json!({"Authorization":"backend-token"})),
        ("authorization", json!({"cookie":"backend-session"})),
        ("audio_tracks", json!([{"input_index":-1}])),
    ] {
        let mut value = lease();
        value["delivery"][field] = bad;
        assert!(run("playbackV2", &value).is_err());
    }
}

#[test]
fn canonical_v2_request_preserves_4k_facts_and_rejects_legacy_options() {
    let playback = json!({"requestId":"request_1","streamId":"opaque-source","client":{"platform":"android_tv","canPlayDirect":true,"maxWidth":3840,"maxHeight":2160,"videoCodecs":["h264","hevc"],"audioCodecs":["aac"]},"position":120,"forceGateway":true,"audioTrack":2,"subtitleTrack":null});
    let mut input = json!({"operation":"playbackV2","playback":playback});
    let out = run("request", &input).unwrap();
    assert_eq!(out["path"], "/api/v2/playback");
    assert_eq!(out["body"]["client"]["max_height"], 2160);
    assert_eq!(out["body"]["client"]["platform"], "android_tv");
    assert_eq!(out["body"]["request_id"], "request_1");
    assert_eq!(out["body"]["audio_track"], 2);
    input["playback"]["forceTranscode"] = json!(true);
    assert!(
        run("request", &input).is_err(),
        "do not silently discard unsupported options"
    );
    for (operation, method, suffix) in [
        ("playbackV2Status", "GET", ""),
        ("playbackV2Heartbeat", "POST", "/heartbeat"),
        ("playbackV2Stop", "DELETE", ""),
    ] {
        let out = run(
            "request",
            &json!({"operation":operation,"id":"pb2_fixture"}),
        )
        .unwrap();
        assert_eq!(out["method"], method);
        assert_eq!(out["path"], format!("/api/v2/playback/pb2_fixture{suffix}"));
    }
}

#[test]
fn player_intent_maps_conversion_tracks_and_real_decoder_facts_once() {
    let mut input = json!({"requestId":"request_1","platform":"tauri","preferences":{"audioLanguage":"pt-BR","subtitleLanguage":"en","subtitlesEnabled":true,"quality":"1080p"},"playback":{"streamId":"source","position":20,"capabilities":{"maxWidth":3840,"maxHeight":2160,"h264":true,"hevc":true,"aac":true,"directUrls":true},"forceTranscode":true,"conversionReason":"audio-codec","audioTrackIndex":2,"subtitleTrackIndex":3,"subtitlesOff":true}});
    let out = run("playbackV2Intent", &input).unwrap();
    assert_eq!(out["client"]["platform"], "desktop");
    assert_eq!(out["client"]["maxHeight"], 2160);
    assert_eq!(out["client"]["videoCodecs"], json!(["h264", "hevc"]));
    assert_eq!(out["conversion"], "audio");
    assert_eq!(out["forceGateway"], true);
    assert_eq!(out["audioTrack"], 2);
    assert!(out["subtitleTrack"].is_null());
    assert!(out["preferredSubtitleLanguage"].is_null());
    assert_eq!(out["preferredAudioLanguage"], "pt-BR");
    assert!(out.get("quality").is_none());
    let wire = run("request", &json!({"operation":"playbackV2","playback":out})).unwrap();
    assert_eq!(wire["body"]["conversion"], "audio");
    for (reason, conversion) in [
        (json!("video-codec"), "video"),
        (Value::Null, "audio_video"),
    ] {
        input["playback"]["conversionReason"] = reason;
        assert_eq!(
            run("playbackV2Intent", &input).unwrap()["conversion"],
            conversion
        );
    }
    input["playback"]["conversionReason"] = json!("network");
    assert!(run("playbackV2Intent", &input).is_err());
    input["playback"]["forceTranscode"] = json!(false);
    input["platform"] = json!("vizio");
    assert_eq!(
        run("playbackV2Intent", &input).unwrap()["client"]["canPlayDirect"],
        false
    );
}

#[test]
fn direct_language_preferences_survive_without_restoring_a_quality_cap() {
    let mut value = lease();
    value["delivery"] = json!({"kind":"direct","url":"http://provider.example/movie.mp4","format":"original","headers":{},"position":0,"live":false,"preferences":{"audio_language":"en","subtitle_language":"pt-BR","subtitles_enabled":true,"quality":"1080p"}});
    let out = run("playbackV2", &value).unwrap();
    assert_eq!(out["session"]["preferredAudioLanguage"], "en");
    assert_eq!(out["session"]["preferredSubtitleLanguage"], "pt-BR");
    assert!(out["session"]["maximumHeight"].is_null());
    value["delivery"]["preferences"]["audio_language"] = json!("http://private.invalid/credential");
    assert!(run("playbackV2", &value).is_err());
}
