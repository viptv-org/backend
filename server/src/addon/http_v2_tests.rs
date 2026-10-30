use crate::{auth_integration_tests::fixture, test_support::request, *};
use axum::{response::IntoResponse, routing::get, Router};
use base64::Engine;
const ROOT: &str = "/api/addons";
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
    let id = first["id"].as_i64().unwrap();
    assert_eq!(
        first["logo"].as_str().unwrap().split('?').next(),
        Some(&*format!("{ROOT}/{id}/logo"))
    );
    assert!(!first.to_string().contains("art.example"));
    assert_eq!(first["credentials_encrypted"], true);
    let (_, again) = request(
        &app,
        "member-token-1",
        "POST",
        ROOT,
        json!({"manifest_url":url}),
    )
    .await;
    assert_eq!(again["id"], id);
    let (status, error) = request(
        &app,
        "member-token-2",
        "POST",
        ROOT,
        json!({"manifest_url":url}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error["error_code"], "owner_required");
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
        let (status, error) = request(
            &app,
            "member-token-2",
            method,
            &path,
            json!({"enabled":false}),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(error["error_code"], "owner_required");
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
    let (status, error) = request(
        &app,
        "member-token-1",
        "POST",
        ROOT,
        json!({"manifest_url":url}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error["error_code"], "account_session_required");
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
    let (status, error) = request(
        &app,
        "member-token-1",
        "POST",
        ROOT,
        json!({"manifest_url":url}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error["error_code"], "account_session_required");
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
    let (status, error) = request(
        &app,
        "member-token-1",
        "PATCH",
        &format!("{ROOT}/{id}"),
        json!({"enabled":false}),
    )
    .await;
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
async fn save_without_keyring_refuses_before_download_or_write() {
    let mut app = app();
    app.secret_vault = None;
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
    let saving = app.clone();
    let pending = tokio::spawn(async move {
        request(
            &saving,
            "member-token-1",
            "POST",
            ROOT,
            json!({"manifest_url":url}),
        )
        .await
    });
    let (status, error) = tokio::time::timeout(Duration::from_secs(3), pending)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error["error_code"], "secret_store_not_configured");
    assert!(
        tokio::time::timeout(Duration::from_millis(50), entered.notified())
            .await
            .is_err()
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


const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDRlogo-fixture";
async fn logo_upstream() -> (
    String,
    Arc<std::sync::atomic::AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    use axum::{extract::Path, http::header};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = hits.clone();
    let origin = base.clone();
    let task = tokio::spawn(async move {
        let router = Router::new()
            .route(
                "/:token/manifest.json",
                get(move |Path(token): Path<String>| {
                    let origin = origin.clone();
                    async move {
                        let mut manifest = json!({"id":format!("fixture.{token}"),"name":format!("Logo {}", token.split('-').next().unwrap()),"resources":["stream"],"types":["movie"]});
                        if !token.starts_with("plain") {
                            manifest["logo"] = json!(format!("{origin}/{token}/logo"));
                        }
                        axum::Json(manifest)
                    }
                }),
            )
            .route(
                "/:token/logo",
                get(move |Path(token): Path<String>| {
                    counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    async move {
                        let (kind, body): (&str, Vec<u8>) = match token.split('-').next().unwrap() {
                            "big" => ("image/png", [PNG, &vec![0u8; super::logo::LIMIT]].concat()),
                            "html" => ("text/html", b"<html>not a logo</html>".to_vec()),
                            "disguised" => ("text/html", PNG.to_vec()),
                            "svg" => ("image/svg+xml", b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>".to_vec()),
                            _ => ("image/png", PNG.to_vec()),
                        };
                        ([(header::CONTENT_TYPE, kind)], body)
                    }
                }),
            );
        axum::serve(listener, router).await.unwrap();
    });
    (base, hits, task)
}
async fn raw_get(
    app: &App,
    token: &str,
    path: &str,
) -> (StatusCode, axum::http::HeaderMap, axum::body::Bytes) {
    use tower::ServiceExt;
    let response = router(app.clone())
        .oneshot(
            axum::http::Request::builder()
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (status, headers, body)
}
async fn install(app: &App, base: &str, token: &str) -> Value {
    let (status, row) = request(
        app,
        "member-token-1",
        "POST",
        ROOT,
        json!({"manifest_url":format!("{base}/{token}/manifest.json")}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{row}");
    row
}
#[tokio::test]
async fn manifest_logos_are_served_only_through_the_owner_checked_route() {
    let app = app();
    let (base, hits, server) = logo_upstream().await;
    let row = install(&app, &base, "png-private-token").await;
    let id = row["id"].as_i64().unwrap();
    let logo = row["logo"].as_str().unwrap().to_owned();
    assert!(logo.starts_with(&format!("{ROOT}/{id}/logo?v=")), "{logo}");
    let (_, page) = request(&app, "member-token-1", "GET", ROOT, Value::Null).await;
    assert_eq!(page["items"][0]["logo"], logo);
    for listed in [&row, &page] {
        assert!(!listed.to_string().contains("private-token"));
        assert!(!listed.to_string().contains(&base));
    }

    let (status, headers, body) = raw_get(&app, "member-token-1", &logo).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..], PNG);
    assert_eq!(headers["content-type"], "image/png");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert!(headers["cache-control"].to_str().unwrap().starts_with("private"));
    let (status, _, body) = raw_get(&app, "member-token-1", &format!("{ROOT}/{id}/logo")).await;
    assert_eq!((status, &body[..]), (StatusCode::OK, PNG));
    let (status, _, body) = raw_get(&app, "member-token-1", &format!("{ROOT}/{id}/logo?v=")).await;
    assert_eq!((status, &body[..]), (StatusCode::OK, PNG));
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    let (status, headers, body) =
        raw_get(&app, "member-token-1", &format!("{ROOT}/{id}/logo?v=deadbeefcafe")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(headers["content-type"], "application/json");
    assert!(!String::from_utf8_lossy(&body).contains("private-token"));
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);

    let (status, _, body) = raw_get(&app, "member-token-2", &logo).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!String::from_utf8_lossy(&body).contains("private-token"));
    let (status, _, _) = raw_get(&app, "member-token-1", &format!("{ROOT}/999999/logo")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    server.abort();
}
#[tokio::test]
async fn missing_oversized_and_non_raster_logos_are_not_served() {
    let app = app();
    let (base, _, server) = logo_upstream().await;
    let plain = install(&app, &base, "plain-private-token").await;
    assert_eq!(plain["logo"], Value::Null);
    let (status, _, body) =
        raw_get(&app, "member-token-1", &format!("{ROOT}/{}/logo", plain["id"])).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!String::from_utf8_lossy(&body).contains(&base));
    for token in ["big", "html", "disguised", "svg"] {
        let row = install(&app, &base, &format!("{token}-private-token")).await;
        let path = row["logo"].as_str().unwrap();
        let (status, headers, body) = raw_get(&app, "member-token-1", path).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{token}");
        assert_eq!(headers["content-type"], "application/json", "{token}");
        let body = String::from_utf8_lossy(&body);
        assert!(!body.contains("private-token") && !body.contains("<svg"), "{token}: {body}");
    }
    server.abort();
}

const REVISED: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDRlogo-revised";
struct RevisionUpstream {
    base: String,
    hits: Arc<std::sync::atomic::AtomicUsize>,
    revision: Arc<std::sync::atomic::AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl RevisionUpstream {
    async fn start() -> Self {
        use axum::{extract::Path, http::header};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let base = format!("http://{address}");
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let revision = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let origin = base.clone();
        let counted = hits.clone();
        let served = hits.clone();
        let current = revision.clone();
        let task = tokio::spawn(async move {
            let router = Router::new()
                .route(
                    "/:token/manifest.json",
                    get({
                        let current = current.clone();
                        move |Path(token): Path<String>| {
                            let origin = origin.clone();
                            let current = current.clone();
                            async move {
                                let mut manifest = json!({"id":format!("fixture.{token}"),"name":"Logo revision","resources":["stream"],"types":["movie"]});
                                if !token.starts_with("plain") {
                                    manifest["logo"] = json!(match current.load(std::sync::atomic::Ordering::SeqCst) {
                                        0 => format!("{origin}/{token}/logo.png"),
                                        _ => format!("{origin}/{token}/logo-revised.png"),
                                    });
                                }
                                axum::Json(manifest)
                            }
                        }
                    }),
                )
                .route(
                    "/:token/logo.png",
                    get(move || {
                        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        async { ([(header::CONTENT_TYPE, "image/png")], PNG.to_vec()) }
                    }),
                )
                .route(
                    "/:token/logo-revised.png",
                    get(move || {
                        served.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        async { ([(header::CONTENT_TYPE, "image/png")], REVISED.to_vec()) }
                    }),
                )
                .route("/", axum::routing::any(|| async { StatusCode::OK }));
            axum::serve(listener, router).await.unwrap();
        });
        crate::test_support::send_port_probe(address).await;
        Self {
            base,
            hits,
            revision,
            task,
        }
    }
}
#[tokio::test]
async fn a_changed_manifest_logo_changes_the_bytes_for_the_new_revision_only() {
    let app = app();
    let upstream = RevisionUpstream::start().await;
    let token = "png-private-token";
    let first = install(&app, &upstream.base, token).await;
    let id = first["id"].as_i64().unwrap();
    let old = first["logo"].as_str().unwrap().to_owned();
    let (status, _, body) = raw_get(&app, "member-token-1", &old).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..], PNG);

    upstream
        .revision
        .store(1, std::sync::atomic::Ordering::SeqCst);
    let again = install(&app, &upstream.base, token).await;
    assert_eq!(again["id"], id);
    let (_, page) = request(&app, "member-token-1", "GET", ROOT, Value::Null).await;
    let current = page["items"][0]["logo"].as_str().unwrap().to_owned();
    assert_ne!(current, old);
    let (status, headers, body) = raw_get(&app, "member-token-1", &current).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "image/png");
    assert_eq!(&body[..], REVISED);
    let (status, _, _) = raw_get(&app, "member-token-1", &old).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(upstream.hits.load(std::sync::atomic::Ordering::SeqCst), 2);
    upstream.task.abort();
}
#[tokio::test]
async fn an_addon_without_a_logo_serves_no_revision() {
    let app = app();
    let upstream = RevisionUpstream::start().await;
    let row = install(&app, &upstream.base, "plain-private-token").await;
    assert_eq!(row["logo"], Value::Null);
    let (_, page) = request(&app, "member-token-1", "GET", ROOT, Value::Null).await;
    assert_eq!(page["items"][0]["logo"], Value::Null);
    let id = row["id"].as_i64().unwrap();
    for suffix in ["", "?v=", "?v=deadbeefcafe"] {
        let (status, headers, body) =
            raw_get(&app, "member-token-1", &format!("{ROOT}/{id}/logo{suffix}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{suffix}");
        assert_eq!(headers["content-type"], "application/json", "{suffix}");
        assert!(!String::from_utf8_lossy(&body).contains("private-token"), "{suffix}");
    }
    assert_eq!(upstream.hits.load(std::sync::atomic::Ordering::SeqCst), 0);
    upstream.task.abort();
}
