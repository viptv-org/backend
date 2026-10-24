//! The KMP viewing app owns every UI route; there is no legacy dashboard or
//! TV-web mount. `/device` falls through to the KMP app shell, which renders
//! its own client-side approval route.
mod common;

use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
    Router,
};
use tower::ServiceExt;
use viptv_server::{router, router_with_viewers, App};

fn application() -> (Router, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("index.html"),
        "<!doctype html><html><body>KMP app shell</body></html>",
    )
    .unwrap();
    // The release-shaped service pins an HTTPS callback origin for Simkl.
    std::env::set_var("VIPTV_AUTH_ORIGIN", "https://tv.example");
    let app = App::new(
        rusqlite::Connection::open_in_memory().unwrap(),
        reqwest::Client::new(),
    )
    .unwrap();
    (router_with_viewers(app, Some(root.path().to_path_buf())), root)
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
async fn kmp_app_owns_root_and_device_and_deep_links() {
    let (app, _root) = application();

    for uri in ["/", "/device", "/device?code=AB12CD34", "/tv", "/tv/detail/tt123"] {
        let response = get(&app, uri).await;
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        assert!(
            body.windows(b"KMP app shell".len())
                .any(|window| window == b"KMP app shell"),
            "{uri} must serve the KMP app shell"
        );
    }

    // The KMP app shell is not cached; its client router renders /device.
    for uri in ["/device", "/device?code=AB12CD34"] {
        let response = conditional_get(&app, uri).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{uri} must not return 304"
        );
    }

    let missing_api = get(&app, "/api/definitely-missing").await;
    assert_eq!(missing_api.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn server_only_image_answers_root_with_identity() {
    std::env::set_var("VIPTV_AUTH_ORIGIN", "https://tv.example");
    let app = App::new(
        rusqlite::Connection::open_in_memory().unwrap(),
        reqwest::Client::new(),
    )
    .unwrap();
    let app = router(app);
    let response = get(&app, "/").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    assert!(body.windows(b"VIPTV server".len()).any(|window| window == b"VIPTV server"));
}

#[tokio::test]
async fn android_app_links_file_is_json_only_when_configured_and_never_the_app_shell() {
    const FINGERPRINT: &str = "14:6D:E9:83:C5:73:06:50:D8:EE:B9:95:2F:34:FC:64:16:A0:83:42:E6:1D:BE:A8:8A:04:96:B2:3F:CF:44:E5";
    std::env::remove_var("VIPTV_ANDROID_PACKAGE");
    std::env::remove_var("VIPTV_ANDROID_CERT_SHA256");
    let (unconfigured, _root) = application();
    // Without configuration Android must see a failure, not the SPA shell the fallback would serve.
    let response = get(&unconfigured, "/.well-known/assetlinks.json").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    std::env::set_var("VIPTV_ANDROID_PACKAGE", "org.viptv.experiment");
    std::env::set_var("VIPTV_ANDROID_CERT_SHA256", FINGERPRINT);
    let (configured, _root) = application();
    std::env::remove_var("VIPTV_ANDROID_PACKAGE");
    std::env::remove_var("VIPTV_ANDROID_CERT_SHA256");
    let response = get(&configured, "/.well-known/assetlinks.json").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
    let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value[0]["target"]["package_name"], "org.viptv.experiment");
    assert_eq!(value[0]["target"]["sha256_cert_fingerprints"][0], FINGERPRINT);
}
