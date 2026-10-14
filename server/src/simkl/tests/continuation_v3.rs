use super::*;
fn next_body() -> Value {
    json!({"identity":{"id":"simkl:tv:20:1:2","kind":"series","ids":[],"episode":{"order":"aired","season":1,"episode":2},"edition":null},"parent_identity":{"id":"simkl:tv:20","kind":"series","ids":[],"episode":null,"edition":null}})
}
#[tokio::test]
async fn typed_next_uses_selected_simkl_metadata_and_honest_airing_without_history_or_tracking() {
    let (mut app, initial, _) = server().await;
    initial.abort();
    let phase = Arc::new(AtomicUsize::new(0));
    let state = phase.clone();
    let routes=Router::new().route("/tv/20",get(||async{axum::Json(json!({"title":"Actual series","ids":{"simkl":20,"imdb":"tt1234567"}}))})).route("/tv/episodes/20",get(move||{let phase=state.clone();async move{let n=phase.load(Ordering::SeqCst);let mut episodes=vec![json!({"title":"Current","season":1,"episode":2,"aired":true,"ids":{"simkl":30}})];if n!=2{episodes.push(json!({"title":"Next","season":1,"episode":3,"aired":if n==3{Value::Null}else{json!(n==0)},"date":if n==1{json!("2100-01-01")}else{Value::Null},"ids":{"simkl":31}}));}axum::Json(json!(episodes))}}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
    for (n, expected) in [
        (0, "Next"),
        (1, "Upcoming"),
        (2, "CaughtUp"),
        (3, "Unavailable"),
    ] {
        phase.store(n, Ordering::SeqCst);
        app.simkl.metadata.lock().await.clear();
        let (status, result) = request(
            &app,
            "member-token-1",
            "POST",
            "/api/v3/profiles/1/continue/next",
            next_body(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{result}");
        assert_eq!(result["status"], expected);
        if n == 0 {
            assert_eq!(result["item"]["identity"]["id"], "simkl:tv:20:1:3");
            assert_eq!(result["item"]["parent_identity"]["id"], "simkl:tv:20");
        } else {
            assert!(result["item"].is_null());
        }
    }
    let db = app.db.lock().unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM progress", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM simkl_tracking_receipts", [], |r| r
            .get::<_, u64>(0))
            .unwrap(),
        0
    );
    drop(db);
    task.abort();
}
#[tokio::test]
async fn profile_switch_during_next_metadata_read_never_publishes_a_transition() {
    let (mut app, initial, _) = server().await;
    initial.abort();
    let entered = Arc::new(tokio::sync::Notify::new());
    let finish = Arc::new(tokio::sync::Notify::new());
    let start = entered.clone();
    let done = finish.clone();
    let routes = Router::new().route(
        "/tv/20",
        get(move || {
            let start = start.clone();
            let done = done.clone();
            async move {
                start.notify_one();
                done.notified().await;
                axum::Json(json!({"title":"Retired series","ids":{"simkl":20}}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
    let pending = app.clone();
    let worker = tokio::spawn(async move {
        request(
            &pending,
            "member-token-1",
            "POST",
            "/api/v3/profiles/1/continue/next",
            next_body(),
        )
        .await
    });
    entered.notified().await;
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE auth_sessions SET profile_id=NULL WHERE id='s1'", [])
        .unwrap();
    finish.notify_one();
    let (status, _) = worker.await.unwrap();
    assert!(matches!(
        status,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::CONFLICT
    ));
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM continuation_cache", [], |r| r
                .get::<_, u64>(0))
            .unwrap(),
        0
    );
    task.abort();
}
