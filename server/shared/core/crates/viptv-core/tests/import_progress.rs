use serde_json::{Value, json};

fn call(kind: &str, input: Value) -> Value {
    serde_json::from_str(
        &viptv_core::normalize(
            kind.into(),
            input.to_string(),
            "https://example.test".into(),
        )
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn imported_facts_normalize_recursively_without_becoming_activity() {
    let item = call(
        "media",
        json!({"id":"series","type":"series","name":"Series","videos":[{"id":"e1","season":1,"episode":1,"watched":true,"resume_active":false,"watch_date_known":false,"completion_only":true}],"previous_episode":{"id":"e0","type":"episode","watched":true,"watch_date_known":true},"watched":true,"resume_active":true,"watch_date_known":true,"completion_only":false}),
    );
    assert_eq!(item["resumeActive"], true);
    assert_eq!(item["watchDateKnown"], true);
    assert_eq!(item["completionOnly"], false);
    assert_eq!(item["episodes"][0]["completionOnly"], true);
    assert_eq!(item["episodes"][0]["watchDateKnown"], false);
    assert_eq!(item["previousEpisode"]["watchDateKnown"], true);
    let old = call("media", json!({"id":"old","type":"movie"}));
    assert!(old.get("resumeActive").is_none());
    assert!(old.get("completionOnly").is_none());
    assert!(old.get("watchDateKnown").is_none());
}

#[test]
fn active_rewatch_beats_watched_next_and_completion_only_activity() {
    let episodes = json!([
        {"id":"e1","season":1,"episode":1,"watched":true,"completionOnly":true,"updatedAtMillis":999999},
        {"id":"e2","season":1,"episode":2,"watched":true,"resumeActive":true,"position":42,"duration":100,"updatedAtMillis":2000},
        {"id":"e3","season":1,"episode":3,"watched":false,"releasedAtMillis":10}
    ]);
    let merged = call(
        "mergeEpisodeProgress",
        json!({"seriesId":"s","episodes":episodes,"history":[{"id":"e2","resumeActive":true,"watchDateKnown":true,"completionOnly":false,"watched":true,"position":42,"updatedAtMillis":2000}]}),
    );
    assert_eq!(merged[1]["resumeActive"], true);
    assert_eq!(merged[1]["watchDateKnown"], true);
    assert_eq!(merged[1]["completionOnly"], false);
    assert_eq!(
        call("initialEpisode", json!({"episodes":merged,"now":10000}))["id"],
        "e2"
    );
    let newer_completion =
        json!({"id":"e3","season":1,"episode":3,"watched":true,"updatedAtMillis":3000});
    let with_newer_completion =
        json!({"episodes":[merged[0],merged[1],newer_completion],"now":10000});
    assert_eq!(call("initialEpisode", with_newer_completion)["id"], "e2");
    let queue = call(
        "enrichHome",
        json!({"original":{"id":"e2","type":"episode","name":"Series","season":1,"episode":2,"queueStatus":"next","watched":true,"resumeActive":true,"position":42,"duration":100,"previousEpisode":{"id":"e1","completionOnly":true}},"metadata":{"id":"s","name":"Series","watched":false,"resumeActive":false,"position":0}}),
    );
    assert_eq!(queue["watched"], true);
    assert_eq!(queue["resumeActive"], true);
    assert_eq!(queue["previousEpisode"]["completionOnly"], true);
    assert_eq!(
        call("presentation", queue.clone())["primaryAction"],
        "resume"
    );
    let card = call("cardPresentation", json!({"item":queue,"context":"queue"}));
    assert_eq!(card["primaryAction"], "resume");
    assert!(
        card["subtitle"]
            .as_str()
            .unwrap()
            .contains("Resume at 0:42")
    );
}

#[test]
fn completion_only_never_selects_activity_and_old_completed_navigation_remains() {
    let episodes = json!([
        {"id":"e1","season":1,"episode":1,"watched":true,"completionOnly":true,"updatedAtMillis":999999},
        {"id":"e2","season":1,"episode":2,"watched":true,"updatedAtMillis":1000},
        {"id":"e3","season":1,"episode":3,"releasedAtMillis":10}
    ]);
    assert_eq!(
        call("initialEpisode", json!({"episodes":episodes,"now":10000}))["id"],
        "e3"
    );
    assert_eq!(
        call(
            "initialEpisode",
            json!({"episodes":[{"id":"e1","season":1,"episode":1,"watched":true,"completionOnly":true,"updatedAtMillis":999999},{"id":"e2","season":1,"episode":2,"releasedAtMillis":10}],"now":10000})
        )["id"],
        "e2"
    );
    assert_eq!(
        call(
            "presentation",
            json!({"type":"episode","queueStatus":"next","watched":true,"position":0})
        )["primaryAction"],
        "next"
    );
}
