use crate::{auth_integration_tests::fixture, test_support::request, *};
use axum::{response::IntoResponse, routing::get, Router};
use base64::Engine;

fn app() -> App {
    let mut app = fixture();
    let vault=Arc::new(secret_store::Vault::from_json(&json!({"active":"fixture","keys":{"fixture":base64::engine::general_purpose::STANDARD.encode([7u8;32])}}).to_string()).unwrap());
    app.secret_vault = Some(vault.clone());
    app.providers.vault = Some(vault);
    app.providers.allow_test_loopback = true;
    app
}
async fn upstream() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/player_api.php",
                get(|Query(q): Query<HashMap<String, String>>| async move {
            if q.get("action").is_some_and(|action|action=="get_series_info") {
                return axum::Json(json!({"episodes":{"1":[{"id":2,"season":1,"episode_num":2}]},"upstream_url":"http://fixture.invalid/private-cache-password"})).into_response();
            }
            match q.get("password").map(String::as_str) {
                        Some("rejected") => {
                            (StatusCode::UNAUTHORIZED, "secret upstream rejection").into_response()
                        }
                        Some("redirect") => {
                            axum::response::Redirect::temporary("http://127.0.0.1:1/private-secret")
                                .into_response()
                        }
                        Some("rate") => (StatusCode::TOO_MANY_REQUESTS, "upstream password secret")
                            .into_response(),
                        Some("large") => "x".repeat(300_000).into_response(),
                        _ => axum::Json(
                            json!({"user_info":{"auth":1,"status":"Active","max_connections":"5"}}),
                        )
                        .into_response(),
                    }
                }),
            ),
        )
        .await
        .unwrap();
    });
    (base, task)
}
fn input(base: &str, user: &str, password: &str) -> Value {
    json!({"name":"Fixture account","url":base,"username":user,"password":password})
}
const ROOT: &str = "/api/v2/iptv/connections";

