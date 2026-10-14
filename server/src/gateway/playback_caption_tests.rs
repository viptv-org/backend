use super::*;
async fn subtitles(app: &App, media: &str) -> String {
    let (status, value) = request(
        app,
        "member-token-1",
        "GET",
        &format!("/api/v3/subtitles/movie/{media}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value["candidates"][0]["id"].as_str().unwrap().into()
}
#[tokio::test]
async fn external_caption_attach_requires_exact_media_scope_and_an_advertised_gateway() {
    let (mut app, peer) = setup().await;
    gateway(&app, "first", 1);
    app.addons.allow_test_loopback = true;
    let source = source(&app);
    app.streams
        .lock()
        .unwrap()
        .get_mut(&source)
        .unwrap()
        .media_id = Some("tt1".into());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let reads = Arc::new(AtomicUsize::new(0));
    let fetched = reads.clone();
    let routes=Router::new().route("/subtitles/movie/:media",axum::routing::get(move||async move{Json(json!({"subtitles":[{"lang":"eng","url":format!("http://{address}/private-caption.srt")}]}))}))
        .route("/private-caption.srt",axum::routing::get(move|headers:axum::http::HeaderMap|{let fetched=fetched.clone();async move {
            assert!(!headers.contains_key("authorization"));assert!(!headers.contains_key("cookie"));fetched.fetch_add(1,Ordering::SeqCst);
            ([("content-type","application/x-subrip; charset=windows-1252")],b"1\n00:00:01,000 --> 00:00:03,000\nCaf\xe9\n".to_vec())
        }}));
    let addon = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    app.db.lock().unwrap().execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id) VALUES(2,'Caption addon',?1,?2,1)",crate::params![format!("http://{address}/manifest.json"),json!({"id":"caption","name":"Caption addon","version":"1","resources":["subtitles"],"types":["movie"]}).to_string()]).unwrap();
    crate::test_support::encrypt_fixture_sources(&app);
    peer.mode.store(10, Ordering::SeqCst);
    let (_, started) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/playback",
        body(&source, "caption-owned", "roku"),
    )
    .await;
    let id = started["id"].as_str().unwrap();
    let ready = settled(&app, id).await;
    assert_eq!(ready["delivery"]["external_subtitles_supported"], true);
    assert_eq!(ready["delivery"]["subtitles_supported"], false);
    let endpoint = format!("/api/v2/playback/{id}/subtitles");
    let wrong = subtitles(&app, "tt2").await;
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            &endpoint,
            json!({"subtitle_id":wrong})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    let selected = subtitles(&app, "tt1").await;
    assert_eq!(
        request(
            &app,
            "member-token-2",
            "POST",
            &endpoint,
            json!({"subtitle_id":selected})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let (status, caption) = request(
        &app,
        "member-token-1",
        "POST",
        &endpoint,
        json!({"subtitle_id":selected}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{caption}");
    assert_eq!(caption["format"], "srt");
    assert_eq!(caption["language"], "eng");
    assert!(caption["url"]
        .as_str()
        .unwrap()
        .starts_with("https://first.gateway.invalid/base/media/"));
    assert!(!caption.to_string().contains("private-caption"));
    assert!(!caption.to_string().contains("member-token"));
    let uploads = peer.captions.lock().unwrap();
    assert_eq!(uploads.len(), 1);
    assert!(uploads[0]["content_vtt"].as_str().unwrap().contains("Café"));
    drop(uploads);
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            &endpoint,
            json!({"subtitle_id":selected,"url":"https://private.invalid"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    peer.mode.store(0, Ordering::SeqCst);
    let (_, old) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/playback",
        body(&source, "caption-old-gateway", "roku"),
    )
    .await;
    let old = old["id"].as_str().unwrap();
    assert_eq!(
        settled(&app, old).await["delivery"]["external_subtitles_supported"],
        false
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            &format!("/api/v2/playback/{old}/subtitles"),
            json!({"subtitle_id":selected})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    request(
        &app,
        "member-token-1",
        "DELETE",
        &format!("/api/v2/playback/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            &endpoint,
            json!({"subtitle_id":selected})
        )
        .await
        .0,
        StatusCode::GONE
    );
    assert_eq!(peer.captions.lock().unwrap().len(), 1);
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM progress", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        0
    );
    addon.abort();
}
