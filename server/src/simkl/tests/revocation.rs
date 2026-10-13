use super::*;

#[tokio::test]
async fn profile_switch_while_metadata_is_queued_prevents_upstream_request() {
    let (mut app, token_server, _) = server().await;
    let reads = Arc::new(AtomicUsize::new(0));
    let count = reads.clone();
    let mock = axum::Router::new().route(
        "/movies/10",
        get(move || {
            let count = count.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                axum::Json(json!({"title":"Movie","ids":{"simkl":10}}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
    let entry = Arc::new(tokio::sync::Mutex::new(None));
    app.simkl
        .metadata
        .lock()
        .await
        .insert("/movies/10?extended=full".into(), entry.clone());
    let guard = entry.lock().await;
    let request_app = app.clone();
    let pending = tokio::spawn(async move {
        request(
            &request_app,
            "member-token-1",
            "GET",
            "/api/v3/profiles/1/simkl/meta/movie/10",
            json!({}),
        )
        .await
    });
    // The cache clone proves the request acquired its profile lease and is queued.
    tokio::time::timeout(Duration::from_secs(3), async {
        while Arc::strong_count(&entry) < 3 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE auth_sessions SET profile_id=NULL WHERE id='s1'", [])
        .unwrap();
    drop(guard);
    assert_eq!(pending.await.unwrap().0, StatusCode::UNAUTHORIZED);
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    token_server.abort();
    task.abort();
}

#[tokio::test]
async fn profile_switch_during_metadata_prevents_watched_write() {
    let (mut app, token_server, _) = server().await;
    link(&app).await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    let start = entered.clone();
    let finish = resume.clone();
    let writes = Arc::new(AtomicUsize::new(0));
    let count = writes.clone();
    let mock = axum::Router::new()
        .route(
            "/movies/10",
            get(move || {
                let start = start.clone();
                let finish = finish.clone();
                async move {
                    start.notify_one();
                    finish.notified().await;
                    axum::Json(json!({"title":"Movie","ids":{"simkl":10}}))
                }
            }),
        )
        .route(
            "/sync/history",
            post(move || {
                let count = count.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    axum::Json(json!({"added":{"movies":1}}))
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
    let request_app = app.clone();
    let pending = tokio::spawn(async move {
        request(
            &request_app,
            "member-token-1",
            "POST",
            "/api/v3/profiles/1/simkl/watched",
            json!({"category":"movie","id":10}),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE auth_sessions SET profile_id=NULL WHERE id='s1'", [])
        .unwrap();
    resume.notify_one();
    assert_eq!(pending.await.unwrap().0, StatusCode::UNAUTHORIZED);
    assert_eq!(writes.load(Ordering::SeqCst), 0);
    token_server.abort();
    task.abort();
}

#[tokio::test]
async fn another_profile_of_same_account_does_not_inherit_link() {
    let (app, task, _) = server().await;
    link(&app).await;
    {
        let db = app.db.lock().unwrap();
        db.execute("INSERT INTO profiles(id,name,avatar_seed,presentation_complete) VALUES(3,'Other','other',1)",[]).unwrap();
        db.execute("INSERT INTO profile_owners VALUES(3,1,0)", [])
            .unwrap();
        db.execute("UPDATE auth_sessions SET profile_id=3 WHERE id='s1'", [])
            .unwrap();
    }
    let (status, body) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v3/profiles/3/simkl",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["linked"], false);
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/v3/profiles/3/simkl/watched",
            json!({"category":"movie","id":10})
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    task.abort();
}
