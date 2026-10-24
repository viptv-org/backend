use crate::{auth_integration_tests::fixture, test_support::request, *};
use axum::{
    body::{to_bytes, Body},
    http::Request,
};
use tower::ServiceExt;

#[tokio::test]
async fn subtitle_discovery_hides_urls_content_is_session_scoped_and_configuration_revokes_it() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let subtitle_url = format!("http://{address}/private-subtitle-secret.srt");
    let mock = Router::new()
        .route(
            "/a/subtitles/movie/tt1.json",
            get(move || {
                let url = subtitle_url.clone();
                async move { axum::Json(json!({"subtitles":[{"lang":"eng","url":url}]})) }
            }),
        )
        .route(
            "/b/subtitles/movie/tt1.json",
            get(|| async { StatusCode::BAD_GATEWAY }),
        )
        .route(
            "/private-subtitle-secret.srt",
            get(|| async {
                (
                    [(
                        header::CONTENT_TYPE,
                        "application/x-subrip; charset=Windows-1252",
                    )],
                    b"1\r\n00:00:01,000 --> 00:00:03,000\r\nCaf\xe9\r\n".to_vec(),
                )
            }),
        );
    let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let mut app = fixture();
    app.addons.allow_test_loopback = true;
    let manifest = json!({"id":"subs","name":"Subtitles","version":"1","resources":["subtitles"],"types":["movie"]});
    for path in ["a", "b"] {
        app.db.lock().unwrap().execute(
            "INSERT INTO addons(name,manifest_url,manifest,account_id) VALUES('Subtitles',?1,?2,1)",
            params![format!("http://{address}/{path}/manifest.json"), manifest.to_string()],
        ).unwrap();
    }
    crate::test_support::encrypt_fixture_sources(&app);
    let (status, found) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/subtitles/movie/tt1",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{found}");
    assert_eq!(found["candidates"].as_array().unwrap().len(), 1);
    assert_eq!(found["providers"][0]["state"], "Ready");
    assert_eq!(found["providers"][1]["state"], "Failed");
    assert!(!found.to_string().contains("private-subtitle-secret"));
    let content = format!(
        "/api/subtitle-content/{}",
        found["candidates"][0]["id"].as_str().unwrap()
    );
    let (status, _) = request(&app, "member-token-2", "GET", &content, json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let response = router(app.clone())
        .oneshot(
            Request::builder()
                .uri(&content)
                .header(header::AUTHORIZATION, "Bearer member-token-1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/vtt; charset=utf-8"
    );
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    assert_eq!(
        std::str::from_utf8(&body).unwrap(),
        "WEBVTT\n\n00:00:01.000 --> 00:00:03.000\nCafé\n\n"
    );
    let (status, _) = request(
        &app,
        "member-token-1",
        "PUT",
        "/api/profiles/1/integrations",
        json!({"revision":0,"browsing_plugin":"stremio","assignments":[]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = request(&app, "member-token-1", "GET", &content, json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, disabled) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/subtitles/movie/tt1",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(disabled["candidates"].as_array().unwrap().is_empty());
    server.abort();
}
