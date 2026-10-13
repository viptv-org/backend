use super::tracking_fixture::*;
use super::*;

fn event(sequence: u64, action: &str, position: u64) -> Value {
    json!({"attempt_id":"completion","sequence":sequence,"identity":media_identity("simkl:movie:10","movie",None),"event":action,"position_millis":position,"duration_millis":100_000})
}

#[tokio::test]
async fn history_first_deduplicates_lifecycle_completion_without_a_second_mutation() {
    let (app, task, writes) = tracking_server().await;
    let events = "/api/v3/profiles/1/playback-events";
    assert_eq!(
        request(&app, "member-token-1", "POST", events, event(1, "start", 0))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/v3/profiles/1/progress",
            history_body("simkl:movie:10", 95_000, 100_000)
        )
        .await
        .0,
        StatusCode::OK
    );
    receipt(&app, "simkl:movie:10", "done").await;
    let (status, result) = request(
        &app,
        "member-token-1",
        "POST",
        events,
        event(2, "stop", 95_000),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["state"], "recorded");
    assert_eq!(result["completed"], true);
    assert_eq!(result["deduplicated"], true);
    assert!(result["action"].is_null());
    assert_eq!(
        writes
            .lock()
            .unwrap()
            .iter()
            .map(|v| v["path"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["start", "history"]
    );
    task.abort();
}

#[tokio::test]
async fn stop_first_confirms_the_shared_receipt_and_suppresses_history_upload() {
    let (app, task, writes) = tracking_server().await;
    let events = "/api/v3/profiles/1/playback-events";
    assert_eq!(
        request(&app, "member-token-1", "POST", events, event(1, "start", 0))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            events,
            event(2, "stop", 95_000)
        )
        .await
        .0,
        StatusCode::OK
    );
    receipt(&app, "simkl:movie:10", "done").await;
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/v3/profiles/1/progress",
            history_body("simkl:movie:10", 95_000, 100_000)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(app.simkl.tracking.pending_count(), 0);
    assert_eq!(
        writes
            .lock()
            .unwrap()
            .iter()
            .map(|v| v["path"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["start", "stop"]
    );
    task.abort();
}

#[tokio::test]
async fn failed_stop_keeps_history_fallback_available() {
    let (app, task, writes) = tracking_server_with_failed_stop(true).await;
    let events = "/api/v3/profiles/1/playback-events";
    assert_eq!(
        request(&app, "member-token-1", "POST", events, event(1, "start", 0))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            events,
            event(2, "stop", 95_000)
        )
        .await
        .0,
        StatusCode::BAD_GATEWAY
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/v3/profiles/1/progress",
            history_body("simkl:movie:10", 95_000, 100_000)
        )
        .await
        .0,
        StatusCode::OK
    );
    receipt(&app, "simkl:movie:10", "done").await;
    assert_eq!(
        writes
            .lock()
            .unwrap()
            .iter()
            .map(|v| v["path"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["start", "stop", "history"]
    );
    task.abort();
}

#[tokio::test]
async fn explicit_watched_correction_is_its_own_user_action_after_automatic_completion() {
    let (app, task, writes) = tracking_server().await;
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/v3/profiles/1/progress",
            history_body("simkl:movie:10", 95_000, 100_000)
        )
        .await
        .0,
        StatusCode::OK
    );
    receipt(&app, "simkl:movie:10", "done").await;
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/profiles/1/progress/correct",
            json!({"id":"simkl:movie:10","type":"movie","name":"Movie","action":"watched"})
        )
        .await
        .0,
        StatusCode::OK
    );
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            if writes.lock().unwrap().len() == 2 {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(writes
        .lock()
        .unwrap()
        .iter()
        .all(|v| v["path"] == "history"));
    task.abort();
}
