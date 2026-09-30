use crate::{auth_integration_tests::fixture, test_support::request, *};
use axum::{response::IntoResponse, routing::get, Router};
use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test]
async fn unsupported_required_header_events_are_safe_and_keep_healthy_siblings() {
    let app = fixture();
    app.db.lock().unwrap().execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id) VALUES(7,'Fixture','https://addon.fixture.invalid/manifest.json','{}',1)",[]).unwrap();
    let mut owned = app.clone().with_lease(ResourceLease {
        policy_revision: 0,
        principal: auth::Principal::Account {
            account_id: 1,
            role: "member".into(),
            profile_id: Some(1),
            session_id: Some("s1".into()),
        },
        session_id: Some("s1".into()),
    });
    owned.providers = owned.providers.for_account(1);
    let job = Job {
        kind: "movie".into(),
        created: Instant::now(),
        state: Mutex::new(JobState {
            events: vec![],
            pending: 1,
        }),
        notify: Notify::new(),
    };
    emit(
        &owned,
        &job,
        "addon:7",
        Ok(vec![
            json!({"url":"https://fixture.invalid/private-input","name":"private-credential","behaviorHints":{"proxyHeaders":{"request":{"X-Unsupported-Credential":"private-credential"}}}}),
            json!({"url":"https://fixture.invalid/healthy","name":"Healthy"}),
        ]),
    );
    let state = job.state.lock().unwrap();
    assert_eq!(state.pending, 0);
    assert_eq!(state.events[0]["error_code"], "source_headers_unsupported");
    assert_eq!(
        state.events[0]["error"],
        account_api::description("source_headers_unsupported")
    );
    assert_eq!(state.events[0]["streams"].as_array().unwrap().len(), 1);
    assert_eq!(state.events[0]["streams"][0]["name"], "Healthy");
    assert!(!state.events[0].to_string().contains("private-credential"));
    assert!(!state.events[0].to_string().contains("private-input"));
}

