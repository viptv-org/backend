#[path = "common/torrent_runtime_vectors.rs"]
mod vectors;
use serde_json::Value;

#[test]
fn private_runtime_authority_vectors() {
    let root = vectors::corpus();
    for case in root["cases"].as_array().unwrap() {
        let output = vectors::run(&root, case);
        assert_eq!(
            &output["constructs"],
            case.get("constructs").unwrap_or(&Value::Bool(true)),
            "{}",
            case["name"]
        );
        for (observed, expected) in output["steps"]
            .as_array()
            .unwrap()
            .iter()
            .zip(case["expected"].as_array().unwrap())
        {
            assert_eq!(
                observed["ok"], expected["ok"],
                "{}: {observed}",
                case["name"]
            );
            if expected.get("status").is_some() {
                assert_eq!(
                    observed["result"]["status"], expected["status"],
                    "{}",
                    case["name"]
                );
                assert_eq!(
                    observed["result"]["deadlineMillis"], expected["deadline"],
                    "{}",
                    case["name"]
                );
                let text = observed["result"].to_string();
                assert!(!text.contains("magnet:") && !text.contains("tracker.example"));
            }
            if expected.get("result").is_some() {
                assert_eq!(observed["result"], expected["result"], "{}", case["name"]);
            }
        }
    }
}

#[test]
fn native_retry_renegotiates_without_gateway_fallback() {
    let facts = serde_json::json!({"operation":"recovery","facts":{"admitted":true,"authorityRetired":true,"authorizationRefused":false,"selectionRefused":false,"action":"retry"}});
    assert_eq!(
        viptv_core::normalize("torrentRuntime".into(), facts.to_string(), String::new()).unwrap(),
        "\"nativeRetry\""
    );
}

#[test]
fn runtime_negotiation_requires_advertised_v2_support() {
    let facts = serde_json::json!({"operation":"negotiation","platform":"android","qualified":true,"scopeMatches":true,"status":200,"authorizationRefused":false,"body":"{\"version\":2,\"native_torrent_versions\":[]}"});
    assert_eq!(
        viptv_core::normalize("torrentRuntime".into(), facts.to_string(), String::new()).unwrap(),
        "\"legacy\""
    );
}
