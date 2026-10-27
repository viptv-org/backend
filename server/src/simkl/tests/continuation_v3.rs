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
            "/api/profiles/1/continue/next",
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
            "/api/profiles/1/continue/next",
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

/// backend#31: with Simkl as the browsing integration (the default once Simkl
/// is configured), a Stremio-imported series Simkl can identify keeps the
/// episode still from Simkl's document on its Continue Watching card.
#[tokio::test]
async fn imported_continue_watching_series_gets_the_simkl_episode_still() {
    let (mut app, initial, _) = server().await;
    initial.abort();
    let routes = Router::new()
        .route("/tv/20", get(|| async { axum::Json(json!({"title":"Actual series","fanart":"fan/20","ids":{"simkl":20,"imdb":"tt1234567"}})) }))
        .route("/tv/episodes/20", get(|| async { axum::Json(json!([
            {"title":"Current","season":1,"episode":2,"aired":true,"img":"ep/30","ids":{"simkl":30}},
            {"title":"Next","season":1,"episode":3,"aired":false,"date":"2100-01-01","img":"ep/31","ids":{"simkl":31}}
        ])) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
    let (status, integrations) = request(&app, "member-token-1", "GET", "/api/profiles/1/integrations", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{integrations}");
    assert_eq!(integrations["configuration"]["browsing_plugin"], "simkl");
    {
        let db = app.db.lock().unwrap();
        db.execute("INSERT INTO progress(profile_id,id,type,name,poster,position,duration,updated_at,context,title_id) VALUES(1,'tt1234567:1:2','series','Actual series',NULL,5,100,1000,?1,'tt1234567')", [json!({"season":1,"episode":2,"series_id":"tt1234567","stremio_import_watched":true}).to_string()]).unwrap();
        // The title is one Simkl knows (a Simkl import mapped it).
        db.execute("INSERT INTO simkl_import_entries(profile_id,remote_id,remote_kind,local_id,local_kind,card,statistics,link_revision) VALUES(1,'simkl:tv:20','series','tt1234567','series','{}','{}',0)", []).unwrap();
    }
    request(&app, "member-token-1", "GET", "/api/profiles/1/home", Value::Null).await;
    let stored = || -> i64 {
        app.db.lock().unwrap().query_row("SELECT COUNT(*) FROM continuation_releases WHERE document IS NOT NULL", [], |r| r.get(0)).unwrap()
    };
    for _ in 0..200 {
        if stored() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(stored(), 1, "the release refresh read Simkl's document");
    // A restart empties Home's in-memory card enrichment.
    app.verified_cards.lock().unwrap().clear();
    let (status, page) = request(&app, "member-token-1", "GET", "/api/profiles/1/continue", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let card = &page["entries"][0]["card"];
    assert_eq!(card["provider_id"], "library", "{page}");
    assert_eq!(card["landscape"], "https://wsrv.nl/?url=https://simkl.in/episodes/ep/30_w.webp", "{page}");
    task.abort();
}
