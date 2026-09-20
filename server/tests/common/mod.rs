//! Shared fixtures for the integration test binaries.
#![allow(dead_code)]

use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
    Router,
};
use serde_json::Value;
use std::time::Duration;
use tower::ServiceExt;
use viptv_server::{
    playback::{Config, PlaybackManager},
    router, App,
};

pub fn playback(
    root: &std::path::Path,
    ffmpeg: impl Into<std::path::PathBuf>,
    ffprobe: impl Into<std::path::PathBuf>,
    max_sessions: usize,
) -> std::sync::Arc<PlaybackManager> {
    PlaybackManager::new(Config {
        ffmpeg: ffmpeg.into(),
        ffprobe: ffprobe.into(),
        root: root.join("hls"),
        max_sessions,
        ttl: Duration::from_secs(30),
    })
}

pub fn application() -> (Router, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let app = App::new(
        rusqlite::Connection::open_in_memory().unwrap(),
        reqwest::Client::new(),
        playback(
            root.path(),
            "missing-test-ffmpeg",
            "missing-test-ffprobe",
            2,
        ),
    )
    .unwrap();
    (router(app, None), root)
}

pub async fn bearer_request(
    app: &Router,
    token: &str,
    method: &str,
    path: &str,
    value: Value,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(value.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

pub async fn call(
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
