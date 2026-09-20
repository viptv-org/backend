//! Public account-only gateway contract through the real router.
mod common;

use axum::http::StatusCode;
use serde_json::{json, Value};

use common::{application, call};

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
