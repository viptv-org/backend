use crate::{auth_integration_tests::fixture, test_support::request, *};
use axum::{response::IntoResponse, routing::get, Router};
use base64::Engine;
const ROOT: &str = "/api/v2/addons";
fn app() -> App {
    let mut app = fixture();
    let vault=Arc::new(secret_store::Vault::from_json(&json!({"active":"fixture","keys":{"fixture":base64::engine::general_purpose::STANDARD.encode([7u8;32])}}).to_string()).unwrap());
    app.secret_vault = Some(vault.clone());
    app.addons.vault = Some(vault);
    app.addons.allow_test_loopback = true;
    app
}
async fn upstream() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener,Router::new().route("/:token/manifest.json",get(||async {axum::Json(json!({"id":"fixture","name":"Fixture addon","resources":["stream"],"types":["movie"],"logo":"https://art.example/icon.png"}))}))).await.unwrap();
    });
    (base, task)
}
#[tokio::test]
async fn account_management_is_encrypted_paged_redacted_and_preserves_identity() {
    let app = app();
    let (base, server) = upstream().await;
    let url = format!("{base}/private-token/manifest.json");
    let (status, first) = request(
        &app,
        "member-token-1",
        "POST",
        ROOT,
        json!({"manifest_url":url}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["manifest_url"], Value::Null);
    assert_eq!(first["logo"], "https://art.example/icon.png");
    assert_eq!(first["credentials_encrypted"], true);
    let id = first["id"].as_i64().unwrap();
    let (_, again) = request(
        &app,
        "member-token-1",
        "POST",
        ROOT,
        json!({"manifest_url":url}),
    )
    .await;
    assert_eq!(again["id"], id);
    assert_eq!(
        request(
            &app,
            "member-token-2",
            "POST",
            ROOT,
            json!({"manifest_url":url})
        )
        .await
        .0,
        StatusCode::OK
    );
    request(
        &app,
        "member-token-1",
        "POST",
        ROOT,
        json!({"manifest_url":format!("{base}/another-token/manifest.json")}),
    )
    .await;
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
    let (_, next) = request(
        &app,
        "member-token-1",
        "GET",
        &format!("{ROOT}?cursor={cursor}"),
        Value::Null,
    )
    .await;
    assert_eq!(next["items"].as_array().unwrap().len(), 1);
    assert!(!page.to_string().contains("private-token") && !page.to_string().contains(&base));
    let path = format!("{ROOT}/{id}");
    for method in ["PATCH", "DELETE"] {
        assert_eq!(
            request(
                &app,
                "member-token-2",
                method,
                &path,
                json!({"enabled":false})
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
    }
    let (_, disabled) = request(
        &app,
        "member-token-1",
        "PATCH",
        &path,
        json!({"enabled":false}),
    )
    .await;
    assert_eq!(disabled["enabled"], false);
    assert_eq!(
        request(&app, "member-token-1", "DELETE", &path, Value::Null)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM addon_credentials_v2 WHERE addon_id=?1",
                [id],
                |r| r.get::<_, i64>(0)
            )
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
        request(&app, "member-token-1", "GET", ROOT, Value::Null)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            ROOT,
            json!({"manifest_url":url})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    app.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE auth_sessions SET profile_id=NULL WHERE account_id=1",
            [],
        )
        .unwrap();
    let (status, error) = request(&app, "member-token-1", "GET", ROOT, Value::Null).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error["error_code"], "profile_required");
    app.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE auth_sessions SET profile_id=1 WHERE account_id=1",
            [],
        )
        .unwrap();
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO kids_profiles(profile_id,enabled) VALUES(1,1)",
            [],
        )
        .unwrap();
    let (status, error) = request(&app, "member-token-1", "GET", ROOT, Value::Null).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error["error_code"], "parent_required");
    server.abort();
}
#[tokio::test]
async fn rejected_manifest_addresses_do_not_register_or_echo_tokens() {
    let mut app = app();
    app.addons.allow_test_loopback = false;
    for url in [
        "http://127.0.0.1/private-token/manifest.json",
        "http://169.254.169.254/manifest.json",
        "http://localhost/manifest.json",
        "file:///private-token/manifest.json",
        "https://user:private-token@public.example/manifest.json",
        "https://public.example/not-a-manifest",
    ] {
        let (status, error) = request(
            &app,
            "member-token-1",
            "POST",
            ROOT,
            json!({"manifest_url":url}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
        assert!(!error.to_string().contains("private-token"));
    }
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM addons", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}
#[tokio::test]
async fn manifest_download_rechecks_session_before_writing() {
    let app = app();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let entering = entered.clone();
    let releasing = release.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/private-token/manifest.json",
        listener.local_addr().unwrap()
    );
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/private-token/manifest.json",
                get(move || {
                    let entered = entering.clone();
                    let release = releasing.clone();
                    async move {
                        entered.notify_one();
                        release.notified().await;
                        axum::Json(json!({"id":"fixture","name":"Fixture","resources":[]}))
                    }
                }),
            ),
        )
        .await
        .unwrap();
    });
    let calling = app.clone();
    let task = tokio::spawn(async move {
        request(
            &calling,
            "member-token-1",
            "POST",
            ROOT,
            json!({"manifest_url":url}),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    app.db
        .lock()
        .unwrap()
        .execute("DELETE FROM auth_sessions WHERE id='s1'", [])
        .unwrap();
    release.notify_one();
    assert!(!tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .0
        .is_success());
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM addons", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    server.abort();
}
#[tokio::test]
async fn protected_redirects_allow_public_chains_but_reject_private_targets_and_loops() {
    let app = app();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route(
                    "/private-token/manifest.json",
                    get(|| async { axum::response::Redirect::temporary("/final/manifest.json") }),
                )
                .route(
                    "/final/manifest.json",
                    get(
                        |headers: axum::http::HeaderMap, uri: axum::http::Uri| async move {
                            assert!(headers.get("referer").is_none());
                            assert!(headers.get("authorization").is_none());
                            assert!(headers.get("cookie").is_none());
                            assert!(uri.query().is_none());
                            axum::Json(json!({"id":"fixture","name":"Redirected","resources":[]}))
                        },
                    ),
                )
                .route(
                    "/unsafe/manifest.json",
                    get(|| async {
                        axum::response::Redirect::temporary(
                            "http://169.254.169.254/private-token/manifest.json",
                        )
                    }),
                )
                .route(
                    "/loop/manifest.json",
                    get(|| async { axum::response::Redirect::temporary("/loop/manifest.json") }),
                )
                .route(
                    "/invalid/manifest.json",
                    get(|| async {
                        (StatusCode::OK, "private-token invalid JSON").into_response()
                    }),
                ),
        )
        .await
        .unwrap();
    });
    let (status, created) = request(
        &app,
        "member-token-1",
        "POST",
        ROOT,
        json!({"manifest_url":format!("{base}/private-token/manifest.json?api_key=private-token")}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["name"], "Redirected");
    for (path, code) in [
        ("unsafe", "addon_private_destination"),
        ("loop", "addon_redirect_rejected"),
        ("invalid", "addon_protocol_invalid"),
    ] {
        let (_, error) = request(
            &app,
            "member-token-1",
            "POST",
            ROOT,
            json!({"manifest_url":format!("{base}/{path}/manifest.json")}),
        )
        .await;
        assert_eq!(error["error_code"], code, "{error}");
        assert!(!error.to_string().contains("private-token"));
    }
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM addons", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    task.abort();
}

