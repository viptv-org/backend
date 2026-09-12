//! Browser SPA entry routes must return a successful, non-cacheable HTML bootstrap.
use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
    Router,
};
use std::time::Duration;
use tower::ServiceExt;
use viptv_server::{
    playback::{Config, PlaybackManager},
    router, App,
};

fn application() -> (Router, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("index.html"),
        "<!doctype html><html><body>VIPTV device activation</body></html>",
    )
    .unwrap();
    let playback = PlaybackManager::new(Config {
        ffmpeg: "missing-test-ffmpeg".into(),
        ffprobe: "missing-test-ffprobe".into(),
        root: root.path().join("hls"),
        max_sessions: 1,
        ttl: Duration::from_secs(30),
    });
    let app = App::new(
        rusqlite::Connection::open_in_memory().unwrap(),
        reqwest::Client::new(),
        playback,
    )
    .unwrap();
    (router(app, Some(root.path().to_path_buf())), root)
}

async fn get(app: &Router, uri: &str) -> axum::response::Response {
    app.clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn conditional_get(app: &Router, uri: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header(header::IF_MODIFIED_SINCE, "Wed, 31 Dec 2099 23:59:59 GMT")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn explicit_spa_entries_are_html_200_no_store_without_changing_fallbacks() {
    let (app, _root) = application();

    for uri in ["/", "/device?code=AB12CD34"] {
        let response = get(&app, uri).await;
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        assert!(response.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/html"));
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        assert!(body
            .windows(b"VIPTV device activation".len())
            .any(|window| window == b"VIPTV device activation"));
    }

    for uri in ["/", "/device?code=AB12CD34"] {
        let response = conditional_get(&app, uri).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{uri} must not return 304"
        );
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        assert!(body
            .windows(b"VIPTV device activation".len())
            .any(|window| window == b"VIPTV device activation"));
    }

    let unknown_page = get(&app, "/missing-browser-route").await;
    assert_eq!(unknown_page.status(), StatusCode::NOT_FOUND);

    let missing_api = get(&app, "/api/definitely-missing").await;
    assert_eq!(missing_api.status(), StatusCode::NOT_FOUND);
}
