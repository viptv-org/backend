use super::*;
use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
};
use serde_json::{json, Value};
use tower::ServiceExt;

fn preflight(path: &str) -> Request<Body> {
    Request::builder()
        .method("OPTIONS")
        .uri(path)
        .header(header::ORIGIN, "null")
        .header("access-control-request-method", "POST")
        .header(
            "access-control-request-headers",
            "content-type,authorization",
        )
        .body(Body::empty())
        .unwrap()
}

fn packaged(path: &str, method: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header(header::ORIGIN, "null")
        .header("sec-fetch-site", "cross-site")
        .header(header::CONTENT_TYPE, "application/json")
        .body(if body.is_null() {
            Body::empty()
        } else {
            Body::from(body.to_string())
        })
        .unwrap()
}

async fn json(response: Response) -> Value {
    let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

fn header_list(headers: &axum::http::HeaderMap, name: axum::http::HeaderName) -> Vec<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.split(',').map(|item| item.trim().to_owned()).collect())
        .unwrap_or_default()
}

#[tokio::test]
async fn packaged_origin_completes_pairing_while_browser_origin_stays_pinned() {
    let app = crate::auth_integration_tests::fixture();
    let router = crate::router(app.clone());

    let response = router.clone().oneshot(preflight("/api/auth/device/code")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "null");
    assert!(header_list(response.headers(), header::ACCESS_CONTROL_ALLOW_METHODS).contains(&"POST".into()));
    let allowed = header_list(response.headers(), header::ACCESS_CONTROL_ALLOW_HEADERS);
    assert!(allowed.contains(&"content-type".into()), "{allowed:?}");
    assert!(allowed.contains(&"authorization".into()), "{allowed:?}");
    assert!(!response.headers().contains_key(header::ACCESS_CONTROL_ALLOW_CREDENTIALS));

    let response = router
        .clone()
        .oneshot(packaged("/api/auth/device/code", "POST", json!({"device_name":"Tizen probe"})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "null");
    assert!(response.headers().contains_key(header::ACCESS_CONTROL_EXPOSE_HEADERS));
    let code = json(response).await;
    let user_code = code["user_code"].clone();

    let approved = crate::test_support::request(
        &app,
        "member-token-1",
        "POST",
        "/api/auth/device/approve",
        json!({"user_code":user_code}),
    )
    .await;
    assert_eq!(approved.0, StatusCode::OK, "{}", approved.1);

    let response = router
        .clone()
        .oneshot(packaged(
            "/api/auth/device/token",
            "POST",
            json!({"device_code":code["device_code"]}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let device = json(response).await;
    let access = device["access_token"].as_str().unwrap();
    assert!(!device["refresh_token"].as_str().unwrap().is_empty());

    let me = Request::builder()
        .uri("/api/auth/me")
        .header(header::ORIGIN, "null")
        .header("sec-fetch-site", "cross-site")
        .header(header::AUTHORIZATION, format!("Bearer {access}"))
        .body(Body::empty())
        .unwrap();
    let response = router.clone().oneshot(me).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "null");
    assert!(response
        .headers()
        .get(header::ACCESS_CONTROL_EXPOSE_HEADERS)
        .unwrap()
        .to_str()
        .unwrap()
        .contains("date"));

    let refresh = router
        .clone()
        .oneshot(packaged(
            "/api/auth/device/refresh",
            "POST",
            json!({"refresh_token":device["refresh_token"]}),
        ))
        .await
        .unwrap();
    assert_eq!(refresh.status(), StatusCode::OK);
    let refreshed = json(refresh).await;
    let access = refreshed["access_token"].as_str().unwrap();

    let foreign = Request::builder()
        .uri("/api/auth/me")
        .header(header::ORIGIN, "https://evil.example")
        .header(header::AUTHORIZATION, format!("Bearer {access}"))
        .body(Body::empty())
        .unwrap();
    let response = router.clone().oneshot(foreign).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!response.headers().contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));

    let cookie_profile = Request::builder()
        .method("POST")
        .uri("/api/auth/profile")
        .header(header::ORIGIN, "null")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, "viptv_session=member-token-1")
        .body(Body::from(json!({"profile_id":1}).to_string()))
        .unwrap();
    let response = router.clone().oneshot(cookie_profile).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let login = router
        .clone()
        .oneshot(packaged(
            "/api/auth/login",
            "POST",
            json!({"username":"member1","password":"not-a-real-password"}),
        ))
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::FORBIDDEN);
    assert_eq!(login.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "null");

    let logout = Request::builder()
        .method("POST")
        .uri("/api/auth/logout")
        .header(header::ORIGIN, "null")
        .header("sec-fetch-site", "cross-site")
        .header(header::AUTHORIZATION, format!("Bearer {access}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let logout = router.clone().oneshot(logout).await.unwrap();
    assert_eq!(logout.status(), StatusCode::OK);
    assert_eq!(logout.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "null");
}
