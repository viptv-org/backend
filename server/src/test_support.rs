//! Shared in-process fixtures for the `cfg(test)` router test modules.
use crate::{router, App};
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use rusqlite::Connection;
use serde_json::Value;
use tower::ServiceExt;

/// `App` over `db` whose media tools can never run, so playback endpoints
/// fail fast instead of spawning processes or touching the filesystem.
pub(crate) fn app_with_db(db: Connection) -> App {
    App::new(db, reqwest::Client::new()).unwrap()
}

/// In-memory `App` with unavailable media tools.
pub(crate) fn app() -> App {
    app_with_db(Connection::open_in_memory().unwrap())
}

/// Bearer-authenticated JSON request against a fresh router for `app`.
pub(crate) async fn request(
    app: &App,
    token: &str,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = router(app.clone(), None)
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
