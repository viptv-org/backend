use crate::{auth_integration_tests::fixture, test_support::request, *};
use axum::{
    extract::OriginalUri,
    http::{HeaderMap, HeaderValue, Uri},
};

async fn stale_approval_cannot_recreate_a_family(revoke: Option<bool>) {
    let app = fixture();
    let principal = {
        let db = app.db.lock().unwrap();
        db.execute("INSERT INTO auth_sessions(id,account_id,access_hash,refresh_hash,csrf_hash,kind,device_name,access_expires,refresh_expires,created_at) VALUES('queued-browser',1,?1,'fixture-refresh','unused','browser','Queued browser',?2,?2,1)",params![auth::hash("queued-access"),auth::now()+3600]).unwrap();
        db.execute("INSERT INTO auth_pairings(code_hash,device_hash,device_name,expires) VALUES(?1,?2,'Queued TV',?3)",params![auth::hash("00000001"),auth::hash("fixture-device-code"),auth::now()+600]).unwrap();
        let captured = auth::Principal::Account {
            account_id: 1,
            role: "owner".into(),
            profile_id: None,
            session_id: Some("queued-browser".into()),
        };
        captured.validate_scope(&db).unwrap();
        captured
    };
    if let Some(all) = revoke {
        let path = if all {
            "/api/account/devices"
        } else {
            "/api/account/devices/queued-browser"
        };
        assert_eq!(
            request(&app, "member-token-1", "DELETE", path, Value::Null)
                .await
                .0,
            StatusCode::OK
        );
    }
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("fixture.invalid"));
    headers.insert(
        header::ORIGIN,
        HeaderValue::from_static("https://fixture.invalid"),
    );
    let approved = auth::action(
        State(app.clone()),
        OriginalUri(Uri::from_static("/api/auth/device/approve")),
        Some(Extension(principal)),
        headers,
        axum::Json(json!({"user_code":"00000001"})),
    )
    .await;
    let (polled, _) = request(
        &app,
        "",
        "POST",
        "/api/auth/device/token",
        json!({"device_code":"fixture-device-code"}),
    )
    .await;
    let db = app.db.lock().unwrap();
    let remaining = db
        .query_row(
            "SELECT count(*) FROM auth_sessions WHERE account_id=1",
            [],
            |r| r.get::<_, u64>(0),
        )
        .unwrap();
    let approved_count = db
        .query_row(
            "SELECT count(*) FROM auth_pairings WHERE account_id=1",
            [],
            |r| r.get::<_, u64>(0),
        )
        .unwrap();
    let expected = match revoke {
        Some(true) => (StatusCode::UNAUTHORIZED, StatusCode::BAD_REQUEST, 0, 0),
        Some(false) => (StatusCode::UNAUTHORIZED, StatusCode::BAD_REQUEST, 1, 0),
        None => (StatusCode::OK, StatusCode::OK, 3, 0),
    };
    assert_eq!((approved.status(),polled,remaining,approved_count),expected,
        "revoked captured authority must not approve or mint a new family; live authority remains supported");
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM auth_sessions WHERE account_id=2",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        1
    );
}
#[tokio::test]
async fn stale_approval_after_all_family_revocation_cannot_mint_a_new_device() {
    stale_approval_cannot_recreate_a_family(Some(true)).await;
}
#[tokio::test]
async fn stale_approval_after_browser_family_revocation_cannot_mint_a_new_device() {
    stale_approval_cannot_recreate_a_family(Some(false)).await;
}

#[tokio::test]
async fn live_approval_still_mints_a_device_with_current_browser_authority() {
    stale_approval_cannot_recreate_a_family(None).await;
}
