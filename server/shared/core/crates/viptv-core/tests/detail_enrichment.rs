use serde_json::{Value, json};

fn call(kind: &str, input: Value) -> Value {
    serde_json::from_str(
        &viptv_core::normalize(kind.into(), input.to_string(), String::new()).unwrap(),
    )
    .unwrap()
}

#[test]
fn detail_enrichment_retains_occurrence_cursor_and_saved_progress() {
    let result = call(
        "enrichDetail",
        json!({
            "original": {"id":"s","type":"series","season":2,"episode":7,"position":42,"duration":100,"watched":true,"resumeActive":true,"completionOnly":false,"watchDateKnown":false,"sourceAddonId":"addon:4","sourceFingerprint":"fp","poster":"poster","genres":["Drama"],"raw":{"updated_at":2}},
            "metadata": {"id":"s","type":"series","name":"Show","position":0,"season":1,"episode":1,"watched":false,"resumeActive":false,"poster":null,"genres":[],"episodes":[{"id":"e"}],"raw":{"description":"Description"}}
        }),
    );
    assert_eq!(result["season"], 2);
    assert_eq!(result["episode"], 7);
    assert_eq!(result["position"], 42);
    assert_eq!(result["duration"], 100);
    assert_eq!(result["watched"], true);
    assert_eq!(result["resumeActive"], true);
    assert_eq!(result["watchDateKnown"], false);
    assert_eq!(result["completionOnly"], false);
    assert_eq!(result["sourceFingerprint"], "fp");
    assert_eq!(result["poster"], "poster");
    assert_eq!(result["genres"], json!(["Drama"]));
    assert_eq!(result["episodes"].as_array().unwrap().len(), 1);
    assert_eq!(result["raw"]["updated_at"], 2);
    assert_eq!(result["raw"]["description"], "Description");
}

#[test]
fn metadata_can_enrich_identity_and_fill_unknown_detail_facts() {
    let result = call(
        "enrichDetail",
        json!({
            "original":{"id":"e","type":"episode","seriesId":"s","season":2,"episode":7,"position":0,"raw":{}},
            "metadata":{"id":"s","type":"series","name":"Show","position":10,"duration":100,"poster":"poster","episodes":[{"id":"e"}],"raw":{}}
        }),
    );
    assert_eq!(result["id"], "s");
    assert_eq!(result["type"], "series");
    assert_eq!(result["season"], 2);
    assert_eq!(result["episode"], 7);
    assert_eq!(result["position"], 10);
    assert_eq!(result["duration"], 100);
    assert_eq!(result["poster"], "poster");
}
