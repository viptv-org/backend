use serde_json::{Value, json};
use viptv_core::normalize;
fn run(kind: &str, input: Value) -> Value {
    serde_json::from_str(
        &normalize(
            kind.into(),
            input.to_string(),
            "https://backend.example".into(),
        )
        .unwrap(),
    )
    .unwrap()
}
#[test]
fn v2_discovery_requests_are_explicit_and_keep_episode_identity() {
    let item = json!({"id":"tt1234567:1:2","type":"series","seriesId":"tt1234567","season":1,"episode":2,"name":"Episode"});
    let request = run("request", json!({"operation":"sourcesV2","item":item}));
    assert_eq!(request["path"], "/api/v2/streams");
    assert_eq!(request["body"]["id"], "tt1234567:1:2");
    assert_eq!(request["body"]["episode"], 2);
    assert!(request["body"].get("profile_id").is_none());
    assert_eq!(
        run(
            "request",
            json!({"operation":"sourcesPollV2","id":"job/one","after":3})
        )["path"],
        "/api/v2/streams/job%2Fone?after=3"
    );
    assert_eq!(
        run(
            "request",
            json!({"operation":"sources","item":{"type":"movie","id":"tt1"}})
        )["path"],
        "/api/streams"
    );
}
#[test]
fn source_failures_survive_polling_without_losing_healthy_sources_or_secrets() {
    let first = run(
        "sourcesPollStep",
        json!({"state":{"after":0,"sources":[],"polls":0},"poll":{"done":false,"events":[{"seq":1,"source":"iptv:1","streams":[],"error_code":"provider_connection_limit","error":"https://provider.invalid/private-token"},{"seq":2,"source":"addon:2","streams":[{"id":"stream","name":"Good","url":"https://provider.invalid/private-token"}]}]}}),
    );
    assert_eq!(first["sources"].as_array().unwrap().len(), 1);
    assert_eq!(
        first["state"]["errors"][0]["code"],
        "provider_connection_limit"
    );
    assert!(
        first["state"]["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("Stop another stream")
    );
    assert!(!first.to_string().contains("private-token"));
    let last = run(
        "sourcesPollStep",
        json!({"state":first["state"],"poll":{"done":true,"events":[{"seq":3,"source":"iptv:1","streams":[],"error_code":"provider_connection_limit"}]}}),
    );
    assert_eq!(last["state"]["after"], 3);
    assert_eq!(last["state"]["errors"].as_array().unwrap().len(), 1);
    assert_eq!(last["sources"].as_array().unwrap().len(), 1);
    let errors=(0..100).map(|n|json!({"seq":n+1,"source":format!("addon:{n}"),"streams":[],"error_code":"addon_timeout"})).collect::<Vec<_>>();
    assert_eq!(
        run(
            "sourcesPollStep",
            json!({"poll":{"done":true,"events":errors}})
        )["state"]["errors"]
            .as_array()
            .unwrap()
            .len(),
        16
    );
}
