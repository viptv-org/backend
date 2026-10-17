use super::tracking_fixture::*;
use super::*;

#[tokio::test]
async fn lifecycle_stop_retains_95_percent_policy_and_rejects_replayed_attempts() {
    let (app, task, writes) = tracking_server().await;
    let mut event = json!({"attempt_id":"first","sequence":1,"identity":media_identity("simkl:tv:20:1:2","series",Some(json!({"order":"aired","season":1,"episode":2}))),"parent_identity":media_identity("simkl:tv:20","series",None),"event":"start","position_millis":0,"duration_millis":100_000,"ended":false});
    let endpoint = "/api/profiles/1/playback-events";
    assert_eq!(
        request(&app, "member-token-1", "POST", endpoint, event.clone())
            .await
            .0,
        StatusCode::OK
    );
    event["sequence"] = json!(2);
    event["event"] = json!("stop");
    event["position_millis"] = json!(94_999);
    let (_, response) = request(&app, "member-token-1", "POST", endpoint, event.clone()).await;
    assert_eq!(response["action"], "pause");
    assert_eq!(response["completed"], false);
    event["sequence"] = json!(3);
    event["position_millis"] = json!(95_000);
    let (_, response) = request(&app, "member-token-1", "POST", endpoint, event.clone()).await;
    assert_eq!(response["action"], "stop");
    assert_eq!(response["completed"], true);
    assert_eq!(
        request(&app, "member-token-1", "POST", endpoint, event.clone())
            .await
            .0,
        StatusCode::CONFLICT
    );
    event["attempt_id"] = json!("second");
    event["sequence"] = json!(1);
    event["event"] = json!("start");
    event["position_millis"] = json!(0);
    assert_eq!(
        request(&app, "member-token-1", "POST", endpoint, event.clone())
            .await
            .0,
        StatusCode::OK
    );
    event["attempt_id"] = json!("first");
    assert_eq!(
        request(&app, "member-token-1", "POST", endpoint, event)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let writes = writes.lock().unwrap();
    assert_eq!(writes.len(), 4);
    assert_eq!(writes[1]["path"], "pause");
    assert_eq!(writes[2]["path"], "stop");
    assert_eq!(writes[2]["body"]["episode"], json!({"season":1,"number":2}));
    task.abort();
}

#[tokio::test]
async fn queued_lifecycle_configuration_revision_change_prevents_scrobble_egress() {
    let (app, task, writes) = tracking_server().await;
    let (_, configuration) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/integrations",
        json!({}),
    )
    .await;
    let blocked = app.simkl.tracking.workers.acquire_many(4).await.unwrap();
    let scoped = app.clone();
    let pending = tokio::spawn(async move {
        request(&scoped,"member-token-1","POST","/api/profiles/1/playback-events",json!({"attempt_id":"queued","sequence":1,"identity":media_identity("simkl:movie:10","movie",None),"event":"start","position_millis":0,"duration_millis":100_000})).await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        while app.simkl.tracking.pending_count() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let update = json!({"revision":0,"browsing_plugin":"simkl","assignments":configuration["configuration"]["assignments"]});
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/profiles/1/integrations",
            update
        )
        .await
        .0,
        StatusCode::OK
    );
    drop(blocked);
    assert_eq!(pending.await.unwrap().0, StatusCode::CONFLICT);
    assert!(writes.lock().unwrap().is_empty());
    task.abort();
}

#[tokio::test]
async fn lifecycle_attempt_cannot_change_media_and_unmapped_provider_does_not_call_upstream() {
    let (app, task, writes) = tracking_server().await;
    let endpoint = "/api/profiles/1/playback-events";
    let mut event = json!({"attempt_id":"immutable","sequence":1,"identity":media_identity("simkl:movie:10","movie",None),"event":"start","position_millis":0,"duration_millis":100_000});
    assert_eq!(
        request(&app, "member-token-1", "POST", endpoint, event.clone())
            .await
            .0,
        StatusCode::OK
    );
    event["sequence"] = json!(2);
    event["identity"] = media_identity("simkl:movie:11", "movie", None);
    event["event"] = json!("pause");
    event["position_millis"] = json!(10_000);
    assert_eq!(
        request(&app, "member-token-1", "POST", endpoint, event.clone())
            .await
            .0,
        StatusCode::CONFLICT
    );
    event["identity"] = media_identity("tt1234567", "movie", None);
    let (status, response) = request(&app, "member-token-1", "POST", endpoint, event).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response["state"], "unavailable");
    assert_eq!(response["reason"], "metadata_mapping_unavailable");
    assert_eq!(writes.lock().unwrap().len(), 1);
    task.abort();
}

