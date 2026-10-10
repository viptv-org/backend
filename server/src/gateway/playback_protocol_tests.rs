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
    protocol_route_request(app, token, body, "/api/v2/playback-protocol").await
}

async fn protocol_route_request(
    app: &crate::App,
    token: &str,
    body: Body,
    path: &str,
) -> (StatusCode, Value, String) {
    let response = router(app.clone(), None)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(path)
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
async fn shared_runtime_protocol_is_separate_scoped_and_bodyless() {
    let app = fixture();
    let route = "/api/v2/torrent-runtime-protocol";
    let (status, value, cache) =
        protocol_route_request(&app, "member-token-1", Body::empty(), route).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value, json!({"version":2,"native_torrent_versions":[2]}));
    assert_eq!(cache, "no-store");
    let (status, _, _) = protocol_route_request(&app, "invalid-token", Body::empty(), route).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, value, _) = protocol_route_request(
        &app,
        "member-token-1",
        Body::from("runtime-private-sentinel"),
        route,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(!value.to_string().contains("runtime-private-sentinel"));
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE auth_sessions SET profile_id=NULL WHERE id='s1'", [])
        .unwrap();
    let (status, _, _) = protocol_route_request(&app, "member-token-1", Body::empty(), route).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn native_protocol_is_authenticated_bodyless_and_enabled_by_default() {
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
