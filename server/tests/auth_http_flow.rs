//! Account-only browser/device HTTP acceptance through router middleware.
mod common;

use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
    Router,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tower::ServiceExt;
use viptv_server::{auth, router, App};

use common::{application, bearer_request, playback};

#[derive(Default)]
struct Browser {
    cookies: BTreeMap<String, String>,
    csrf: String,
}
impl Browser {
    fn cookie_header(&self) -> String {
        self.cookies
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ")
    }
}
fn owner_application() -> (Router, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(
        "PRAGMA foreign_keys=ON;CREATE TABLE profiles(id INTEGER PRIMARY KEY,name TEXT NOT NULL);",
    )
    .unwrap();
    auth::init(&db).unwrap();
    auth::create_owner_offline(&mut db, "owner", "Owner", "secure-owner-password").unwrap();
    let app = App::new(
        db,
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
async fn browser_request(
    app: &Router,
    browser: &mut Browser,
    method: &str,
    path: &str,
    value: Value,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, "tv.example")
        .header(header::CONTENT_TYPE, "application/json");
    if !browser.cookies.is_empty() {
        request = request.header(header::COOKIE, browser.cookie_header());
    }
    if !matches!(method, "GET" | "HEAD" | "OPTIONS") {
        request = request.header(header::ORIGIN, "https://tv.example");
        if !browser.csrf.is_empty() {
            request = request.header("x-csrf-token", &browser.csrf);
        }
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(value.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    for cookie in response.headers().get_all(header::SET_COOKIE) {
        let pair = cookie.to_str().unwrap().split(';').next().unwrap();
        let (name, value) = pair.split_once('=').unwrap();
        browser.cookies.insert(name.into(), value.into());
    }
    let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if let Some(csrf) = body.get("csrf_token").and_then(Value::as_str) {
        browser.csrf = csrf.into();
    }
    (status, body)
}
async fn register(app: &Router, username: &str) -> Browser {
    let mut browser = Browser::default();
    let (status,body)=browser_request(app,&mut browser,"POST","/api/auth/register",json!({"username":username,"name":username,"password":format!("secure-password-for-{username}")})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["profile_id"].is_null());
    assert_eq!(body["profiles"], json!([]));
    browser
}
async fn pair_device(app: &Router, browser: &mut Browser, name: &str) -> Value {
    let (status, code) = browser_request(
        app,
        &mut Browser::default(),
        "POST",
        "/api/device/code",
        json!({"device_name":name}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        browser_request(
            app,
            browser,
            "POST",
            "/api/device/approve",
            json!({"user_code":code["user_code"]})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (status, device) = browser_request(
        app,
        &mut Browser::default(),
        "POST",
        "/api/device/token",
        json!({"device_code":code["device_code"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    device
}
#[tokio::test]
async fn registration_profile_avatar_and_history_are_account_isolated() {
    let (app, _) = application();
    let mut first = register(&app, "first.viewer").await;
    let mut second = register(&app, "second.viewer").await;
    let (status, profile) = browser_request(
        &app,
        &mut first,
        "POST",
        "/api/profiles",
        json!({"name":"Kid","avatar_style":"critters"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(profile["avatar_style"], "critters");
    assert_eq!(profile["setup_complete"], true);
    assert!(profile.get("presentation_complete").is_none());
    assert!(profile["avatar_url"]
        .as_str()
        .unwrap()
        .starts_with("https://api.dicebear.com/10.x/critters/png?seed="));
    let id = profile["id"].as_str().unwrap();
    assert_eq!(
        browser_request(
            &app,
            &mut first,
            "POST",
            "/api/auth/profile",
            json!({"profile_id":id})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (status, updated) = browser_request(
        &app,
        &mut first,
        "PATCH",
        &format!("/api/profiles/{id}"),
        json!({"name":"Kid Viewer","avatar_style":"moods","setup_complete":true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["name"], "Kid Viewer");
    assert_eq!(updated["setup_complete"], true);
    assert_eq!(
        browser_request(
            &app,
            &mut first,
            "PUT",
            &format!("/api/profiles/{id}/progress"),
            json!({"id":"tt1","type":"movie","name":"Movie","position":13.554,"duration":8888.0})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        browser_request(&app, &mut second, "GET", "/api/profiles", Value::Null)
            .await
            .1,
        json!([])
    );
    assert_eq!(
        browser_request(
            &app,
            &mut second,
            "GET",
            &format!("/api/profiles/{id}/progress"),
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        browser_request(
            &app,
            &mut second,
            "POST",
            "/api/profiles",
            json!({"name":"Bad","avatar_style":"untrusted-url"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        browser_request(
            &app,
            &mut second,
            "POST",
            "/api/profiles",
            json!({"name":"Bad","avatar_url":"https://evil.example/avatar.png"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}
#[tokio::test]
async fn device_code_deep_link_binds_confirming_account_and_device_manages_profile() {
    let (app, _) = application();
    let mut browser = register(&app, "device.viewer").await;
    let (status, code) = browser_request(
        &app,
        &mut Browser::default(),
        "POST",
        "/api/device/code",
        json!({"device_name":"Living Room Roku"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(code["verification_uri_complete"]
        .as_str()
        .unwrap()
        .ends_with(&format!(
            "/device?code={}",
            code["user_code"].as_str().unwrap()
        )));
    assert!(code["qr_uri"]
        .as_str()
        .unwrap()
        .contains("/api/auth/device/qr?code="));
    let qr_path = format!(
        "/api/auth/device/qr?code={}",
        code["user_code"].as_str().unwrap()
    );
    let qr = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(&qr_path)
                .header(header::HOST, "tv.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(qr.status(), StatusCode::OK);
    assert_eq!(qr.headers()[header::CONTENT_TYPE], "image/png");
    assert_eq!(qr.headers()[header::CACHE_CONTROL], "no-store");
    assert!(to_bytes(qr.into_body(), 1_000_000)
        .await
        .unwrap()
        .starts_with(b"\x89PNG\r\n\x1a\n"));
    let user_code = code["user_code"].clone();
    assert_eq!(
        browser_request(
            &app,
            &mut browser,
            "POST",
            "/api/device/lookup",
            json!({"user_code":user_code})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        browser_request(
            &app,
            &mut browser,
            "POST",
            "/api/device/approve",
            json!({"user_code":user_code})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (status, device) = browser_request(
        &app,
        &mut Browser::default(),
        "POST",
        "/api/device/token",
        json!({"device_code":code["device_code"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(device["profile_id"].is_null());
    let expired_qr = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(&qr_path)
                .header(header::HOST, "tv.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(expired_qr.status(), StatusCode::NOT_FOUND);
    let access = device["access_token"].as_str().unwrap();
    assert_eq!(
        bearer_request(&app, access, "GET", "/api/profiles", Value::Null)
            .await
            .1,
        json!([])
    );
    let (status, profile) = bearer_request(
        &app,
        access,
        "POST",
        "/api/profiles",
        json!({"name":"TV Kid","avatar_style":"pixel-art"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        bearer_request(
            &app,
            access,
            "POST",
            "/api/auth/profile",
            json!({"profile_id":profile["id"]})
        )
        .await
        .0,
        StatusCode::OK
    );
}
#[tokio::test]
async fn canonical_devices_are_account_owned_and_revoke_only_the_callers_device() {
    let (app, _) = owner_application();
    let mut first = Browser::default();
    assert_eq!(
        browser_request(
            &app,
            &mut first,
            "POST",
            "/api/auth/login",
            json!({"username":"owner","password":"secure-owner-password"})
        )
        .await
        .0,
        StatusCode::OK
    );
    let mut second = register(&app, "device.owner.two").await;
    let first_device = pair_device(&app, &mut first, "Owner Roku").await;
    let second_device = pair_device(&app, &mut second, "Member Roku").await;

    let (status, first_devices) =
        browser_request(&app, &mut first, "GET", "/api/devices", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    let first_devices = first_devices.as_array().unwrap();
    assert_eq!(first_devices.len(), 1);
    assert_eq!(first_devices[0]["device_name"], "Owner Roku");
    assert!(first_devices[0].get("profile_ids").is_none());

    let second_id = second_device["session_id"].as_str().unwrap();
    assert_eq!(
        browser_request(
            &app,
            &mut first,
            "DELETE",
            &format!("/api/devices/{second_id}"),
            json!({})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        bearer_request(
            &app,
            second_device["access_token"].as_str().unwrap(),
            "GET",
            "/api/profiles",
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );

    let first_id = first_device["session_id"].as_str().unwrap();
    assert_eq!(
        browser_request(
            &app,
            &mut first,
            "DELETE",
            &format!("/api/devices/{first_id}"),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        bearer_request(
            &app,
            first_device["access_token"].as_str().unwrap(),
            "GET",
            "/api/profiles",
            Value::Null
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn former_static_bearer_never_establishes_identity() {
    let (app, _) = application();
    for path in [
        "/api/profiles",
        "/api/providers",
        "/api/addons",
        "/api/status",
    ] {
        assert_eq!(
            bearer_request(
                &app,
                "former-static-key-0123456789",
                "GET",
                path,
                Value::Null
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
    }
}
