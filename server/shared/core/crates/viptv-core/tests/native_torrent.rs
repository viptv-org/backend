use serde_json::{Value, json};
use viptv_core::normalize;

fn request() -> Value {
    json!({"operation":"playbackV2","playback":{"requestId":"request_fixture","streamId":"opaque-source","client":{"platform":"android_tv","canPlayDirect":false,"maxWidth":3840,"maxHeight":2160,"videoCodecs":["h264"],"audioCodecs":["aac"],"nativeTorrent":{"version":1,"networkPolicy":"public_dht_tcp_v1"}}}})
}

#[test]
fn native_extension_is_closed_and_omitted_for_legacy_requests() {
    let mut input = request();
    let run = |v: &Value| {
        normalize(
            "request".into(),
            v.to_string(),
            "https://fixture.invalid".into(),
        )
    };
    let output: Value = serde_json::from_str(&run(&input).unwrap()).unwrap();
    assert_eq!(
        output["body"]["client"]["native_torrent"],
        json!({"version":1,"network_policy":"public_dht_tcp_v1"})
    );
    input["playback"]["client"]
        .as_object_mut()
        .unwrap()
        .remove("nativeTorrent");
    let output: Value = serde_json::from_str(&run(&input).unwrap()).unwrap();
    assert!(output["body"]["client"].get("native_torrent").is_none());
}

#[test]
fn negotiation_is_scoped_qualified_android_only_and_auth_refusal_is_not_fallback() {
    let input = json!({"operation":"negotiation","platform":"android_tv","qualified":true,"scopeMatches":true,"status":200,"authorizationRefused":false,"body":"{\"version\":1,\"native_torrent_versions\":[1]}"});
    let value = normalize("nativeTorrent".into(), input.to_string(), String::new()).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&value).unwrap(),
        json!("advertise")
    );
}

#[path = "common/native_torrent_vectors.rs"]
mod vectors;

#[test]
fn raw_native_lease_transition_clock_and_privacy_corpus() {
    assert_eq!(
        vectors::run_vectors().len(),
        vectors::corpus()["cases"].as_array().unwrap().len()
    );
}

#[test]
fn rolling_cache_accepts_large_exact_metadata_without_reserving_the_file_size() {
    use viptv_core::NativeTorrentBridge;
    let root = vectors::corpus();
    for expected in [None, Some(100 * 1024 * 1024 * 1024_u64)] {
        let bridge = NativeTorrentBridge::new(root["context"].to_string()).unwrap();
        let mut body: Value =
            serde_json::from_str(root["cases"][0]["steps"][0]["body"].as_str().unwrap()).unwrap();
        let grant = body["delivery"]["grant"].as_object_mut().unwrap();
        grant.remove("expected_file_size");
        if let Some(size) = expected {
            grant.insert("expected_file_size".into(), json!(size));
        }
        bridge
            .accept(200, body.to_string(), root["observation"].to_string())
            .unwrap();
        assert!(
            bridge
                .metadata_matches_native(
                    root["infoHash"].as_str().unwrap().into(),
                    3,
                    4,
                    100 * 1024 * 1024 * 1024,
                    true,
                    root["clock"].to_string()
                )
                .unwrap()
        );
        assert!(
            !bridge
                .metadata_matches_native(
                    root["infoHash"].as_str().unwrap().into(),
                    2,
                    4,
                    100 * 1024 * 1024 * 1024,
                    true,
                    root["clock"].to_string()
                )
                .unwrap()
        );
        assert!(
            bridge
                .private_input_value(root["clock"].to_string())
                .is_err()
        );
    }
}

#[test]
fn measured_receipt_clock_is_derived_after_strict_raw_validation() {
    use viptv_core::NativeTorrentBridge;
    let root = vectors::corpus();
    let ready = root["cases"][0]["steps"][0]["body"].as_str().unwrap();
    let mut observation = root["observation"].clone();
    observation["trustedWallUpperUnixMillis"] = Value::Null;
    let bridge = NativeTorrentBridge::new(root["context"].to_string()).unwrap();
    let state: Value = serde_json::from_str(
        &bridge
            .accept_measured_bytes(200, ready.as_bytes().to_vec(), observation.to_string())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        bridge.trusted_wall_upper_unix_millis().unwrap(),
        Some(1_700_000_001_500)
    );
    assert_eq!(state["deadlineMillis"], 158_500);
    bridge.invalidate().unwrap();
    assert_eq!(bridge.trusted_wall_upper_unix_millis().unwrap(), None);
    assert!(
        bridge
            .accept_measured_bytes(200, ready.as_bytes().to_vec(), observation.to_string())
            .is_err()
    );
    for bytes in [
        ready.replace("1700000000", "1700000000.0").into_bytes(),
        vec![0xff],
    ] {
        let bridge = NativeTorrentBridge::new(root["context"].to_string()).unwrap();
        assert!(
            bridge
                .accept_measured_bytes(200, bytes, observation.to_string())
                .is_err()
        );
        assert_eq!(bridge.trusted_wall_upper_unix_millis().unwrap(), None);
    }
    observation["roundTripMillis"] = json!(59_000);
    let bridge = NativeTorrentBridge::new(root["context"].to_string()).unwrap();
    assert!(
        bridge
            .accept_measured_bytes(200, ready.as_bytes().to_vec(), observation.to_string())
            .is_err()
    );
}