#[tokio::test]
async fn connection_crud_encrypts_immediately_and_preserves_account_defaults() {
    let app = app();
    let (base, server) = upstream().await;
    let (status, first) = request(
        &app,
        "member-token-1",
        "POST",
        ROOT,
        input(&base, "user-one", "private-password"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let id = first["id"].as_i64().unwrap();
    assert_eq!(first["credentials_encrypted"], true);
    assert!(!first.to_string().contains("private-password"));
    {
        let db = app.db.lock().unwrap();
        assert_eq!(
            db.query_row("SELECT password FROM providers WHERE id=?1", [id], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            ""
        );
        let secret: String = db
            .query_row(
                "SELECT secret FROM provider_credentials_v2 WHERE provider_id=?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            !secret.contains("private-password")
                && !secret.contains("user-one")
                && !secret.contains(&base)
        );
        assert_eq!(provider::v2::live_catalog(&db, 1, None).unwrap(), Some(id));
        assert_eq!(
            db.query_row(
                "SELECT max_connections FROM providers WHERE id=?1",
                [id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            5
        );
    }
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            ROOT,
            input(&base, "user-one", "private-password")
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    // A different account may have the same provider login without disclosure.
    assert_eq!(
        request(
            &app,
            "member-token-2",
            "POST",
            ROOT,
            input(&base, "user-one", "private-password")
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, second) = request(
        &app,
        "member-token-1",
        "POST",
        ROOT,
        input(&base, "user-two", "private-password"),
    )
    .await;
    let second = second["id"].as_i64().unwrap();
    let (_, page) = request(
        &app,
        "member-token-1",
        "GET",
        &format!("{ROOT}?limit=1"),
        Value::Null,
    )
    .await;
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    let cursor = page["next_cursor"].as_str().unwrap();
    let (_, next) = request(
        &app,
        "member-token-1",
        "GET",
        &format!("{ROOT}?cursor={cursor}"),
        Value::Null,
    )
    .await;
    assert_eq!(next["items"][0]["id"], second);
    assert_eq!(
        request(
            &app,
            "member-token-2",
            "GET",
            &format!("{ROOT}?cursor={cursor}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let path = format!("{ROOT}/{id}");
    assert_eq!(
        request(
            &app,
            "member-token-2",
            "PATCH",
            &path,
            json!({"enabled":false})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(&app, "member-token-2", "DELETE", &path, Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        provider::v2::live_catalog(&app.db.lock().unwrap(), 1, None).unwrap(),
        Some(id)
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PATCH",
            &path,
            json!({"enabled":false})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        provider::v2::live_catalog(&app.db.lock().unwrap(), 1, None).unwrap(),
        Some(second)
    );
    let renewal = format!("{path}/credentials");
    let before: String = app
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT secret FROM provider_credentials_v2 WHERE provider_id=?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            &renewal,
            json!({"password":"rejected"})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT secret FROM provider_credentials_v2 WHERE provider_id=?1",
                [id],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        before
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            &renewal,
            json!({"password":"new-private-password"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_ne!(
        app.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT secret FROM provider_credentials_v2 WHERE provider_id=?1",
                [id],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        before
    );
    let permit = app
        .providers
        .acquire_playback_for_kind(second, "movie")
        .await
        .unwrap();
    assert!(app
        .providers
        .playback_gates
        .lock()
        .unwrap()
        .contains_key(&-second));
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "DELETE",
            &format!("{ROOT}/{second}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(!app
        .providers
        .playback_gates
        .lock()
        .unwrap()
        .contains_key(&-second));
    drop(permit);
    assert_eq!(
        provider::v2::live_catalog(&app.db.lock().unwrap(), 1, None).unwrap(),
        None
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PATCH",
            &path,
            json!({"enabled":true})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        provider::v2::live_catalog(&app.db.lock().unwrap(), 1, None).unwrap(),
        Some(id)
    );
    server.abort();
}

#[tokio::test]
async fn rejected_checks_never_store_credentials_or_echo_upstream_diagnostics() {
    let app = app();
    let (base, server) = upstream().await;
    for (password, code) in [
        ("rejected", "provider_credentials_rejected"),
        ("redirect", "provider_redirect_rejected"),
        ("rate", "provider_rate_limited"),
        ("large", "provider_response_too_large"),
    ] {
        let (_, error) = request(
            &app,
            "member-token-1",
            "POST",
            ROOT,
            input(&base, "private-user", password),
        )
        .await;
        assert_eq!(error["error_code"], code, "{error}");
        assert!(
            !error.to_string().contains("private-user")
                && !error.to_string().contains("private-secret")
                && !error.to_string().contains("upstream password")
        );
    }
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM providers", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    app.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE auth_sessions SET kind='device' WHERE account_id=1",
            [],
        )
        .unwrap();
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            ROOT,
            input(&base, "user", "password")
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    server.abort();
}

#[test]
fn public_http_is_supported_but_private_and_credential_bearing_endpoints_are_not() {
    use super::transport_v2::base;
    assert!(base("http://8.8.8.8/base", false).is_ok());
    assert!(base("https://provider.example/base/player_api.php", false).is_ok());
    for raw in [
        "http://127.0.0.1",
        "http://2130706433",
        "http://169.254.169.254",
        "http://10.0.0.1",
        "http://[::1]",
        "http://[::ffff:127.0.0.1]",
        "http://LOCALHOST./",
        "http://box.local/",
        "http://user:secret@provider.example",
        "http://provider.example?password=secret",
        "file:///etc/passwd",
    ] {
        assert!(base(raw, false).is_err(), "{raw}");
    }
}

#[tokio::test]
async fn protected_detail_cache_is_encrypted_and_survives_offline_read() {
    let app = app();
    let (base, server) = upstream().await;
    let (status, created) = request(
        &app,
        "member-token-1",
        "POST",
        ROOT,
        input(&base, "private-user", "private-password"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = created["id"].as_i64().unwrap();
    let service = app.providers.for_account(1);
    let provider = service.provider(id).unwrap();
    let details = service
        .cached_api(&provider, "get_series_info", "7")
        .await
        .unwrap();
    assert!(details["upstream_url"]
        .as_str()
        .unwrap()
        .contains("private-cache-password"));
    let payload: String = app
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT payload FROM provider_cache WHERE provider_id=?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!payload.contains("private-cache-password") && !payload.contains("upstream_url"));
    server.abort();
    let _ = server.await;
    assert_eq!(
        service
            .cached_api(&provider, "get_series_info", "7")
            .await
            .unwrap(),
        details
    );
}

#[tokio::test]
async fn connection_validation_rechecks_revoked_account_before_persistence() {
    let app = app();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let entering = entered.clone();
    let releasing = release.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/player_api.php",
                get(move || {
                    let entered = entering.clone();
                    let release = releasing.clone();
                    async move {
                        entered.notify_one();
                        release.notified().await;
                        axum::Json(json!({"user_info":{"auth":1,"status":"Active"}}))
                    }
                }),
            ),
        )
        .await
        .unwrap();
    });
    let target = app.clone();
    let creation = tokio::spawn(async move {
        request(
            &target,
            "member-token-1",
            "POST",
            ROOT,
            input(&base, "user", "password"),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE auth_accounts SET disabled=1 WHERE id=1", [])
        .unwrap();
    release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(3), creation)
        .await
        .unwrap()
        .unwrap();
    assert!(!result.0.is_success());
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM providers", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    server.abort();
}
