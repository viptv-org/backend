//! Public account-only gateway contract through the real router.
use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
    Router,
};
use serde_json::{json, Value};
use std::time::Duration;
use tower::ServiceExt;
use viptv_server::{
    playback::{Config, PlaybackManager},
    router, App,
};

fn application() -> (Router, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let playback = PlaybackManager::new(Config {
        ffmpeg: "missing-test-ffmpeg".into(),
        ffprobe: "missing-test-ffprobe".into(),
        root: root.path().join("hls"),
        max_sessions: 2,
        ttl: Duration::from_secs(30),
    });
    let app = App::new(
        rusqlite::Connection::open_in_memory().unwrap(),
        reqwest::Client::new(),
        playback,
    )
    .unwrap();
    (router(app, None), root)
}
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    origin: bool,
    value: Value,
) -> (StatusCode, Value, Vec<String>) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::HOST, "tv.example");
    if origin {
        request = request.header(header::ORIGIN, "https://tv.example");
    }
    if let Some(token) = token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(value.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let cookies = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|value| value.to_str().unwrap().to_owned())
        .collect();
    let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        cookies,
    )
}
#[tokio::test]
async fn status_exposes_registration_not_bootstrap_or_legacy_mode() {
    let (app, _) = application();
    let (status, body, _) = call(&app, "GET", "/api/auth/status", None, false, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({"registration_enabled":true,"authenticated":false,"csrf_token":null})
    );
    for key in [
        "claimed",
        "claim_available",
        "claim_required",
        "accounts_enabled",
        "mode",
        "legacy",
    ] {
        assert!(body.get(key).is_none());
    }
}
#[tokio::test]
async fn public_registration_requires_same_origin_and_starts_with_zero_profiles() {
    let (app, _) = application();
    let payload = json!({"username":"new.viewer","name":"New Viewer","password":"long-random-viewer-password"});
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/auth/register",
            None,
            false,
            payload.clone()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, body, cookies) =
        call(&app, "POST", "/api/auth/register", None, true, payload).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["profile_id"].is_null());
    assert_eq!(body["profiles"], json!([]));
    assert!(body["recovery_codes"]
        .as_array()
        .is_some_and(|codes| codes.len() == 1));
    assert_eq!(cookies.len(), 2);
    assert!(cookies.iter().all(|cookie| cookie.contains("HttpOnly")
        && cookie.contains("Secure")
        && cookie.contains("SameSite=Strict")));
}
#[tokio::test]
async fn old_shared_bearer_and_claim_surface_are_rejected() {
    let (app, _) = application();
    for (method, path) in [
        ("GET", "/api/profiles"),
        ("GET", "/api/providers"),
        ("POST", "/api/auth/claim"),
        ("POST", "/api/accounts"),
    ] {
        let status = call(
            &app,
            method,
            path,
            Some("former-static-api-token-0123456789"),
            true,
            json!({}),
        )
        .await
        .0;
        assert!(
            matches!(status, StatusCode::UNAUTHORIZED | StatusCode::NOT_FOUND),
            "{method} {path}: {status}"
        );
    }
}