#[test]
fn native_authorization_scope_keeps_only_the_same_stable_epoch() {
    let scope = json!({"serverOrigin":"https://fixture.invalid", "accountId":"account_fixture", "profileId":"profile_fixture", "deviceAuthorizationEpoch":"epoch_fixture"});
    let decide = |previous: Value, current: Value, revoked: bool| {
        let output = normalize("nativeTorrent".into(), json!({"operation":"authorizationScope","previous":previous,"current":current,"revoked":revoked}).to_string(), String::new()).unwrap();
        serde_json::from_str::<Value>(&output).unwrap()
    };
    assert_eq!(decide(Value::Null, scope.clone(), false), "create");
    let mut canonical_origin = scope.clone();
    canonical_origin["serverOrigin"] = json!("https://FIXTURE.invalid:443/");
    assert_eq!(decide(scope.clone(), canonical_origin, false), "keep");
    for field in [
        "serverOrigin",
        "accountId",
        "profileId",
        "deviceAuthorizationEpoch",
    ] {
        let mut changed = scope.clone();
        changed[field] = json!(if field == "serverOrigin" {
            "https://other.invalid"
        } else {
            "different_fixture"
        });
        assert_eq!(decide(scope.clone(), changed, false), "retire");
    }
    assert_eq!(decide(scope.clone(), Value::Null, false), "retire");
    assert_eq!(decide(scope.clone(), scope.clone(), true), "retire");
    assert_eq!(decide(Value::Null, scope.clone(), true), "reject");
    let mut invalid = scope.clone();
    invalid["serverOrigin"] = json!("http://fixture.invalid");
    assert_eq!(decide(scope, invalid, false), "reject");
}

#[test]
fn backend_admission_compares_caller_source_and_request_resource_sessions() {
    use viptv_core::native_torrent_policy::*;
    let request: viptv_core::dto::PlaybackV2Request =
        serde_json::from_value(request()["playback"].clone()).unwrap();
    let caller = NativeTorrentScope {
        scope_key: "account_profile_fixture".into(),
        session_id: "session_a".into(),
    };
    let other_session = NativeTorrentScope {
        scope_key: caller.scope_key.clone(),
        session_id: "session_b".into(),
    };
    let other_profile = NativeTorrentScope {
        scope_key: "account_other_profile".into(),
        session_id: caller.session_id.clone(),
    };
    let mut facts = NativeTorrentAdmissionFacts {
        caller_scope: &caller,
        source_scope: &caller,
        request_scope: &caller,
        request: &request,
        backend_policy_enabled: true,
        qualified: true,
        negotiated: true,
        resource_authorized: true,
        source_proof_current: true,
        exact_vod: true,
        info_hash: Some("0000000000000000000000000000000000000000"),
        file_index: Some(3),
    };
    assert_eq!(
        native_admission(&facts),
        NativeTorrentAdmissionDecision::Native
    );
    facts.source_scope = &other_session;
    assert_eq!(
        native_admission(&facts),
        NativeTorrentAdmissionDecision::RefuseAuthorization
    );
    facts.source_scope = &caller;
    facts.request_scope = &other_session;
    assert_eq!(
        native_admission(&facts),
        NativeTorrentAdmissionDecision::RefuseAuthorization
    );
    facts.request_scope = &caller;
    facts.source_scope = &other_profile;
    assert_eq!(
        native_admission(&facts),
        NativeTorrentAdmissionDecision::RefuseAuthorization
    );
    facts.source_scope = &caller;
    facts.source_proof_current = false;
    assert_eq!(
        native_admission(&facts),
        NativeTorrentAdmissionDecision::RefuseSelection
    );
    facts.source_proof_current = true;
    facts.file_index = None;
    assert_eq!(
        native_admission(&facts),
        NativeTorrentAdmissionDecision::Gateway
    );
    assert_eq!(
        format!("{facts:?}"),
        "NativeTorrentAdmissionFacts(<redacted>)"
    );
}

