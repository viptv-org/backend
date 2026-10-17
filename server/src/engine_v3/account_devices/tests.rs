use crate::{auth_integration_tests::fixture, test_support::request, *};
use axum::{body::Body, http::Request};
use tower::ServiceExt;

fn session(app: &App, id: &str, account: i64, kind: &str) {
    app.db.lock().unwrap().execute("INSERT INTO auth_sessions(id,account_id,access_hash,refresh_hash,csrf_hash,kind,device_name,access_expires,refresh_expires,created_at) VALUES(?1,?2,?3,?4,'unused',?5,'Fixture device',?6,?6,1)",params![id,account,auth::hash(id),format!("refresh-{id}"),kind,auth::now()+3600]).unwrap();
}

#[tokio::test]
async fn account_devices_include_both_kinds_and_preserve_family_scope() {
    let app = fixture();
    session(&app, "own-tv", 1, "device");
    session(&app, "other-browser", 1, "browser");
    let (status, data) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/account/devices",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{data}");
    let devices = data["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 3);
    let current = devices
        .iter()
        .find(|device| device["current"] == true)
        .unwrap();
    assert_eq!(current["id"], "s1");
    assert_eq!(current["can_sign_out"], false);
    assert!(devices
        .iter()
        .any(|device| device["id"] == "own-tv" && device["kind"] == "device"));
    assert_eq!(data["account"]["id"], "1");
    assert!(!data.to_string().contains("hash"));
    assert!(!data.to_string().contains("member-token"));
    assert_eq!(
        request(
            &app,
            "own-tv",
            "GET",
            "/api/account/devices",
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "DELETE",
            "/api/account/devices/s2",
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "DELETE",
            "/api/account/devices/s1",
            Value::Null
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "DELETE",
            "/api/account/devices/other-browser",
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, "other-browser", "GET", "/api/auth/me", Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "DELETE",
            "/api/account/devices/own-tv",
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, "own-tv", "GET", "/api/auth/me", Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(&app, "member-token-2", "GET", "/api/auth/me", Value::Null)
            .await
            .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn account_devices_sign_out_all_is_atomic_and_keeps_account_content() {
    let app = fixture();
    session(&app, "own-tv", 1, "device");
    let profile_count: i64 = app
        .db
        .lock()
        .unwrap()
        .query_row("SELECT count(*) FROM profiles", [], |row| row.get(0))
        .unwrap();
    {
        let db = app.db.lock().unwrap();
        db.execute("INSERT INTO favorites(profile_id,id,type,name) VALUES(1,'saved-title','movie','Saved')",[]).unwrap();
        db.execute("INSERT INTO progress(profile_id,id,type,name,position,duration,updated_at) VALUES(1,'watched-episode','series','Episode',123,900,1)",[]).unwrap();
        db.execute("INSERT INTO auth_pairings(code_hash,device_hash,device_name,expires,account_id) VALUES('approved-code','approved-device','Pending TV',?1,1)",[auth::now()+600]).unwrap();
    }
    let (status, body) = request(
        &app,
        "member-token-1",
        "DELETE",
        "/api/account/devices",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["signed_out"], true);
    assert_eq!(
        request(&app, "member-token-1", "GET", "/api/auth/me", Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(&app, "own-tv", "GET", "/api/auth/me", Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(&app, "member-token-2", "GET", "/api/auth/me", Value::Null)
            .await
            .0,
        StatusCode::OK
    );
    let db = app.db.lock().unwrap();
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM auth_pairings WHERE account_id=1",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM profiles", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        profile_count
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM auth_accounts", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        db.query_row(
            "SELECT position FROM progress WHERE profile_id=1 AND id='watched-episode'",
            [],
            |row| row.get::<_, f64>(0)
        )
        .unwrap(),
        123.0
    );
    assert_eq!(
        db.query_row(
            "SELECT name FROM favorites WHERE profile_id=1 AND id='saved-title'",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "Saved"
    );
}

#[tokio::test]
async fn account_devices_cookie_mutations_require_csrf_and_clear_cookies() {
    let app = fixture();
    app.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE auth_sessions SET csrf_hash=?1 WHERE id='s1'",
            [auth::hash("fixture-csrf")],
        )
        .unwrap();
    for csrf in [None, Some("fixture-csrf")] {
        let mut builder = Request::builder()
            .method("DELETE")
            .uri("/api/account/devices")
            .header("cookie", "viptv_session=member-token-1")
            .header("host", "fixture.invalid")
            .header("origin", "https://fixture.invalid");
        if let Some(value) = csrf {
            builder = builder.header("x-csrf-token", value);
        }
        let response = router(app.clone(), None)
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if csrf.is_some() {
                StatusCode::OK
            } else {
                StatusCode::FORBIDDEN
            }
        );
        if csrf.is_some() {
            assert_eq!(
                response
                    .headers()
                    .get_all(header::SET_COOKIE)
                    .iter()
                    .count(),
                2
            );
            assert!(response
                .headers()
                .get_all(header::SET_COOKIE)
                .iter()
                .all(|value| value.to_str().unwrap().contains("Max-Age=0")));
        }
    }
}

#[tokio::test]
async fn account_devices_parent_policy_blocks_reads_and_revocation() {
    let app = fixture();
    session(&app, "own-tv", 1, "device");
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO kids_profiles(profile_id,enabled) VALUES(1,1)",
            [],
        )
        .unwrap();
    for (method, path) in [
        ("GET", "/api/account/devices"),
        ("DELETE", "/api/account/devices/own-tv"),
        ("DELETE", "/api/account/devices"),
    ] {
        assert_eq!(
            request(&app, "member-token-1", method, path, Value::Null)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    let db = app.db.lock().unwrap();
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM auth_sessions WHERE account_id=1",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
}
