//! Browser SPA entry routes must return a successful, non-cacheable HTML bootstrap.
mod common;

use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
    Router,
};
use tower::ServiceExt;
use viptv_server::{router, router_with_tv, App};

use common::playback;

// Both fixtures keep one session and unavailable media tools; only the mount differs.
fn application() -> (Router, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("index.html"),
        "<!doctype html><html><body>VIPTV device activation</body></html>",
    )
    .unwrap();
    let app = App::new(
        rusqlite::Connection::open_in_memory().unwrap(),
        reqwest::Client::new(),
        playback(
            root.path(),
            "missing-test-ffmpeg",
            "missing-test-ffprobe",
            1,
        ),
    )
    .unwrap();
    (router(app, Some(root.path().to_path_buf())), root)
}

fn tv_application() -> (Router, tempfile::TempDir, tempfile::TempDir) {
    let dashboard = tempfile::tempdir().unwrap();
    let tv = tempfile::tempdir().unwrap();
    std::fs::write(
        dashboard.path().join("index.html"),
        "<html>dashboard</html>",
    )
    .unwrap();
    std::fs::write(tv.path().join("index.html"), "<html>tv shell</html>").unwrap();
    std::fs::write(tv.path().join("asset.js"), "window.viptv=true").unwrap();
    let app = App::new(
        rusqlite::Connection::open_in_memory().unwrap(),
        reqwest::Client::new(),
        playback(dashboard.path(), "missing", "missing", 1),
    )
    .unwrap();
    (
        router_with_tv(
            app,
            Some(dashboard.path().to_path_buf()),
            Some(tv.path().to_path_buf()),
        ),
        dashboard,
        tv,
    )
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

#[tokio::test]
async fn tv_bundle_has_its_own_same_origin_mount_without_replacing_dashboard_or_api() {
    let (app, _dashboard, _tv) = tv_application();
    for uri in ["/tv", "/tv/", "/tv/detail/tt123"] {
        let response = get(&app, uri).await;
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        assert!(body
            .windows(b"tv shell".len())
            .any(|window| window == b"tv shell"));
    }
    let asset = get(&app, "/tv/asset.js").await;
    assert_eq!(asset.status(), StatusCode::OK);
    let body = to_bytes(asset.into_body(), 64 * 1024).await.unwrap();
    assert!(body
        .windows(b"window.viptv=true".len())
        .any(|window| window == b"window.viptv=true"));
    let dashboard = get(&app, "/").await;
    let body = to_bytes(dashboard.into_body(), 64 * 1024).await.unwrap();
    assert!(body
        .windows(b"dashboard".len())
        .any(|window| window == b"dashboard"));
    assert_eq!(
        get(&app, "/api/definitely-missing").await.status(),
        StatusCode::NOT_FOUND
    );
}