#[tokio::test]
async fn deleting_an_addon_during_reinstall_cannot_resurrect_it() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let app = app();
    let hold = Arc::new(AtomicBool::new(false));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let holding = hold.clone();
    let entering = entered.clone();
    let releasing = release.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/private-token/manifest.json",
        listener.local_addr().unwrap()
    );
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/private-token/manifest.json",
                get(move || {
                    let hold = holding.load(Ordering::Acquire);
                    let entered = entering.clone();
                    let release = releasing.clone();
                    async move {
                        if hold {
                            entered.notify_one();
                            release.notified().await;
                        }
                        axum::Json(json!({"id":"fixture","name":"Fixture","resources":[]}))
                    }
                }),
            ),
        )
        .await
        .unwrap();
    });
    let (_, first) = request(
        &app,
        "member-token-1",
        "POST",
        ROOT,
        json!({"manifest_url":url}),
    )
    .await;
    let id = first["id"].as_i64().unwrap();
    hold.store(true, Ordering::Release);
    let calling = app.clone();
    let task = tokio::spawn(async move {
        request(
            &calling,
            "member-token-1",
            "POST",
            ROOT,
            json!({"manifest_url":url}),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "DELETE",
            &format!("{ROOT}/{id}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    release.notify_one();
    let (status, error) = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error_code"], "addon_configuration_changed");
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM addons", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    server.abort();
}

#[tokio::test]
async fn protected_cache_never_reuses_legacy_or_another_accounts_response() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let app = app();
    let calls = Arc::new(AtomicUsize::new(0));
    let called = calls.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/data", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/data",
                get(move || {
                    let call = called.fetch_add(1, Ordering::SeqCst) + 1;
                    async move { axum::Json(json!({"call":call})) }
                }),
            ),
        )
        .await
        .unwrap();
    });
    let mut legacy = app.addons.clone().for_account(1);
    legacy.vault = None;
    assert_eq!(legacy.fetch(&url, 300).await.unwrap()["call"], 1);
    let secure = legacy.with_protected_fetch();
    assert_eq!(secure.fetch(&url, 300).await.unwrap()["call"], 2);
    assert_eq!(secure.fetch(&url, 300).await.unwrap()["call"], 2);
    assert_eq!(
        secure.for_account(2).fetch(&url, 300).await.unwrap()["call"],
        3
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    server.abort();
}

