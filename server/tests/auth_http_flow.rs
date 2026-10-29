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

use common::{application, bearer_request};

#[derive(Default)]
struct Browser {
    cookies: BTreeMap<String, String>,
    csrf: String,
}
struct RawPost {
    status: StatusCode,
    body: Value,
    retry_after: Option<String>,
}
/// POST with a browser Origin/Host plus the JSON body, capturing `Retry-After`.
async fn post_with_headers(app: &Router, path: &str, value: Value) -> RawPost {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header(header::HOST, "tv.example")
                .header(header::ORIGIN, "https://tv.example")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(value.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let retry_after = response
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
    RawPost {
        status,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        retry_after,
    }
}
/// Force an already-seeded per-subject bucket to be exhausted so the next
/// request trips the limit. Only touches test fixtures, never production.
fn exhaust_subject_bucket(app: &App, path: &str) {
    let db = app.db.lock().unwrap();
    let pattern = format!("auth:subject:{path}:%");
    db.execute(
        "UPDATE auth_limits SET count=900 WHERE bucket LIKE ?1",
        rusqlite::params![pattern],
    )
    .unwrap();
}
/// In-memory app with an owner account plus access to `App` for seeding limits.
fn owner_with_db() -> (Router, App, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(
        "PRAGMA foreign_keys=ON;CREATE TABLE profiles(id INTEGER PRIMARY KEY,name TEXT NOT NULL);",
    )
    .unwrap();
    auth::init(&db).unwrap();
    auth::create_owner_offline(&mut db, "owner", "Owner", "secure-owner-password").unwrap();
    let app = App::new(db, reqwest::Client::new()).unwrap();
    (router(app.clone()), app, root)
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
    let app = App::new(db, reqwest::Client::new()).unwrap();
    (router(app), root)
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
        json!({"name":"Kid","avatar_id":"vynxc"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(profile["avatar_id"], "vynxc");
    assert_eq!(profile["setup_complete"], true);
    assert!(profile.get("presentation_complete").is_none());
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
        json!({"name":"Kid Viewer","avatar_id":"zayne"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["name"], "Kid Viewer");
    assert_eq!(updated["avatar_id"], "zayne");
    // Renaming with an avatar completes setup on a fresh profile.
    assert_eq!(updated["setup_complete"], true);
    let (status, catalog) = browser_request(
        &app,
        &mut first,
        "POST",
        "/api/profiles",
        json!({"name":"Catalog Kid","avatar_style":"pixel-art","avatar_choice":5}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{catalog}");
    assert_eq!(catalog["avatar_style"], "pixel-art");
    assert_eq!(catalog["avatar_choice"], 5);
    assert_eq!(
        catalog["avatar_url"],
        "https://api.dicebear.com/10.x/pixel-art/png?seed=viptv-pixel-art-5&size=256"
    );
    assert_eq!(catalog["avatar_id"], Value::Null);
    assert_eq!(
        browser_request(
            &app,
            &mut first,
            "PATCH",
            &format!("/api/profiles/{id}"),
            json!({"avatar_style":"princesses","avatar_choice":11})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        browser_request(
            &app,
            &mut first,
            "PUT",
            &format!("/api/profiles/{id}/progress"),
            json!({"record":{"identity":{"id":"tt1","kind":"movie","ids":[],"episode":null,"edition":null},"provider_id":"fixture","title":"Movie","poster":null,"landscape":null,"position_millis":13_554,"duration_millis":8_888_000,"updated_at_millis":0,"completed_at_millis":null},"ended":false})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        browser_request(&app, &mut second, "GET", "/api/profiles", Value::Null)
            .await
            .1["profiles"],
        json!([])
    );
    assert_eq!(
        browser_request(
            &app,
            &mut second,
            "GET",
            &format!("/api/profiles/{id}/progress/page"),
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
            json!({"name":"Bad","avatar_id":"untrusted-url"})
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
            .1["profiles"],
        json!([])
    );
    let (status, profile) = bearer_request(
        &app,
        access,
        "POST",
        "/api/profiles",
        json!({"name":"TV Kid","avatar_id":"zayne"}),
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

/// The beta state app#97 reported: an account whose stored grants are all
/// device sessions with an immortal refresh but a stale access token. The cap
/// must not refuse the browser sign-in that is the only way to revoke them.
#[tokio::test]
async fn an_account_at_the_cap_with_only_stale_device_grants_can_sign_in() {
    let (app, state, _root) = owner_with_db();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    {
        let db = state.db.lock().unwrap();
        db.execute("DELETE FROM auth_sessions", []).unwrap();
        // MAX_SESSIONS_PER_ACCOUNT stale device rows: an immortal refresh
        // (`DEVICE_EXPIRY`, year 9999) with an access token that has expired.
        for index in 0..32 {
            db.execute("INSERT INTO auth_sessions(id,account_id,access_hash,refresh_hash,csrf_hash,profile_id,kind,device_name,access_expires,refresh_expires,created_at) VALUES(?1,1,?2,?3,?4,NULL,'device','VIPTV television',?5,253402300799,?6)",rusqlite::params![format!("stale-{index:02}"),format!("access-{index}"),format!("refresh-{index}"),format!("csrf-{index}"),now-60,index])
                .unwrap();
        }
    }
    let mut browser = Browser::default();
    let (status, body) = browser_request(
        &app,
        &mut browser,
        "POST",
        "/api/auth/login",
        json!({"username":"owner","password":"secure-owner-password"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(browser.cookies.contains_key("viptv_session"));
    // The native desktop path app#97 also had refused.
    let desktop = post_with_headers(
        &app,
        "/api/auth/device/login",
        json!({"username":"owner","password":"secure-owner-password","device_name":"Desktop"}),
    )
    .await;
    assert_eq!(desktop.status, StatusCode::OK, "{}", desktop.body);
    assert!(desktop.body["access_token"].as_str().is_some_and(|token| !token.is_empty()));
    let db = state.db.lock().unwrap();
    let stored: i64 = db
        .query_row("SELECT count(*) FROM auth_sessions WHERE account_id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(stored, 32, "stored {stored} sessions");
    // Only the oldest stale grant made room; the newest one is still there.
    assert!(db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM auth_sessions WHERE id='stale-31')",
            [],
            |row| row.get::<_, bool>(0)
        )
        .unwrap());
    assert!(!db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM auth_sessions WHERE id='stale-00')",
            [],
            |row| row.get::<_, bool>(0)
        )
        .unwrap());
}

/// The remedy `AUTH.md` promises and the issue found unreachable: a paired TV
/// revokes its own grant without a browser, under either device-facing route
/// spelling, and can never revoke a sibling's.
#[tokio::test]
async fn a_paired_device_revokes_its_own_grant_and_never_a_sibling() {
    let (app, _root) = owner_application();
    let mut browser = Browser::default();
    assert_eq!(
        browser_request(
            &app,
            &mut browser,
            "POST",
            "/api/auth/login",
            json!({"username":"owner","password":"secure-owner-password"})
        )
        .await
        .0,
        StatusCode::OK
    );
    let first = pair_device(&app, &mut browser, "Owner Roku").await;
    let second = pair_device(&app, &mut browser, "Bedroom Roku").await;
    let first_access = first["access_token"].as_str().unwrap();
    let first_id = first["session_id"].as_str().unwrap();
    let second_access = second["access_token"].as_str().unwrap();
    let second_id = second["session_id"].as_str().unwrap();
    assert_eq!(
        bearer_request(
            &app,
            second_access,
            "POST",
            "/api/device/revoke-own",
            json!({"session_id":first_id})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        bearer_request(&app, first_access, "GET", "/api/profiles", Value::Null)
            .await
            .0,
        StatusCode::OK
    );
    let (status, body) = bearer_request(
        &app,
        second_access,
        "POST",
        "/api/auth/device/revoke-own",
        json!({"session_id":second_id}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
    assert_eq!(
        bearer_request(&app, second_access, "GET", "/api/profiles", Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    // The sibling TV keeps its grant, and a device may name no grant at all
    // when the one it means is its own credential family.
    assert_eq!(
        bearer_request(&app, first_access, "GET", "/api/profiles", Value::Null)
            .await
            .0,
        StatusCode::OK
    );
    let third = pair_device(&app, &mut browser, "Kitchen Roku").await;
    assert_eq!(
        bearer_request(
            &app,
            third["access_token"].as_str().unwrap(),
            "POST",
            "/api/auth/device/revoke",
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn former_static_bearer_never_establishes_identity() {
    let (app, _) = application();
    for path in ["/api/profiles", "/api/addons", "/api/catalogs"] {
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

#[tokio::test]
async fn native_password_login_issues_revocable_device_tokens_without_browser_cookies() {
    let (app, _root) = owner_application();
    let mut browser = Browser::default();
    let (status, invalid) = browser_request(
        &app,
        &mut browser,
        "POST",
        "/api/auth/device/login",
        json!({"username":"owner","password":"wrong-password","device_name":"Android phone"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(invalid.get("access_token").is_none());
    let (status, grant) = browser_request(&app, &mut browser, "POST", "/api/auth/device/login",
        json!({"username":"owner","password":"secure-owner-password","device_name":"Android phone"})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(browser.cookies.is_empty());
    assert!(grant["access_token"].as_str().is_some());
    assert!(grant["refresh_token"].as_str().is_some());
    assert!(grant["profile_id"].is_null());
    let (status, next) = browser_request(
        &app,
        &mut browser,
        "POST",
        "/api/auth/device/refresh",
        json!({"refresh_token":grant["refresh_token"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(grant["access_token"], next["access_token"]);
    assert!(browser.cookies.is_empty());
}

#[tokio::test]
async fn rate_limited_login_is_distinct_from_wrong_password() {
    let (router, app, _root) = owner_with_db();

    // A wrong password is an ordinary 401 with no retry hint.
    let wrong = post_with_headers(
        &router,
        "/api/auth/login",
        json!({"username":"owner","password":"wrong-password"}),
    )
    .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    assert_eq!(wrong.body["error_code"], "unauthorized");
    assert!(wrong.retry_after.is_none());

    // Seed the per-subject bucket, then exhaust it.
    let seed = post_with_headers(
        &router,
        "/api/auth/login",
        json!({"username":"owner","password":"secure-owner-password"}),
    )
    .await;
    assert_eq!(seed.status, StatusCode::OK);
    exhaust_subject_bucket(&app, "/auth/login");

    // The correct password now tripping the rate limit is a 429 with a retry hint.
    let limited = post_with_headers(
        &router,
        "/api/auth/login",
        json!({"username":"owner","password":"secure-owner-password"}),
    )
    .await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited.body["error_code"], "rate_limited");
    assert!(limited.retry_after.is_some());
}

#[tokio::test]
async fn rate_limited_device_login_is_distinct_from_wrong_password() {
    let (router, app, _root) = owner_with_db();

    let wrong = post_with_headers(
        &router,
        "/api/auth/device/login",
        json!({"username":"owner","password":"wrong-password","device_name":"Phone"}),
    )
    .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    assert_eq!(wrong.body["error_code"], "unauthorized");

    let seed = post_with_headers(
        &router,
        "/api/auth/device/login",
        json!({"username":"owner","password":"secure-owner-password","device_name":"Phone"}),
    )
    .await;
    assert_eq!(seed.status, StatusCode::OK);
    exhaust_subject_bucket(&app, "/auth/device/login");

    let limited = post_with_headers(
        &router,
        "/api/auth/device/login",
        json!({"username":"owner","password":"secure-owner-password","device_name":"Phone"}),
    )
    .await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited.body["error_code"], "rate_limited");
    assert!(limited.retry_after.is_some());
}

#[tokio::test]
async fn rate_limited_device_poll_endpoints_return_429() {
    let (router, app, _root) = owner_with_db();

    let seed = post_with_headers(
        &router,
        "/api/device/token",
        json!({"device_code":"some-device-code"}),
    )
    .await;
    assert_ne!(seed.status, StatusCode::TOO_MANY_REQUESTS);
    exhaust_subject_bucket(&app, "/auth/device/token");
    let limited = post_with_headers(
        &router,
        "/api/device/token",
        json!({"device_code":"some-device-code"}),
    )
    .await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited.body["error_code"], "rate_limited");
    assert!(limited.retry_after.is_some());

    let seed = post_with_headers(
        &router,
        "/api/device/refresh",
        json!({"refresh_token":"some-refresh-token"}),
    )
    .await;
    assert_ne!(seed.status, StatusCode::TOO_MANY_REQUESTS);
    exhaust_subject_bucket(&app, "/auth/device/refresh");
    let limited = post_with_headers(
        &router,
        "/api/device/refresh",
        json!({"refresh_token":"some-refresh-token"}),
    )
    .await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited.body["error_code"], "rate_limited");
    assert!(limited.retry_after.is_some());
}
