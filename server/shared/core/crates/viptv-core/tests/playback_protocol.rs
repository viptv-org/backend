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

#[test]
fn gateway_clock_offset_round_trips_and_invalid_or_double_seek_clocks_fail_closed() {
    let mut lease = json!({"id":"clock_fixture","status":"ready","expires_at":1800000060_u64,"renew_after_seconds":20,"delivery":{"kind":"gateway","url":"https://gateway.example/media/viewer/token/index.m3u8","format":"hls","mode":"remux","video_mode":"copy","audio_mode":"copy","position":0.0,"timeline_offset_seconds":48.6,"duration":600.0,"live":false,"audio_tracks":[],"subtitle_tracks":[],"selected_audio":null,"selected_subtitle":null,"subtitles_supported":false}});
    let decode = |value: &Value| {
        normalize(
            "playbackV2".into(),
            value.to_string(),
            "https://fixture.invalid".into(),
        )
    };
    let value: Value = serde_json::from_str(&decode(&lease).unwrap()).unwrap();
    assert_eq!(value["session"]["timelineOffsetSeconds"], 48.6);
    assert_eq!(value["session"]["position"], 0.0);
    assert_eq!(value["session"]["duration"], 600.0);
    lease["delivery"]["position"] = json!(48.6);
    assert!(decode(&lease).is_err());
    lease["delivery"]["position"] = json!(0.0);
    lease["delivery"]["timeline_offset_seconds"] = json!(-1.0);
    assert!(decode(&lease).is_err());
    lease["delivery"]["timeline_offset_seconds"] = json!(1e10);
    assert!(decode(&lease).is_err());
}

#[test]
fn gateway_native_mp4_preserves_one_client_seek_without_a_source_cut_offset() {
    let mut lease=json!({"id":"mp4_fixture","status":"ready","expires_at":1800000060_u64,"renew_after_seconds":20,"delivery":{"kind":"gateway","url":"https://gateway.example/media/viewer/token/source.mp4","format":"mp4","mode":"direct","video_mode":"copy","audio_mode":"copy","position":48.6,"timeline_offset_seconds":0.0,"duration":600.0,"live":false,"audio_tracks":[],"subtitle_tracks":[],"selected_audio":null,"selected_subtitle":null,"subtitles_supported":false}});
    let decode=|value:&Value|normalize("playbackV2".into(),value.to_string(),"https://fixture.invalid".into());
    let value:Value=serde_json::from_str(&decode(&lease).unwrap()).unwrap();
    assert_eq!(value["session"]["format"],"mp4");assert_eq!(value["session"]["position"],48.6);assert_eq!(value["session"]["timelineOffsetSeconds"],0.0);
    lease["delivery"]["timeline_offset_seconds"]=json!(48.6);assert!(decode(&lease).is_err());
    lease["delivery"]["timeline_offset_seconds"]=json!(0.0);lease["delivery"]["mode"]=json!("transcode");assert!(decode(&lease).is_err());
}