#[tokio::test]
async fn stalled_installs_do_not_take_viewing_request_slots() {
    let app = app();
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let releasing = release.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route(
                    "/:token/manifest.json",
                    get(move || {
                        let release = releasing.clone();
                        async move {
                            let _permit = release.acquire().await.unwrap();
                            axum::Json(json!({"id":"fixture","name":"Fixture","resources":[]}))
                        }
                    }),
                )
                .route(
                    "/data",
                    get(|| async { axum::Json(json!({"viewing":true})) }),
                ),
        )
        .await
        .unwrap();
    });
    let mut tasks = vec![];
    for token in ["one", "two"] {
        let calling = app.clone();
        let url = format!("{base}/{token}/manifest.json");
        tasks.push(tokio::spawn(async move {
            request(
                &calling,
                "member-token-1",
                "POST",
                ROOT,
                json!({"manifest_url":url}),
            )
            .await
        }));
    }
    tokio::time::timeout(Duration::from_secs(3), async {
        while app.addons.manifest_gate.available_permits() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let (status, error) = request(
        &app,
        "member-token-1",
        "POST",
        ROOT,
        json!({"manifest_url":format!("{base}/three/manifest.json")}),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error["error_code"], "addon_checks_busy");
    let view = tokio::time::timeout(
        Duration::from_secs(3),
        app.addons
            .clone()
            .for_account(1)
            .with_protected_fetch()
            .fetch(&format!("{base}/data"), 60),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(view["viewing"], true);
    release.add_permits(2);
    for task in tasks {
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), task)
                .await
                .unwrap()
                .unwrap()
                .0,
            StatusCode::OK
        );
    }
    server.abort();
}

#[tokio::test]
async fn legacy_save_rechecks_encryption_protection_after_download() {
    let app = app();
    let mut old = app.addons.clone().for_account(1);
    old.vault = None;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let entering = entered.clone();
    let releasing = release.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/private-token/manifest.json",
        listener.local_addr().unwrap()
    );
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/private-token/manifest.json",
                get(move || {
                    let entered = entering.clone();
                    let release = releasing.clone();
                    async move {
                        entered.notify_one();
                        release.notified().await;
                        axum::Json(json!({"id":"fixture","name":"Fixture","resources":[]}))
                    }
                }),
            ),
        )
        .await
        .unwrap();
    });
    let pending = tokio::spawn(async move { old.add(&url).await });
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    app.db
        .lock()
        .unwrap()
        .execute("INSERT INTO addon_encryption_accounts_v2 VALUES(1)", [])
        .unwrap();
    release.notify_one();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), pending)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err(),
        "secret_store_not_configured"
    );
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM addons", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    server.abort();
}
