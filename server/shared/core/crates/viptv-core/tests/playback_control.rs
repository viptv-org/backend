use serde_json::{Value, json};
use viptv_core::{normalize, policy::playback_control};

fn run(operation: &str, mut input: Value) -> Value {
    input["operation"] = json!(operation);
    let expected = playback_control::normalize(&input).unwrap();
    let wire = normalize("playbackControl".into(), input.to_string(), "".into()).unwrap();
    let actual: Value = serde_json::from_str(&wire).unwrap();
    assert_eq!(actual, expected);
    actual
}
fn timeline(mode: &str) -> Value {
    json!({"deliveryMode":mode,"launchPositionMillis":42000,"segmentPositionMillis":3000,"titleOffsetMillis":42000,"titlePositionMillis":47000,"nativeDurationMillis":78000,"titleDurationMillis":120000,"pauseAnchorMillis":null,"playerError":false,"trustedPositionMillis":44000})
}
fn lease() -> Value {
    json!({"expectedId":"request_lease","actualId":"request_lease","status":"ready","hasSession":true,"expiresAtMillis":20000,"nowMillis":10000,"heartbeat":false,"sameDeliveryUrl":true,"sameDeliveryKind":true})
}
fn page() -> Value {
    json!({"catalogId":"2","generation":"0","ids":["z","a"],"names":["Zulu","Alpha"],"categories":true,"nextCursor":"opaque_next","previousCursor":"opaque_previous","requestedCatalogId":null,"limit":200,"checkSnapshot":false,"snapshotCatalogId":null,"snapshotGeneration":null,"knownIds":[],"cursor":null,"previous":false,"extendingWindow":false})
}

#[test]
fn title_coordinates_duration_and_trusted_error_precedence() {
    let direct = run("timeline", timeline("direct"));
    assert_eq!(direct["launchOffsetMillis"], 0);
    assert_eq!(direct["durationMillis"], 78000);
    let managed = run("timeline", timeline("managed"));
    assert_eq!(managed["launchOffsetMillis"], 42000);
    assert_eq!(managed["positionMillis"], 45000);
    assert_eq!(managed["segmentPositionMillis"], 5000);
    assert_eq!(managed["durationMillis"], 120000);
    let mut facts = timeline("managed");
    facts["playerError"] = json!(true);
    assert_eq!(run("timeline", facts.clone())["positionMillis"], 44000);
    assert_eq!(
        run("timeline", facts.clone())["updateTrustedPosition"],
        false
    );
    facts["pauseAnchorMillis"] = json!(43000);
    assert_eq!(run("timeline", facts.clone())["positionMillis"], 43000);
    facts["pauseAnchorMillis"] = Value::Null;
    facts["trustedPositionMillis"] = json!(0);
    facts["nativeDurationMillis"] = Value::Null;
    assert_eq!(
        run("timeline", facts.clone())["updateTrustedPosition"],
        true
    );
    facts["titlePositionMillis"] = json!(2000);
    assert_eq!(run("timeline", facts)["segmentPositionMillis"], 0);
    let mut direct_fallback = timeline("direct");
    direct_fallback["nativeDurationMillis"] = Value::Null;
    assert_eq!(run("timeline", direct_fallback)["durationMillis"], 120000);
    let mut managed_fallback = timeline("managed");
    managed_fallback["titleDurationMillis"] = Value::Null;
    assert_eq!(run("timeline", managed_fallback)["durationMillis"], 78000);
}

#[test]
fn seek_preview_dvr_bounds_noop_and_commit_delivery_are_independent_of_processing() {
    let mut facts = json!({"currentMillis":12000,"deltaMillis":-15000,"durationMillis":null,"rangeStartMillis":10000,"rangeEndMillis":20000});
    assert_eq!(run("seekPreview", facts.clone()), 10000);
    facts["deltaMillis"] = json!(50000);
    assert_eq!(run("seekPreview", facts.clone()), 20000);
    facts["deltaMillis"] = json!(499);
    assert_eq!(run("seekPreview", facts.clone()), Value::Null);
    facts["deltaMillis"] = json!(500);
    assert_eq!(run("seekPreview", facts.clone()), 12500);
    facts["durationMillis"] = json!(120000);
    facts["deltaMillis"] = json!(50000);
    assert_eq!(run("seekPreview", facts.clone()), 62000);
    facts["durationMillis"] = Value::Null;
    facts["rangeStartMillis"] = Value::Null;
    assert_eq!(run("seekPreview", facts), Value::Null);
    assert_eq!(run("seekCommit", json!({"deliveryMode":"DIRECT"})), false);
    assert_eq!(
        run(
            "seekCommit",
            json!({"deliveryMode":"managed","processingMode":"direct"})
        ),
        true
    );
}

