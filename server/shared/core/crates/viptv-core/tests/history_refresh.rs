use serde_json::{Value, json};

#[test]
fn repeated_history_refresh_clears_unknown_activity_and_source_identity() {
    let episode = json!({"id":"e","season":2,"episode":7,"duration":100,
        "raw":{"updated_at":"2026-01-01T00:00:00Z"}});
    let known = json!({"id":"e","seriesId":"s","position":42,"duration":null,"watched":true,
        "resumeActive":true,"completionOnly":false,"watchDateKnown":true,
        "sourceAddonId":"addon","sourceFingerprint":"prior","sourceName":"Prior",
        "updatedAtMillis":2000});
    let merge = |episodes: Value, history: Value| -> Value {
        serde_json::from_str(
            &viptv_core::normalize(
                "mergeEpisodeProgress".into(),
                json!({"seriesId":"s","episodes":episodes,"history":history}).to_string(),
                "".into(),
            )
            .unwrap(),
        )
        .unwrap()
    };
    let first = merge(json!([episode]), json!([known]));
    assert_eq!(first[0]["sourceFingerprint"], "prior");
    assert_eq!(first[0]["updatedAtMillis"], 2000);
    let unknown = json!({"id":"e","seriesId":"s","position":0,"duration":null,"watched":false,
        "resumeActive":null,"sourceFingerprint":null,"raw":{}});
    let refreshed = merge(first, json!([unknown]));
    for key in [
        "sourceAddonId",
        "sourceFingerprint",
        "sourceName",
        "updatedAtMillis",
        "resumeActive",
        "completionOnly",
        "watchDateKnown",
    ] {
        assert!(refreshed[0][key].is_null(), "Retained obsolete {key}");
    }
    assert!(refreshed[0]["raw"]["updated_at"].is_null());
    assert_eq!(refreshed[0]["duration"], 100);
    assert_eq!(refreshed[0]["position"], 0);
    assert_eq!(refreshed[0]["watched"], false);
}