#[tokio::test]
async fn lifecycle_busy_reservation_does_not_consume_an_unsubmitted_sequence() {
    let (app, task, writes) = tracking_server().await;
    let held =
        automatic::reserve(app.simkl.clone(), "lifecycle:1:simkl:movie:10:start".into()).unwrap();
    let event = json!({"attempt_id":"retry","sequence":1,"identity":media_identity("simkl:movie:10","movie",None),"event":"start","position_millis":0,"duration_millis":100_000});
    let endpoint = "/api/profiles/1/playback-events";
    assert_eq!(
        request(&app, "member-token-1", "POST", endpoint, event.clone())
            .await
            .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert!(writes.lock().unwrap().is_empty());
    drop(held);
    assert_eq!(
        request(&app, "member-token-1", "POST", endpoint, event.clone())
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, "member-token-1", "POST", endpoint, event)
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(app.simkl.tracking.pending_count(), 0);
    assert_eq!(writes.lock().unwrap().len(), 1);
    task.abort();
}

#[tokio::test]
async fn provider_authorization_failure_is_not_a_viptv_session_expiry() {
    let (mut app, old_task, _) = tracking_server().await;
    let mock = axum::Router::new()
        .route(
            "/movies/10",
            get(|| async { axum::Json(json!({"title":"Movie","ids":{"simkl":10}})) }),
        )
        .route(
            "/scrobble/start",
            post(|| async { StatusCode::UNAUTHORIZED }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
    let (status,result)=request(&app,"member-token-1","POST","/api/profiles/1/playback-events",json!({"attempt_id":"expired-provider","sequence":1,"identity":media_identity("simkl:movie:10","movie",None),"event":"start","position_millis":0,"duration_millis":100_000})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["state"], "unavailable");
    assert_eq!(result["reason"], "tracking_authorization_required");
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/profiles/1/simkl",
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(app.simkl.tracking.pending_count(), 0);
    old_task.abort();
    task.abort();
}

#[tokio::test]
async fn anime_scrobble_uses_verified_sequential_mapping_and_rejects_missing_mapping() {
    let (app, task, writes) = tracking_server().await;
    let mut event = json!({"attempt_id":"anime","sequence":1,"identity":media_identity("simkl:anime:40:2:1","series",Some(json!({"order":"aired","season":2,"episode":1}))),"parent_identity":media_identity("simkl:anime:40","series",None),"event":"start","position_millis":0,"duration_millis":100_000,"ended":false});
    let endpoint = "/api/profiles/1/playback-events";
    let (status, result) = request(&app, "member-token-1", "POST", endpoint, event.clone()).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["state"], "recorded");
    event["sequence"] = json!(2);
    event["event"] = json!("stop");
    event["position_millis"] = json!(94_000);
    let (status, result) = request(&app, "member-token-1", "POST", endpoint, event.clone()).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["action"], "pause");
    assert_eq!(writes.lock().unwrap().len(), 2);
    let body = writes.lock().unwrap()[0]["body"].clone();
    assert_eq!(body["anime"]["ids"]["simkl"], 40);
    assert!(body["show"].is_null());
    assert_eq!(body["episode"], json!({"season":1,"number":36}));
    event["attempt_id"] = json!("missing");
    event["sequence"] = json!(1);
    event["event"] = json!("start");
    event["position_millis"] = json!(0);
    event["identity"] = media_identity(
        "simkl:anime:40:2:2",
        "series",
        Some(json!({"order":"aired","season":2,"episode":2})),
    );
    assert_ne!(
        request(&app, "member-token-1", "POST", endpoint, event)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(writes.lock().unwrap().len(), 2);
    let film = json!({"attempt_id":"anime-film","sequence":1,"identity":media_identity("simkl:anime:50","movie",None),"event":"start","position_millis":0,"duration_millis":100_000});
    let (status, result) = request(&app, "member-token-1", "POST", endpoint, film).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let body = writes.lock().unwrap()[2]["body"].clone();
    assert_eq!(body["anime"]["ids"]["simkl"], 50);
    assert!(body["episode"].is_null());
    task.abort();
}
