use serde_json::{Value, json};
use viptv_core::{CoreError, normalize};

fn vectors() -> Vec<Value> {
    serde_json::from_str(include_str!(
        "../../../tests/playback-protocol-vectors.json"
    ))
    .unwrap()
}

fn run(vector: &Value) -> Result<Value, CoreError> {
    normalize(
        vector["kind"].as_str().unwrap().into(),
        vector["input"].as_str().unwrap().into(),
        "https://fixture.invalid".into(),
    )
    .map(|text| serde_json::from_str(&text).unwrap())
}

#[test]
fn closed_protocol_and_bodyless_request_success_vectors() {
    for vector in vectors().iter().filter(|v| v.get("output").is_some()) {
        assert_eq!(run(vector).unwrap(), vector["output"], "{}", vector["name"]);
    }
}

#[test]
fn malformed_protocol_and_requests_fail_with_static_errors() {
    for vector in vectors().iter().filter(|v| v.get("error").is_some()) {
        let error = run(vector).expect_err(vector["name"].as_str().unwrap());
        assert_eq!(error.to_string(), vector["error"], "{}", vector["name"]);
        assert!(matches!(error, CoreError::InvalidInput));
    }
}

#[test]
fn legacy_models_do_not_project_private_grants() {
    let playback = json!({"requestId":"request_fixture","streamId":"opaque-source","client":{"platform":"android_tv","canPlayDirect":true,"maxWidth":3840,"maxHeight":2160,"videoCodecs":["h264"],"audioCodecs":["aac"]},"position":120});
    let invoke = |kind: &str, value: Value| {
        normalize(
            kind.into(),
            value.to_string(),
            "https://fixture.invalid".into(),
        )
    };
    let out: Value = serde_json::from_str(
        &invoke(
            "request",
            json!({"operation":"playbackV2","playback":playback}),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        out["body"]["client"],
        json!({"platform":"android_tv","can_play_direct":true,"max_width":3840,"max_height":2160,"video_codecs":["h264"],"audio_codecs":["aac"]})
    );
    let mut native = playback;
    native["client"]["nativeTorrent"] = json!({"version":1,"networkPolicy":"public_dht_tcp_v1"});
    assert!(
        invoke(
            "request",
            json!({"operation":"playbackV2","playback":native})
        )
        .is_ok()
    );
    assert!(invoke("playbackV2", json!({"id":"playback_fixture","status":"ready","expires_at":1800000060_u64,"renew_after_seconds":20,"delivery":{"kind":"native_torrent","grant":{}}})).is_err());
}
