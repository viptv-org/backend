use super::*;
#[tokio::test]
async fn anime_tracking_rejects_ambiguous_absolute_numbers_before_any_provider_write() {
    let (mut app, original, _) = server().await;
    link(&app).await;
    original.abort();
    let writes = Arc::new(AtomicUsize::new(0));
    let counter = writes.clone();
    let routes=Router::new()
        .route("/anime/40",get(||async{axum::Json(json!({"title":"Fixture anime","ids":{"simkl":40}}))}))
        .route("/anime/episodes/40",get(||async{axum::Json(json!([
            {"type":"episode","episode":52,"ids":{"simkl":401},"tvdb":{"season":2,"episode":1}},
            {"type":"episode","episode":52,"ids":{"simkl":402},"tvdb":{"season":2,"episode":2}}
        ]))}))
        .route("/sync/history",post(move||{let counter=counter.clone();async move{counter.fetch_add(1,Ordering::SeqCst);axum::Json(json!({"added":{"episodes":1}}))}}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
    let (status, _) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v3/profiles/1/simkl/watched",
        json!({"category":"anime","id":40,"season":2,"episode":1}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        writes.load(Ordering::SeqCst),
        0,
        "never submit an ambiguous absolute episode to Simkl"
    );
    task.abort();
}
