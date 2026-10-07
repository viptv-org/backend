use serde_json::{Value, json};
use viptv_core::normalize;

fn run(kind: &str, input: Value) -> Value {
    serde_json::from_str(&normalize(kind.into(), input.to_string(), String::new()).unwrap())
        .unwrap()
}

fn vectors() -> Vec<Value> {
    serde_json::from_str(include_str!("../../../tests/playback-error-vectors.json")).unwrap()
}

fn failed_lease(code: Value) -> Value {
    json!({
        "id": "pb2_error_fixture",
        "status": "failed",
        "expires_at": 1800000060_u64,
        "renew_after_seconds": 20,
        "error_code": code,
        "error": "https://provider.invalid/private-token Authorization: Bearer private-token",
        "delivery": {"url": "https://provider.invalid/private-token", "headers": {"Cookie": "private-token"}}
    })
}

#[test]
fn known_playback_errors_have_canonical_copy_independent_of_http_status_or_raw_text() {
    for vector in vectors() {
        for status in [vector["status"].as_u64().unwrap(), 502] {
            for raw in [
                "A different upstream explanation",
                "https://private.invalid/token",
            ] {
                let out = run(
                    "apiError",
                    json!({"status": status, "error_code": vector["code"], "error": raw}),
                );
                assert_eq!(
                    out,
                    json!({"code": vector["code"], "message": vector["message"]})
                );
            }
        }
    }
}

#[test]
fn failed_playback_leases_preserve_the_same_safe_reason_as_http_errors() {
    for vector in vectors() {
        let out = run("playbackV2", failed_lease(vector["code"].clone()));
        assert_eq!(out["errorCode"], vector["code"]);
        assert_eq!(out["error"], vector["message"]);
        assert!(out["session"].is_null());
        assert!(!out.to_string().contains("private-token"));
        let typed: viptv_core::dto::PlaybackLease = serde_json::from_value(out).unwrap();
        assert!(typed.session.is_none());
    }
}

#[test]
fn unknown_and_malformed_reasons_never_promote_upstream_diagnostics() {
    let fallback =
        "The server or provider is temporarily unavailable. Try again or choose another source.";
    for (code, expected) in [
        (json!("future_gateway_failure"), "future_gateway_failure"),
        (json!("https://private.invalid/token"), ""),
        (json!("gateway_startup_timeout\nprivate-token"), ""),
        (json!("x".repeat(65)), ""),
        (Value::Null, "playback_failed"),
    ] {
        let out = run("playbackV2", failed_lease(code));
        assert_eq!(out["errorCode"], expected);
        if expected == "playback_failed" {
            assert_eq!(
                out["error"],
                "The selected source could not start. Try another source or retry playback."
            );
        } else {
            assert_eq!(out["error"], fallback);
        }
        assert!(out["session"].is_null());
        assert!(!out.to_string().contains("private-token"));
        assert!(!out.to_string().contains("private.invalid"));
    }
    let rate_limit = run("apiError", json!({"status": 429}));
    assert_eq!(
        rate_limit["message"],
        "Too many requests. Wait a moment and try again."
    );
    assert_eq!(rate_limit["code"], "");
}
