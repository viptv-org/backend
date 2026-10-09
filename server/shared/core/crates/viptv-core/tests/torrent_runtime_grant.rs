use serde_json::{Value, json};
use viptv_core::{
    native_torrent::{NativeTorrentControlOperation, NativeTorrentInput, NativeTorrentPreferences},
    normalize,
    torrent_runtime::*,
};

fn grant() -> TorrentRuntimeGrant {
    let hash = "1111111111111111111111111111111111111111".to_string();
    TorrentRuntimeGrant {
        id: "private_grant_fixture".into(),
        server_time: 1800000000,
        expires_at: 1800000060,
        info_hash: hash.clone(),
        file_index: None,
        archive_index: None,
        input: NativeTorrentInput {
            kind: "magnet".into(),
            value: format!("magnet:?xt=urn:btih:{hash}"),
        },
        trackers: vec!["udp://tracker.example:1337/announce".into()],
        expected_file_size: None,
    }
}

#[test]
fn runtime_grant_preserves_hints_optional_selection_and_private_boundary() {
    let grant = grant();
    let prefs = NativeTorrentPreferences {
        audio_language: None,
        subtitle_language: None,
        subtitles_enabled: false,
    };
    let body = ready_response("playback_fixture", 12.0, &prefs, &grant).unwrap();
    let wire: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(wire["delivery"]["grant"]["version"], 2);
    assert_eq!(wire["delivery"]["grant"]["trackers"], json!(grant.trackers));
    assert!(wire["delivery"]["grant"]["file_index"].is_null());
    assert!(normalize("playbackV2".into(), body, String::new()).is_err());
    assert_eq!(format!("{grant:?}"), "TorrentRuntimeGrant(<redacted>)");
}

#[test]
fn runtime_renewal_cannot_mutate_input_or_selection() {
    let grant = grant();
    let mut renewed = grant.clone();
    renewed.server_time += 20;
    renewed.expires_at += 20;
    assert!(
        validate_transition(&grant, &renewed, NativeTorrentControlOperation::Heartbeat).is_ok()
    );
    assert!(validate_transition(&grant, &renewed, NativeTorrentControlOperation::Poll).is_err());
    renewed.file_index = Some(0);
    assert!(
        validate_transition(&grant, &renewed, NativeTorrentControlOperation::Heartbeat).is_err()
    );
    let mut invalid = grant.clone();
    invalid.trackers[0] = "https://private-user:secret@tracker.example/announce".into();
    assert!(validate_grant(&invalid, invalid.expires_at).is_err());
    invalid = grant.clone();
    invalid.expires_at = grant.server_time + 61;
    assert!(validate_grant(&invalid, invalid.expires_at).is_err());
}

#[test]
fn runtime_capability_is_native_only_and_does_not_reuse_v1_authority() {
    let request = json!({"operation":"playbackV2","playback":{"requestId":"request_fixture","streamId":"source_fixture","client":{
        "platform":"desktop","canPlayDirect":true,"maxWidth":1920,"maxHeight":1080,"videoCodecs":["h264"],"audioCodecs":["aac"],
        "nativeTorrent":{"version":2,"networkPolicy":NETWORK_POLICY}}}});
    assert!(normalize("request".into(), request.to_string(), String::new()).is_ok());
    let mut web = request.clone();
    web["playback"]["client"]["platform"] = json!("web");
    assert!(normalize("request".into(), web.to_string(), String::new()).is_err());
    let context = json!({"origin":"https://server.example/","scope":"scope_fixture","generation":1,"qualified":true,"negotiated":true,"vod":true,
        "request":request["playback"]});
    assert!(viptv_core::NativeTorrentBridge::new(context.to_string()).is_err());
}

#[test]
fn runtime_metainfo_can_exceed_cache_and_select_automatically() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use sha1::{Digest, Sha1};
    // Original byte-only metainfo: a 4 GiB file with 16 MiB pieces. No payload
    // allocation is needed to verify descriptor validation and legacy isolation.
    let mut info =
        b"d6:lengthi4294967296e4:name10:large.data12:piece lengthi16777216e6:pieces5120:".to_vec();
    info.extend(vec![0u8; 5120]);
    info.push(b'e');
    let hash = format!("{:x}", Sha1::digest(&info));
    let mut meta = b"d4:info".to_vec();
    meta.extend(&info);
    meta.push(b'e');
    let mut runtime = grant();
    runtime.info_hash = hash;
    runtime.input = NativeTorrentInput {
        kind: "metainfo".into(),
        value: STANDARD.encode(meta),
    };
    assert!(validate_grant(&runtime, runtime.expires_at).is_ok());
    let legacy = viptv_core::native_torrent::NativeTorrentGrant {
        version: 1,
        network_policy: "public_dht_tcp_v1".into(),
        id: runtime.id,
        server_time: runtime.server_time,
        expires_at: runtime.expires_at,
        info_hash: runtime.info_hash,
        file_index: 0,
        input: runtime.input,
        expected_file_size: None,
    };
    assert!(viptv_core::native_torrent::validate_native_grant(&legacy, legacy.expires_at).is_err());
}
