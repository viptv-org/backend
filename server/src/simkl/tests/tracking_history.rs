use super::tracking_fixture::*;
use super::*;

#[tokio::test]
async fn typed_progress_uses_server_clock_95_percent_and_coalesces_completion() {
    let (app, task, writes) = tracking_server().await;
    let endpoint = "/api/profiles/1/progress";
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            endpoint,
            history_body("simkl:movie:10", 94_999, 100_000)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(writes.lock().unwrap().is_empty());
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            endpoint,
            history_body("simkl:movie:10", 95_000, 100_000)
        )
        .await
        .0,
        StatusCode::OK
    );
    receipt(&app, "simkl:movie:10", "done").await;
    for _ in 0..3 {
        assert_eq!(
            request(
                &app,
                "member-token-1",
                "PUT",
                endpoint,
                history_body("simkl:movie:10", 96_000, 100_000)
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    assert_eq!(writes.lock().unwrap().len(), 1);
    let date = writes.lock().unwrap()[0]["body"]["movies"][0]["watched_at"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        chrono::DateTime::parse_from_rfc3339(&date)
            .unwrap()
            .timestamp()
            > 999
    );
    let updated: i64 = app
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT updated_at FROM progress WHERE id='simkl:movie:10'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(updated > 999);
    task.abort();
}

#[tokio::test]
async fn queued_completion_is_revoked_before_any_history_egress() {
    let (app, task, writes) = tracking_server().await;
    let blocked = app.simkl.tracking.workers.acquire_many(4).await.unwrap();
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/profiles/1/progress",
            history_body("simkl:movie:10", 95_000, 100_000)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "DELETE",
            "/api/profiles/1/simkl",
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    drop(blocked);
    receipt(&app, "simkl:movie:10", "failed").await;
    assert!(writes.lock().unwrap().is_empty());
    task.abort();
}

#[tokio::test]
async fn opaque_stremio_history_and_unlinked_lifecycle_remain_usable() {
    let (app, task, _) = server().await;
    let mut body = history_body("opaque:episode", 0, 0);
    body["record"]["identity"] = media_identity(
        "opaque:episode",
        "series",
        Some(json!({"order":"aired","season":3,"episode":4})),
    );
    body["record"]["provider_id"] = json!("stremio");
    body["parent_identity"] = media_identity("opaque:parent", "series", None);
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/profiles/1/progress",
            body.clone()
        )
        .await
        .0,
        StatusCode::OK
    );
    let saved: String = app
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT title_id FROM progress WHERE id='opaque:episode'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(saved, "opaque:parent");
    let event = json!({"attempt_id":"unlinked","sequence":1,"identity":body["record"]["identity"],"parent_identity":body["parent_identity"],"event":"start","position_millis":0,"duration_millis":0});
    let (status, response) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/profiles/1/playback-events",
        event,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response["state"], "ignored");
    assert_eq!(response["reason"], "tracking_unlinked");
    task.abort();
}

#[tokio::test]
async fn typed_simkl_episode_parent_coordinates_are_authoritative_before_history_write() {
    let (app, task, writes) = tracking_server().await;
    let mut body = history_body("simkl:tv:20:1:2", 95_000, 100_000);
    body["record"]["identity"] = media_identity(
        "simkl:tv:20:1:2",
        "series",
        Some(json!({"order":"aired","season":1,"episode":2})),
    );
    body["parent_identity"] = media_identity("simkl:tv:20", "series", None);
    body["parent_identity"]["id"] = json!("simkl:tv:21");
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/profiles/1/progress",
            body.clone()
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    body["parent_identity"]["id"] = json!("simkl:tv:20");
    body["record"]["identity"]["episode"]["episode"] = json!(3);
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/profiles/1/progress",
            body.clone()
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    body["record"]["identity"]["episode"]["episode"] = json!(2);
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/profiles/1/progress",
            body
        )
        .await
        .0,
        StatusCode::OK
    );
    receipt(&app, "simkl:tv:20:1:2", "done").await;
    assert_eq!(
        writes.lock().unwrap()[0]["body"]["shows"][0]["seasons"][0]["episodes"][0]["number"],
        2
    );
    task.abort();
}

#[tokio::test]
async fn ended_unknown_runtime_retains_observed_position_without_completion_or_tracking() {
    let (app, task, writes) = tracking_server().await;
    let mut body = history_body("simkl:movie:10", 42_125, 0);
    body["ended"] = json!(true);
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/profiles/1/progress",
            body
        )
        .await
        .0,
        StatusCode::OK
    );
    let item: Value = app
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT position,duration FROM progress WHERE id='simkl:movie:10'",
            [],
            |row| Ok(json!({"position":row.get::<_,f64>(0)?,"duration":row.get::<_,f64>(1)?})),
        )
        .unwrap();
    assert_eq!(item["position"], 42.125);
    assert_eq!(item["duration"], 0.0);
    assert!(!library::watched(&item, &json!({})));
    assert_eq!(app.simkl.tracking.pending_count(), 0);
    assert!(writes.lock().unwrap().is_empty());
    task.abort();
}

#[tokio::test]
async fn anime_progress_preserves_tvdb_identity_and_history_uses_verified_native_number() {
    let (app, task, writes) = tracking_server().await;
    let mut body = history_body("simkl:anime:40:2:1", 94_000, 100_000);
    body["record"]["identity"] = media_identity(
        "simkl:anime:40:2:1",
        "series",
        Some(json!({"order":"aired","season":2,"episode":1})),
    );
    body["parent_identity"] = media_identity("simkl:anime:40", "series", None);
    let endpoint = "/api/profiles/1/progress";
    let (status, result) = request(&app, "member-token-1", "PUT", endpoint, body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert!(writes.lock().unwrap().is_empty());
    body["record"]["position_millis"] = json!(95_000);
    let (status, result) = request(&app, "member-token-1", "PUT", endpoint, body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    receipt(&app, "simkl:anime:40:2:1", "done").await;
    let history = writes.lock().unwrap()[0]["body"].clone();
    assert!(history["shows"].is_null());
    assert_eq!(history["anime"][0]["ids"]["simkl"], 40);
    assert_eq!(history["anime"][0]["seasons"][0]["number"], 1);
    assert_eq!(
        history["anime"][0]["seasons"][0]["episodes"][0]["number"],
        36
    );
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT id FROM progress WHERE profile_id=1 AND id='simkl:anime:40:2:1'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "simkl:anime:40:2:1"
    );
    body["parent_identity"] = media_identity("simkl:tv:40", "series", None);
    assert_eq!(
        request(&app, "member-token-1", "PUT", endpoint, body)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(writes.lock().unwrap().len(), 1);
    task.abort();
}

#[tokio::test]
async fn verified_anime_film_progress_uses_anime_title_without_episode() {
    let (app, task, writes) = tracking_server().await;
    let body = history_body("simkl:anime:50", 95_000, 100_000);
    let (status, result) = request(
        &app,
        "member-token-1",
        "PUT",
        "/api/profiles/1/progress",
        body,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    receipt(&app, "simkl:anime:50", "done").await;
    let body = writes.lock().unwrap()[0]["body"].clone();
    assert_eq!(body["anime"][0]["ids"]["simkl"], 50);
    assert!(body["anime"][0]["seasons"].is_null());
    assert!(body["movies"].is_null());
    task.abort();
}