#[test]
fn paused_managed_seek_preserves_anchor_and_resumes_by_replacement_not_live_edge() {
    let mut facts = json!({"deliveryMode":"managed","live":false,"anchorMillis":42000,"launchPositionMillis":55000,"playWhenReady":false});
    let decision = run("pause", facts.clone());
    assert_eq!(decision["usesAnchor"], true);
    assert_eq!(decision["replaceOnResume"], true);
    assert_eq!(decision["anchorAfterOpenMillis"], 55000);
    facts["live"] = json!(true);
    assert_eq!(run("pause", facts.clone())["usesAnchor"], false);
    facts["live"] = json!(false);
    facts["deliveryMode"] = json!("direct");
    assert_eq!(run("pause", facts)["replaceOnResume"], false);
    for (managed, network, attempted, expected) in [
        (true, true, false, true),
        (false, true, false, false),
        (true, false, false, false),
        (true, true, true, false),
    ] {
        assert_eq!(
            run(
                "recovery",
                json!({"serverManaged":managed,"networkFailure":network,"alreadyAttempted":attempted})
            ),
            expected
        );
    }
}

#[test]
fn direct_requires_requested_capability_and_gateway_conversion_remains_managed() {
    let mut facts = json!({"directDelivery":true,"canPlayDirect":true,"forceGateway":false,"automaticConversion":true});
    assert_eq!(run("delivery", facts.clone()), true);
    for field in ["canPlayDirect", "automaticConversion"] {
        let mut invalid = facts.clone();
        invalid[field] = json!(false);
        assert_eq!(run("delivery", invalid), false);
    }
    facts["forceGateway"] = json!(true);
    assert_eq!(run("delivery", facts.clone()), false);
    facts["directDelivery"] = json!(false);
    assert_eq!(run("delivery", facts), true);
}

#[test]
fn mismatched_lease_terminal_expiry_pending_and_heartbeat_invariants() {
    assert_eq!(run("lease", lease()), "ready");
    let mut facts = lease();
    facts["actualId"] = json!("another");
    assert_eq!(run("lease", facts), "invalid");
    for status in ["failed", "expired", "released"] {
        let mut facts = lease();
        facts["status"] = json!(status);
        assert_eq!(run("lease", facts), "terminal");
    }
    let mut facts = lease();
    facts["expiresAtMillis"] = json!(10000);
    assert_eq!(run("lease", facts), "expired");
    let mut fractional_expiry = lease();
    fractional_expiry["expiresAtMillis"] = json!(10000.5);
    assert_eq!(run("lease", fractional_expiry), "ready");
    let mut facts = lease();
    facts["status"] = json!("starting");
    assert_eq!(run("lease", facts.clone()), "pending");
    facts["heartbeat"] = json!(true);
    assert_eq!(run("lease", facts), "invalid");
    for field in ["sameDeliveryUrl", "sameDeliveryKind", "hasSession"] {
        let mut facts = lease();
        facts["heartbeat"] = json!(true);
        facts[field] = json!(false);
        assert_eq!(run("lease", facts), "invalid");
    }
    // Whether a validated late heartbeat still belongs to the active entry is a native CAS decision.
    let mut facts = lease();
    facts["heartbeat"] = json!(true);
    assert_eq!(run("lease", facts), "ready");
}

#[test]
fn authority_is_bounded_by_wall_expiry_and_injected_observation_cap() {
    let mut facts = json!({"expiresAtMillis":100000,"nowMillis":20000,"elapsedMillis":55000,"observationCapMillis":60000,"waitMillis":20000});
    let budget = run("authority", facts.clone());
    assert_eq!(budget["remainingMillis"], 5000);
    assert_eq!(budget["delayMillis"], 5000);
    facts["observationCapMillis"] = json!(120000);
    assert_eq!(run("authority", facts.clone())["remainingMillis"], 65000);
    facts["expiresAtMillis"] = json!(23000);
    assert_eq!(run("authority", facts.clone())["remainingMillis"], 3000);
    facts["nowMillis"] = json!(24000);
    assert_eq!(run("authority", facts)["remainingMillis"], 0);
}