#[test]
fn backend_idempotency_is_session_scoped_and_tombstones_never_reopen() {
    use viptv_core::native_torrent_policy::*;
    let request: viptv_core::dto::PlaybackV2Request =
        serde_json::from_value(request()["playback"].clone()).unwrap();
    let existing = NativeTorrentRequestIdentity {
        scope: NativeTorrentScope {
            scope_key: "account_profile_fixture".into(),
            session_id: "session_a".into(),
        },
        request,
    };
    let mut incoming = existing.clone();
    assert_eq!(
        native_request_transition(Some(&existing), &incoming, false),
        NativeTorrentRequestDecision::Idempotent
    );
    incoming.scope.session_id = "session_b".into();
    assert_eq!(
        native_request_transition(Some(&existing), &incoming, false),
        NativeTorrentRequestDecision::DifferentScope
    );
    incoming = existing.clone();
    incoming.scope.scope_key = "other_profile".into();
    assert_eq!(
        native_request_transition(Some(&existing), &incoming, false),
        NativeTorrentRequestDecision::DifferentScope
    );
    incoming = existing.clone();
    incoming.request.position = 1.0;
    assert_eq!(
        native_request_transition(Some(&existing), &incoming, false),
        NativeTorrentRequestDecision::Conflict
    );
    assert_eq!(
        native_request_transition(None, &existing, true),
        NativeTorrentRequestDecision::Cancelled
    );
    assert_eq!(
        native_request_transition(Some(&existing), &existing, true),
        NativeTorrentRequestDecision::Cancelled
    );
    assert_eq!(
        format!("{existing:?}"),
        "NativeTorrentRequestIdentity(<redacted>)"
    );
}

#[test]
fn private_typed_grants_are_redacted_and_http_emission_round_trips() {
    use viptv_core::native_torrent::*;
    use viptv_core::native_torrent_policy::*;
    let root = vectors::corpus();
    let grant = NativeTorrentGrant {
        version: 1,
        network_policy: "public_dht_tcp_v1".into(),
        id: "grant_fixture".into(),
        server_time: 1700000000,
        expires_at: 1700000060,
        info_hash: "0000000000000000000000000000000000000000".into(),
        file_index: 3,
        input: NativeTorrentInput {
            kind: "magnet".into(),
            value: "magnet:?xt=urn:btih:0000000000000000000000000000000000000000".into(),
        },
        expected_file_size: None,
    };
    assert_eq!(format!("{grant:?}"), "NativeTorrentGrant(<redacted>)");
    assert_eq!(
        format!("{:?}", grant.input),
        "NativeTorrentInput(<redacted>)"
    );
    let body = native_ready_response(
        "playback_fixture",
        120.0,
        &NativeTorrentPreferences {
            audio_language: None,
            subtitle_language: None,
            subtitles_enabled: false,
        },
        &grant,
    )
    .unwrap();
    let value: Value = serde_json::from_str(&body).unwrap();
    assert!(
        value["delivery"]["grant"]
            .get("expected_file_size")
            .is_none()
    );
    let bridge = NativeTorrentBridge::new(root["context"].to_string()).unwrap();
    bridge
        .accept(200, body, root["observation"].to_string())
        .unwrap();
    assert_eq!(
        bridge
            .private_file_index(root["clock"].to_string())
            .unwrap(),
        3
    );
    let scope = NativeTorrentScope {
        scope_key: "scope".into(),
        session_id: "session_a".into(),
    };
    let other = NativeTorrentScope {
        scope_key: "scope".into(),
        session_id: "session_b".into(),
    };
    let mut next = grant.clone();
    next.server_time += 20;
    next.expires_at += 20;
    assert!(
        validate_native_transition(
            &grant,
            &next,
            NativeTorrentControlOperation::Heartbeat,
            &scope,
            &scope
        )
        .is_ok()
    );
    assert!(
        validate_native_transition(
            &grant,
            &next,
            NativeTorrentControlOperation::Heartbeat,
            &scope,
            &other
        )
        .is_err()
    );
    assert!(
        validate_native_transition(
            &grant,
            &next,
            NativeTorrentControlOperation::Poll,
            &scope,
            &scope
        )
        .is_err()
    );
    next.file_index = 2;
    assert!(
        validate_native_transition(
            &grant,
            &next,
            NativeTorrentControlOperation::Heartbeat,
            &scope,
            &scope
        )
        .is_err()
    );
}
