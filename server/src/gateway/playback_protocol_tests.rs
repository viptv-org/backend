use crate::{auth_integration_tests::fixture, router};
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use serde_json::{json, Value};
use tower::ServiceExt;

async fn protocol_request(
    app: &crate::App,
    token: &str,
    body: Body,
) -> (StatusCode, Value, String) {
    let response = router(app.clone(), None)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v2/playback-protocol")
                .header("authorization", format!("Bearer {token}"))
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let cache = response
        .headers()
        .get("cache-control")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let bytes = to_bytes(response.into_body(), 4096).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap(), cache)
}

#[tokio::test]
async fn native_protocol_is_authenticated_bodyless_and_support_is_separate_from_policy() {
    let app = fixture();
    let (status, value, cache) = protocol_request(&app, "member-token-1", Body::empty()).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value, json!({"version":1,"native_torrent_versions":[1]}));
    assert_eq!(cache, "no-store");
    let (status, value, _) = protocol_request(&app, "invalid-token", Body::empty()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{value}");
}

#[tokio::test]
async fn native_protocol_requires_an_admitted_profile_and_revalidates_resource_authority() {
    let app = fixture();
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE auth_sessions SET profile_id=NULL WHERE id='s1'", [])
        .unwrap();
    let (status, value, _) = protocol_request(&app, "member-token-1", Body::empty()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{value}");
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE auth_sessions SET profile_id=1 WHERE id='s1'", [])
        .unwrap();
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE auth_accounts SET disabled=1 WHERE id=1", [])
        .unwrap();
    let (status, value, _) = protocol_request(&app, "member-token-1", Body::empty()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{value}");
}

#[tokio::test]
async fn native_protocol_rejects_a_body_without_echoing_it() {
    let app = fixture();
    let (status, value, cache) = protocol_request(
        &app,
        "member-token-1",
        Body::from("private-protocol-sentinel"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
    assert_eq!(cache, "no-store");
    assert!(!value.to_string().contains("private-protocol-sentinel"));
}
