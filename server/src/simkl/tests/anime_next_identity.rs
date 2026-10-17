use super::*;
#[tokio::test]
async fn imported_anime_next_rejects_aired_mapping_shared_by_different_absolute_episodes() {
    let (mut app, original, _) = server().await;
    link(&app).await;
    original.abort();
    let routes=Router::new()
        .route("/sync/activities",get(||async{axum::Json(json!({"all":"2020-01-02T00:00:00Z"}))}))
        .route("/sync/all-items",get(||async{axum::Json(json!({"anime":[{"status":"watching","next_to_watch_info":{"episode":52,"date":"2020-01-01T00:00:00Z"},"show":{"title":"Fixture anime","runtime":24,"genres":["Drama"],"ids":{"simkl":40}},"seasons":[{"number":1,"episodes":[{"number":51,"runtime":24,"watched_at":"2020-01-01T00:00:00Z"}]}]}]}))}))
        .route("/sync/playback",get(||async{axum::Json(json!([]))}))
        .route("/anime/40",get(||async{axum::Json(json!({"title":"Fixture anime","runtime":24,"ids":{"simkl":40}}))}))
        .route("/anime/episodes/40",get(||async{axum::Json(json!([
            {"type":"episode","episode":51,"aired":true,"ids":{"simkl":401},"tvdb":{"season":2,"episode":1}},
            {"type":"episode","episode":52,"aired":true,"ids":{"simkl":402},"tvdb":{"season":2,"episode":1}}
        ]))}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/profiles/1/library",
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    let state = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let (_, state) = request(
                &app,
                "member-token-1",
                "GET",
                "/api/profiles/1/imports/simkl",
                Value::Null,
            )
            .await;
            if state["running"] == false {
                break state;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(state["availability"], "Available");
    assert_eq!(state["unresolved"], 1);
    let db = app.db.lock().unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM continuation_cache", [], |r| r
            .get::<_, u64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT id FROM progress WHERE profile_id=1", [], |r| r
            .get::<_, String>(
            0
        ))
        .unwrap(),
        "simkl:anime:40:anidb:51"
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM simkl_tracking_receipts", [], |r| r
            .get::<_, u64>(0))
            .unwrap(),
        0
    );
    task.abort();
}
