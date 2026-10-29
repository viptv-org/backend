//! A viewing host that is not the canonical origin creates and claims its own code.

use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
    Router,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tower::ServiceExt;
use viptv_server::{router, App};

const CANONICAL_HOST: &str = "viptv.example:8443";
const CANONICAL: &str = "https://viptv.example:8443";
const VIEWING_HOST: &str = "watch.example:8443";
const VIEWING: &str = "https://watch.example:8443";

struct Client {
    host: &'static str,
    origin: Option<&'static str>,
    bearer: Option<String>,
    csrf: Option<String>,
    cookies: BTreeMap<String, String>,
}

impl Client {
    fn new(host: &'static str, origin: Option<&'static str>) -> Self {
        Self {
            host,
            origin,
            bearer: None,
            csrf: None,
            cookies: BTreeMap::new(),
        }
    }

    async fn post(&mut self, app: &Router, path: &str, body: Value) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method("POST")
            .uri(path)
            .header(header::HOST, self.host)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(origin) = self.origin {
            request = request.header(header::ORIGIN, origin);
        }
        if let Some(bearer) = &self.bearer {
            request = request.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
        }
        if let Some(csrf) = &self.csrf {
            request = request.header("x-csrf-token", csrf);
        }
        if !self.cookies.is_empty() {
            request = request.header(
                header::COOKIE,
                self.cookies
                    .iter()
                    .map(|(name, value)| format!("{name}={value}"))
                    .collect::<Vec<_>>()
                    .join("; "),
            );
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        for cookie in response.headers().get_all(header::SET_COOKIE) {
            let pair = cookie.to_str().unwrap().split(';').next().unwrap();
            let (name, value) = pair.split_once('=').unwrap();
            self.cookies.insert(name.into(), value.into());
        }
        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if let Some(csrf) = body.get("csrf_token").and_then(Value::as_str) {
            self.csrf = Some(csrf.into());
        }
        (status, body)
    }
}

fn application() -> Router {
    std::env::set_var("VIPTV_AUTH_ORIGIN", CANONICAL);
    let db = rusqlite::Connection::open_in_memory().unwrap();
    router(App::new(db, reqwest::Client::new()).unwrap())
}

fn canonical_link(code: &Value, path: &str) -> String {
    format!(
        "{CANONICAL}{path}?code={}",
        code["user_code"].as_str().unwrap()
    )
}

#[tokio::test]
async fn a_viewing_host_creates_claims_and_refreshes_its_own_pairing() {
    let app = application();
    let mut browser = Client::new(CANONICAL_HOST, Some(CANONICAL));
    assert_eq!(
        browser
            .post(
                &app,
                "/api/auth/register",
                json!({"username":"viewer","name":"Viewer","password":"long-random-viewer-password"}),
            )
            .await
            .0,
        StatusCode::OK
    );
    let mut tv = Client::new(VIEWING_HOST, Some(VIEWING));
    let (status, code) = tv
        .post(
            &app,
            "/api/auth/device/code",
            json!({"device_name":"Living room TV"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(code["verification_uri"], json!(format!("{CANONICAL}/device")));
    assert_eq!(
        code["verification_uri_complete"],
        json!(canonical_link(&code, "/device"))
    );
    assert_eq!(
        code["qr_uri"],
        json!(canonical_link(&code, "/api/auth/device/qr"))
    );
    let device_code = code["device_code"].as_str().unwrap().to_owned();
    let (status, pending) = tv
        .post(
            &app,
            "/api/auth/device/token",
            json!({"device_code":device_code}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(pending["error_code"], "authorization_pending");
    assert_eq!(
        browser
            .post(
                &app,
                "/api/auth/device/approve",
                json!({"user_code":code["user_code"]}),
            )
            .await
            .0,
        StatusCode::OK
    );
    let (status, device) = tv
        .post(
            &app,
            "/api/auth/device/token",
            json!({"device_code":device_code}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let refresh_token = device["refresh_token"].as_str().unwrap().to_owned();
    let (status, rotated) = tv
        .post(
            &app,
            "/api/auth/device/refresh",
            json!({"refresh_token":refresh_token}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let mut viewer = Client::new(VIEWING_HOST, Some(VIEWING));
    viewer.bearer = rotated["access_token"].as_str().map(str::to_owned);
    let (status, profile) = viewer
        .post(
            &app,
            "/api/profiles",
            json!({"name":"TV Kid","avatar_id":"zayne"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        viewer
            .post(
                &app,
                "/api/auth/profile",
                json!({"profile_id":profile["id"]}),
            )
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        viewer.post(&app, "/api/auth/logout", json!({})).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn unlisted_origins_and_browser_routes_stay_forbidden() {
    let app = application();
    let mut tv = Client::new(VIEWING_HOST, Some(VIEWING));
    let (status, code) = tv
        .post(
            &app,
            "/api/auth/device/code",
            json!({"device_name":"Living room TV"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let mut foreign = Client::new(VIEWING_HOST, Some("https://unlisted.example"));
    assert_eq!(
        foreign
            .post(
                &app,
                "/api/auth/device/code",
                json!({"device_name":"Foreign origin"}),
            )
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        foreign
            .post(
                &app,
                "/api/auth/device/token",
                json!({"device_code":code["device_code"]}),
            )
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        tv.post(
            &app,
            "/api/auth/login",
            json!({"username":"viewer","password":"long-random-viewer-password"}),
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}