#[test]
fn terminal_transient_and_exact_request_reconciliation_classification() {
    for (gateway, status, invalid, io, retry, reconcile) in [
        (true, 409, false, false, false, false),
        (true, 408, false, false, true, true),
        (true, 503, false, false, true, true),
        (true, 502, true, false, false, true),
        (false, 0, false, true, true, true),
        (false, 0, false, false, false, true),
    ] {
        let facts = json!({"gatewayError":gateway,"status":status,"invalidResponse":invalid,"ioError":io,"hasLeaseId":false});
        let decision = run("failure", facts.clone());
        assert_eq!(decision["retryRenewal"], retry);
        assert_eq!(decision["reconcileAndRelease"], reconcile);
        let mut owned = facts;
        owned["hasLeaseId"] = json!(true);
        assert_eq!(run("failure", owned)["reconcileAndRelease"], true);
    }
}

#[test]
fn live_request_scope_snapshot_overlap_cursor_and_empty_initial_page() {
    assert_eq!(run("livePage", page()), "valid");
    let mut facts = page();
    facts["ids"] = json!(["a", "a"]);
    assert_eq!(run("livePage", facts), "invalid");
    let mut limited = page();
    limited["limit"] = json!(1);
    assert_eq!(run("livePage", limited), "invalid");
    let mut facts = page();
    facts["requestedCatalogId"] = json!("3");
    assert_eq!(run("livePage", facts), "invalid");
    let mut facts = page();
    facts["checkSnapshot"] = json!(true);
    facts["snapshotCatalogId"] = json!("2");
    facts["snapshotGeneration"] = json!("1");
    assert_eq!(run("livePage", facts), "catalog_changed");
    let mut facts = page();
    facts["cursor"] = json!("opaque_request");
    facts["knownIds"] = json!(["a"]);
    assert_eq!(run("livePage", facts), "invalid");
    let mut facts = page();
    facts["cursor"] = json!("opaque_next");
    assert_eq!(run("livePage", facts.clone()), "invalid");
    facts["previous"] = json!(true);
    assert_eq!(run("livePage", facts.clone()), "valid");
    facts["cursor"] = json!("opaque_previous");
    assert_eq!(run("livePage", facts), "invalid");
    let mut facts = page();
    facts["ids"] = json!([]);
    facts["names"] = json!([]);
    facts["nextCursor"] = Value::Null;
    facts["previousCursor"] = Value::Null;
    assert_eq!(run("livePage", facts.clone()), "valid");
    facts["extendingWindow"] = json!(true);
    assert_eq!(run("livePage", facts), "catalog_changed");
    let mut empty_channel_page = page();
    empty_channel_page["categories"] = json!(false);
    empty_channel_page["ids"] = json!([]);
    empty_channel_page["names"] = json!([]);
    // Initial channel-page cursor semantics remain distinct from category continuation checks.
    assert_eq!(run("livePage", empty_channel_page), "valid");
}

#[test]
fn live_wire_requires_explicit_reverse_cursor_and_preserves_provider_order() {
    let raw = json!({"catalog_id":2,"generation":0,"items":[{"id":"z","name":"Zulu"},{"id":"a","name":"Alpha"}],"next_cursor":"opaque_next","previous_cursor":null});
    let normalize =
        |raw: Value| viptv_core::normalize("liveCategoriesV2".into(), raw.to_string(), "".into());
    let out: Value = serde_json::from_str(&normalize(raw.clone()).unwrap()).unwrap();
    assert_eq!(out["items"][0]["id"], "z");
    assert_eq!(out["items"][1]["id"], "a");
    let mut missing = raw.clone();
    missing.as_object_mut().unwrap().remove("previous_cursor");
    assert!(normalize(missing).is_err());
    let mut repeated = raw.clone();
    repeated["items"][1]["id"] = json!("z");
    assert!(normalize(repeated).is_err());
    let mut empty = raw;
    empty["items"] = json!([]);
    empty["next_cursor"] = Value::Null;
    assert!(normalize(empty).is_ok());
    for kind in ["liveCatalogV2", "liveCategoriesV2"] {
        let raw = json!({"catalog_id":null,"generation":null,"items":[],"next_cursor":null});
        assert!(viptv_core::normalize(kind.into(), raw.to_string(), "".into()).is_err());
        let duplicate = json!({"catalog_id":2,"generation":0,"items":[{"id":"a","name":"One"},{"id":"a","name":"Two"}],"next_cursor":null,"previous_cursor":null});
        assert!(viptv_core::normalize(kind.into(), duplicate.to_string(), "".into()).is_err());
    }
}
