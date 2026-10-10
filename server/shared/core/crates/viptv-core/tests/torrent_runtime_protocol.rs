use serde_json::{Value, json};
use viptv_core::normalize;

#[test]
fn runtime_protocol_is_separate_and_closed_without_changing_v1() {
    let parse = |kind: &str, body: &str| normalize(kind.into(), body.into(), String::new());
    let v1 = r#"{"version":1,"native_torrent_versions":[1]}"#;
    let v2 = r#"{"version":2,"native_torrent_versions":[2]}"#;
    assert!(parse("playbackProtocolV2", v1).is_ok());
    assert!(parse("playbackProtocolV2", v2).is_err());
    assert!(parse("torrentRuntimeProtocol", v1).is_err());
    let accepted: Value =
        serde_json::from_str(&parse("torrentRuntimeProtocol", v2).unwrap()).unwrap();
    assert_eq!(accepted, json!({"version":2,"nativeTorrentVersions":[2]}));
    for body in [
        r#"{"version":2.0,"native_torrent_versions":[2]}"#,
        r#"{"version":2,"native_torrent_versions":[2.0]}"#,
        r#"{"version":2,"version":2,"native_torrent_versions":[2]}"#,
        r#"{"version":2,"native_torrent_versions":[1,2]}"#,
        r#"{"version":2,"native_torrent_versions":[2],"extra":true}"#,
    ] {
        assert!(parse("torrentRuntimeProtocol", body).is_err());
    }
}

#[test]
fn runtime_protocol_request_is_bodyless_and_rejects_extra_fields() {
    let request = |body: &str| normalize("request".into(), body.into(), String::new());
    let parsed: Value =
        serde_json::from_str(&request(r#"{"operation":"torrentRuntimeProtocol"}"#).unwrap())
            .unwrap();
    assert_eq!(parsed["method"], "GET");
    assert_eq!(parsed["path"], "/api/v2/torrent-runtime-protocol");
    assert!(parsed["body"].is_null());
    assert!(request(r#"{"operation":"torrentRuntimeProtocol","body":{}}"#).is_err());
    assert!(
        request(r#"{"operation":"torrentRuntimeProtocol","operation":"torrentRuntimeProtocol"}"#)
            .is_err()
    );
}