struct Upstream {
    url: String,
    mode: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Upstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn upstream() -> Upstream {
    let mode = Arc::new(AtomicUsize::new(0));
    let iptv = mode.clone();
    let addon = mode.clone();
    fn failure(mode: usize) -> Response {
        match mode {
            0 => (
                StatusCode::UNAUTHORIZED,
                "upstream private-password private-token",
            )
                .into_response(),
            1 => (
                StatusCode::TOO_MANY_REQUESTS,
                "upstream private-password private-token",
            )
                .into_response(),
            2 => (
                StatusCode::SERVICE_UNAVAILABLE,
                "upstream private-password private-token",
            )
                .into_response(),
            3 => (
                StatusCode::OK,
                "invalid JSON private-password private-token",
            )
                .into_response(),
            _ => axum::Json(json!({"diagnostic":"private-password private-token"})).into_response(),
        }
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener,Router::new()
            .route("/player_api.php",get(move |Query(q):Query<HashMap<String,String>>| {let mode=iptv.load(Ordering::Relaxed);async move {
                if q["username"]=="good" {return axum::Json(json!({"episodes":{"1":[{"id":2,"season":1,"episode_num":2,"container_extension":"mp4"}]}})).into_response();}
                failure(mode)
            }}))
            .route("/private-token/stream/series/:id",get(move || {let mode=addon.load(Ordering::Relaxed);async move {failure(mode)}}))
        ).await.unwrap();
    });
    Upstream { url, mode, task }
}
async fn discover(app: &App, value: Value) -> Value {
    let (status, started) = request(app, "member-token-1", "POST", "/api/v2/streams", value).await;
    assert_eq!(status, StatusCode::OK, "{started}");
    let path = format!("/api/v2/streams/{}", started["id"].as_str().unwrap());
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (status, result) = request(app, "member-token-1", "GET", &path, Value::Null).await;
            assert_eq!(status, StatusCode::OK, "{result}");
            if result["done"] == true {
                return result;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}
fn no_diagnostics(value: &Value, base: &str) {
    let body = value.to_string();
    assert!(
        !body.contains("private-password")
            && !body.contains("private-token")
            && !body.contains("upstream private")
            && !body.contains(base),
        "{body}"
    );
}

#[tokio::test]
async fn iptv_discovery_and_guide_explain_failures_without_losing_healthy_sources() {
    let mut app = fixture();
    app.providers.allow_test_loopback = true;
    let upstream = upstream().await;
    {
        let db = app.db.lock().unwrap();
        for (id, user) in [(1, "bad"), (2, "good")] {
            db.execute("INSERT INTO providers(id,name,url,username,password) VALUES(?1,'Fixture provider',?2,?3,'private-password')",params![id,upstream.url,user]).unwrap();
            db.execute("INSERT INTO provider_ownership VALUES(?1,1)", [id])
                .unwrap();
            db.execute("INSERT INTO provider_vod(id,provider_id,stream_id,kind,name,normalized,year,imdb_id,extension) VALUES(?1,?2,'1','series','Show','show',2020,'tt1234567','mp4')",params![format!("iptv:{id}:series:1"),id]).unwrap();
            db.execute("INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES(?1,?2,'1','Channel')",params![format!("iptv:{id}:1"),id]).unwrap();
        }
    }
    for (mode, code, http_status) in [
        (
            0,
            "provider_credentials_rejected",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (1, "provider_rate_limited", StatusCode::TOO_MANY_REQUESTS),
        (2, "provider_unavailable", StatusCode::BAD_GATEWAY),
        (3, "provider_protocol_invalid", StatusCode::BAD_GATEWAY),
        (4, "provider_protocol_invalid", StatusCode::BAD_GATEWAY),
    ] {
        upstream.mode.store(mode, Ordering::Relaxed);
        let result=discover(&app,json!({"type":"series","id":"tt1234567:1:2","name":"Show","year":2020,"imdb_id":"tt1234567","tmdb_id":"123"})).await;
        let events = result["events"].as_array().unwrap();
        let error = events.iter().find(|e| e["source"] == "iptv:1").unwrap();
        assert_eq!(error["error_code"], code, "{result}");
        assert_eq!(error["error"], account_api::description(code));
        assert_ne!(error["error"], error["error_code"]);
        assert!(events
            .iter()
            .any(|e| e["source"] == "iptv:2" && e["streams"].as_array().unwrap().len() == 1));
        no_diagnostics(&result, &upstream.url);
        let (status, guide) = request(
            &app,
            "member-token-1",
            "GET",
            "/api/v2/iptv/guide/iptv:1:1",
            Value::Null,
        )
        .await;
        assert_eq!(status, http_status, "{guide}");
        assert_eq!(guide["error_code"], code);
        no_diagnostics(&guide, &upstream.url);
    }
}

#[tokio::test]
async fn addon_errors_and_invalid_success_bodies_are_not_silent_empty_results() {
    let mut app = fixture();
    app.addons.allow_test_loopback = true;
    let upstream = upstream().await;
    app.db.lock().unwrap().execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id) VALUES(1,'Fixture addon',?1,?2,1)",params![format!("{}/private-token/manifest.json",upstream.url),json!({"id":"fixture","name":"Fixture addon","version":"1.0.0","resources":["stream"],"types":["series"],"catalogs":[]}).to_string()]).unwrap();
    for (mode, code) in [
        (0, "addon_access_denied"),
        (1, "addon_rate_limited"),
        (2, "addon_unavailable"),
        (3, "addon_protocol_invalid"),
        (4, "addon_protocol_invalid"),
    ] {
        upstream.mode.store(mode, Ordering::Relaxed);
        let result = discover(
            &app,
            json!({"type":"series","id":format!("tt1234567:1:{}",mode+1),"only_addons":true}),
        )
        .await;
        let event = &result["events"][0];
        assert_eq!(event["error_code"], code, "{result}");
        assert_eq!(event["error"], account_api::description(code));
        no_diagnostics(&result, &upstream.url);
    }
}

#[tokio::test]
async fn discovery_rejections_are_structured_and_do_not_echo_request_details() {
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use tower::ServiceExt;
    let app = fixture();
    for body in [
        Value::Null,
        json!({"type":"series","id":"private-token","only_provider_id":"private-password"}),
    ] {
        let (status, error) =
            request(&app, "member-token-1", "POST", "/api/v2/streams", body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error["error_code"], "invalid_discovery_request");
        no_diagnostics(&error, "fixture.invalid");
    }
    let response = router(app.clone(), None)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v2/streams")
                .header("authorization", "Bearer member-token-1")
                .header("content-type", "application/json")
                .body(Body::from("{private-password"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let value: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(value["error_code"], "invalid_discovery_request");
    no_diagnostics(&value, "fixture.invalid");
    for query in ["after=-1", "after=abc", "unknown=value"] {
        let (status, error) = request(
            &app,
            "member-token-1",
            "GET",
            &format!("/api/v2/streams/missing?{query}"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error["error_code"], "invalid_discovery_cursor");
    }
    let (status, error) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v2/streams/missing",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error["error_code"], "discovery_not_found");
}

#[tokio::test]
async fn encrypted_addon_disable_blocks_late_and_cached_source_publication() {
    use base64::Engine;
    let mut app = fixture();
    let vault=Arc::new(secret_store::Vault::from_json(&json!({"active":"fixture","keys":{"fixture":base64::engine::general_purpose::STANDARD.encode([7u8;32])}}).to_string()).unwrap());
    app.secret_vault = Some(vault.clone());
    app.addons.vault = Some(vault);
    app.addons.allow_test_loopback = true;
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
        axum::serve(listener,Router::new().route("/private-token/manifest.json",get(||async {axum::Json(json!({"id":"fixture","name":"Fixture","resources":["stream"],"types":["movie"]}))}))
            .route("/private-token/stream/movie/:id",get(move || {let entered=entering.clone();let release=releasing.clone();async move {entered.notify_one();release.notified().await;axum::Json(json!({"streams":[{"url":"http://media.invalid/private-stream-token.mp4","name":"Fixture source"}]}))}}))).await.unwrap();
    });
    let owned = app.addons.clone().for_account(1);
    let addon = owned.add(&url).await.unwrap()["id"].as_i64().unwrap();
    let (_, started) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/streams",
        json!({"type":"movie","id":"tt1234567","only_addons":true}),
    )
    .await;
    let path = format!("/api/v2/streams/{}", started["id"].as_str().unwrap());
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    owned.update(addon, json!({"enabled":false})).unwrap();
    release.notify_one();
    async fn poll(app: &App, path: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let (_, value) = request(app, "member-token-1", "GET", path, Value::Null).await;
                if value["done"] == true {
                    break value;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap()
    }
    let late = poll(&app, &path).await;
    assert_eq!(late["events"][0]["error_code"], "source_not_found");
    assert!(app.streams.lock().unwrap().is_empty());
    owned.update(addon, json!({"enabled":true})).unwrap();
    let (_, started) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/streams",
        json!({"type":"movie","id":"tt1234567","only_addons":true}),
    )
    .await;
    let path = format!("/api/v2/streams/{}", started["id"].as_str().unwrap());
    let before = poll(&app, &path).await;
    assert_eq!(before["events"][0]["streams"].as_array().unwrap().len(), 1);
    owned.update(addon, json!({"enabled":false})).unwrap();
    let after = poll(&app, &path).await;
    assert_eq!(before["events"][0]["seq"], after["events"][0]["seq"]);
    assert_eq!(after["events"][0]["error_code"], "source_not_found");
    assert!(after["events"][0]["streams"].as_array().unwrap().is_empty());
    server.abort();
}
