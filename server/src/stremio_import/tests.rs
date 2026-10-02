use super::*;
use crate::test_support::request;
use axum::{body::Body, http::Response as HttpResponse, routing::get};
use std::sync::atomic::{AtomicUsize, Ordering};

const PATH: &str = "/api/profiles/1/imports/stremio/preview";
const DATE: &str = "2025-01-01T00:00:00.123Z";
const TIME: i64 = 1735689600;
fn movie(id: &str) -> Value {
    json!({"_id":id,"type":"movie","name":"Synthetic title","removed":false,"temp":false,
        "state":{"lastWatched":DATE,"timeOffset":12345,"duration":100000,"timesWatched":2}})
}
fn credentials() -> Value {
    json!({"email":"synthetic@example.invalid","password":"synthetic-private-password","import_library":true,"import_progress":true})
}
fn artifact_directory() -> tempfile::TempDir {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../android/qualification/artifacts/stremio-import-tests");
    std::fs::create_dir_all(&path).unwrap();
    tempfile::tempdir_in(path).unwrap()
}
struct Fixture {
    app: App,
    _directory: tempfile::TempDir,
    task: tokio::task::JoinHandle<()>,
    calls: Arc<AtomicUsize>,
    addon_response: Arc<Mutex<Value>>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn fixture(items: Vec<Value>, login: Value) -> Fixture {
    fixture_with(items, login, None).await
}
async fn fixture_with(
    items: Vec<Value>,
    login: Value,
    on_data: Option<Arc<dyn Fn() + Send + Sync>>,
) -> Fixture {
    let mut app = crate::auth_integration_tests::fixture();
    let directory = artifact_directory();
    let calls = Arc::new(AtomicUsize::new(0));
    let login_calls = calls.clone();
    let addon_response = Arc::new(Mutex::new(json!({"addons":[]})));
    let addon_data = addon_response.clone();
    let source = Router::new()
        .route(
            "/login",
            post(move |Json(body): Json<Value>| {
                let result = login.clone();
                let calls = login_calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(body["type"], "Login");
                    assert_eq!(body["email"], "synthetic@example.invalid");
                    assert_eq!(body["password"], "synthetic-private-password");
                    Json(result)
                }
            }),
        )
        .route(
            "/datastoreGet",
            post(move |Json(body): Json<Value>| {
                let items = items.clone();
                let on_data = on_data.clone();
                async move {
                    assert_eq!(body["authKey"], "synthetic-token");
                    assert_eq!(body["collection"], "libraryItem");
                    assert_eq!(body["all"], true);
                    if let Some(action) = on_data {
                        action();
                    }
                    Json(json!({"result":items}))
                }
            }),
        )
        .route(
            "/addonCollectionGet",
            post(move |Json(body): Json<Value>| {
                let result = addon_data.lock().unwrap().clone();
                async move {
                    assert_eq!(body["authKey"], "synthetic-token");
                    assert_eq!(body["update"], false);
                    assert_eq!(body["addFromURL"], json!([]));
                    Json(json!({"result":result}))
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, source).await.unwrap();
    });
    let service = Arc::get_mut(&mut app.stremio_import).unwrap();
    service.endpoint = Some(endpoint);
    service.backup_directory = Some(directory.path().join("backups"));
    Fixture {
        app,
        _directory: directory,
        task,
        calls,
        addon_response,
    }
}
fn login() -> Value {
    json!({"result":{"authKey":"synthetic-token","user":{"_id":"synthetic-source-account"}}})
}
async fn get_preview(app: &App) -> Value {
    let (status, body) = request(app, "member-token-1", "POST", PATH, credentials()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}
async fn apply_preview(app: &App, preview: &Value) -> (StatusCode, Value) {
    request(
        app,
        "member-token-1",
        "POST",
        &format!(
            "/api/profiles/1/imports/stremio/{}/apply",
            preview["preview_id"].as_str().unwrap()
        ),
        json!({"confirm":true}),
    )
    .await
}
fn counts(app: &App) -> (i64, i64, i64) {
    let db = app.db.lock().unwrap();
    let count = |table| {
        db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    };
    (
        count("favorites"),
        count("progress"),
        count("stremio_import_receipts"),
    )
}

async fn staged_preview(app: &App) -> Value {
    let mut body = credentials();
    body["inspect_addons"] = json!(true);
    let (status, response) = request(app, "member-token-1", "POST", PATH, body).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    response
}

async fn review_selection(app: &App, preview: &Value, ids: Value) -> (StatusCode, Value) {
    request(
        app,
        "member-token-1",
        "POST",
        &format!(
            "/api/profiles/1/imports/stremio/{}/review",
            preview["preview_id"].as_str().unwrap()
        ),
        json!({"selected_addons":ids}),
    )
    .await
}

#[tokio::test]
async fn repeated_review_keeps_handles_and_requires_current_apply_version() {
    let f = fixture(vec![movie("tt0000001"), movie("tt0000002")], login()).await;
    let preview = staged_preview(&f.app).await;
    assert_eq!(preview["review_revision"], 0);
    let (status, first) = review_selection(&f.app, &preview, json!([])).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["review_revision"], 1);
    let id = preview["preview_id"].as_str().unwrap();
    let review_path = format!("/api/profiles/1/imports/stremio/{id}/review");
    let apply_path = format!("/api/profiles/1/imports/stremio/{id}/apply");
    let (status, second) = request(
        &f.app,
        "member-token-1",
        "POST",
        &review_path,
        json!({"selected_addons":[],"expected_review_revision":1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["review_revision"], 2);
    let first_ids: Vec<_> = first["review_items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["item_id"].clone())
        .collect();
    let second_ids: Vec<_> = second["review_items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["item_id"].clone())
        .collect();
    assert_eq!(first_ids, second_ids);
    assert_eq!(second_ids.len(), 2);
    assert_eq!(counts(&f.app), (0, 0, 0));
    assert_eq!(
        request(
            &f.app,
            "member-token-1",
            "POST",
            &review_path,
            json!({"selected_addons":[],"expected_review_revision":1})
        )
        .await
        .1["error_code"],
        "stremio_preview_stale"
    );
    assert_eq!(
        request(
            &f.app,
            "member-token-1",
            "POST",
            &apply_path,
            json!({"confirm":true,"review_revision":1})
        )
        .await
        .1["error_code"],
        "stremio_preview_stale"
    );
    let confirmed = json!({"confirm":true,"review_revision":2,"excluded_items":[second_ids[0]]});
    let (status, result) = request(
        &f.app,
        "member-token-1",
        "POST",
        &apply_path,
        confirmed.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(counts(&f.app), (1, 1, 1));
    assert_eq!(
        request(&f.app, "member-token-1", "POST", &apply_path, confirmed)
            .await
            .1["already_completed"],
        true
    );
    assert_eq!(
        request(
            &f.app,
            "member-token-1",
            "POST",
            &apply_path,
            json!({"confirm":true,"review_revision":1,"excluded_items":[second_ids[0]]})
        )
        .await
        .1["error_code"],
        "stremio_preview_stale"
    );
}

#[tokio::test]
async fn failed_review_can_retry_from_last_published_version() {
    let f = fixture(vec![movie("tt0000001")], login()).await;
    let preview = staged_preview(&f.app).await;
    let id = preview["preview_id"].as_str().unwrap();
    let review_path = format!("/api/profiles/1/imports/stremio/{id}/review");
    let apply_path = format!("/api/profiles/1/imports/stremio/{id}/apply");
    assert_eq!(
        request(
            &f.app,
            "member-token-1",
            "POST",
            &review_path,
            json!({"selected_addons":["unknown"],"expected_review_revision":0})
        )
        .await
        .1["error_code"],
        "stremio_invalid_request"
    );
    // Exhausting verification permits fails after reservation; no plan exists yet.
    let permits = f.app.stremio_import.fetches.acquire_many(4).await.unwrap();
    assert_eq!(
        request(
            &f.app,
            "member-token-1",
            "POST",
            &review_path,
            json!({"selected_addons":[],"expected_review_revision":0})
        )
        .await
        .1["error_code"],
        "stremio_import_busy"
    );
    assert_ne!(
        request(
            &f.app,
            "member-token-1",
            "POST",
            &apply_path,
            json!({"confirm":true,"review_revision":0})
        )
        .await
        .0,
        StatusCode::OK
    );
    drop(permits);
    let (status, reviewed) = request(
        &f.app,
        "member-token-1",
        "POST",
        &review_path,
        json!({"selected_addons":[],"expected_review_revision":0}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reviewed}");
    assert_eq!(reviewed["review_revision"], 1);
    assert_eq!(counts(&f.app), (0, 0, 0));
}

#[tokio::test]
async fn failed_changed_review_preserves_published_plan_and_version() {
    let f = fixture(vec![movie("tt0000001")], login()).await;
    *f.addon_response.lock().unwrap() = json!({"addons":[
        {"transportUrl":"http://127.0.0.1/manifest.json","manifest":{"id":"unsafe","name":"Unavailable","resources":["meta"]}}
    ]});
    let preview = staged_preview(&f.app).await;
    let (status, first) = review_selection(&f.app, &preview, json!([])).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let id = preview["preview_id"].as_str().unwrap();
    let review_path = format!("/api/profiles/1/imports/stremio/{id}/review");
    let apply_path = format!("/api/profiles/1/imports/stremio/{id}/apply");
    let (status, failure) = request(
        &f.app,
        "member-token-1",
        "POST",
        &review_path,
        json!({"selected_addons":[preview["addons"][0]["item_id"]],"expected_review_revision":1}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(failure["error_code"], "stremio_addon_unavailable");
    assert_eq!(
        failure["failed_addon_items"],
        json!([preview["addons"][0]["item_id"]])
    );
    assert!(!failure.to_string().contains("127.0.0.1"));
    assert!(!failure.to_string().contains("manifest.json"));
    {
        let slots = f.app.stremio_import.pending.lock().unwrap();
        let plan = slots.get(id).unwrap().preview.as_ref().unwrap();
        assert_eq!(plan.review_revision, 1);
        assert!(plan.reviewed);
        assert!(!plan.review_in_progress);
        assert_eq!(plan.candidates.len(), 1);
    }
    assert_eq!(counts(&f.app), (0, 0, 0));
    let (status, second) = request(
        &f.app,
        "member-token-1",
        "POST",
        &review_path,
        json!({"selected_addons":[],"expected_review_revision":1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["review_revision"], 2);
    assert_eq!(
        second["review_items"][0]["item_id"],
        first["review_items"][0]["item_id"]
    );
    assert_eq!(
        request(
            &f.app,
            "member-token-1",
            "POST",
            &apply_path,
            json!({"confirm":true,"review_revision":1})
        )
        .await
        .1["error_code"],
        "stremio_preview_stale"
    );
}

#[tokio::test]
async fn removing_metadata_addon_removes_only_its_rows_and_restores_same_handle() {
    use base64::Engine;
    let mut f = fixture(vec![movie("tt0000001"), movie("opaque_movie")], login()).await;
    let vault = Arc::new(crate::secret_store::Vault::from_json(&json!({"active":"fixture","keys":{"fixture":base64::engine::general_purpose::STANDARD.encode([8u8;32])}}).to_string()).unwrap());
    f.app.secret_vault = Some(vault.clone());
    f.app.addons.vault = Some(vault);
    f.app.addons.allow_test_loopback = true;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/manifest.json", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        axum::serve(listener, Router::new()
            .route("/manifest.json", get(|| async { Json(json!({"id":"synthetic.meta","name":"Verified metadata","types":["movie"],"resources":["meta"]})) }))
            .route("/meta/movie/opaque_movie.json", get(|| async { Json(json!({"meta":{"id":"opaque_movie","type":"movie","name":"Synthetic title"}})) })))
            .await.unwrap();
    });
    *f.addon_response.lock().unwrap() = json!({"addons":[{"manifest":{"id":"synthetic.meta","name":"Verified metadata","resources":["meta"]},"transportUrl":url}]});
    let preview = staged_preview(&f.app).await;
    let addon_id = preview["addons"][0]["item_id"].clone();
    let id = preview["preview_id"].as_str().unwrap();
    let path = format!("/api/profiles/1/imports/stremio/{id}/review");
    let (status, with_addon) = review_selection(&f.app, &preview, json!([addon_id])).await;
    assert_eq!(status, StatusCode::OK, "{with_addon}");
    let rows = with_addon["review_items"].as_array().unwrap();
    assert_eq!(rows.iter().filter(|r| r["selectable"] == true).count(), 2);
    assert_eq!(with_addon["summary"]["needs_review"], 0);
    let metadata_id = rows
        .iter()
        .find(|r| r["selectable"] == true && r["item_id"] != rows[0]["item_id"])
        .unwrap()["item_id"]
        .clone();
    let (status, removed) = request(
        &f.app,
        "member-token-1",
        "POST",
        &path,
        json!({"selected_addons":[],"expected_review_revision":1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{removed}");
    assert_eq!(
        removed["review_items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["selectable"] == true)
            .count(),
        1
    );
    assert_eq!(removed["summary"]["needs_review"], 1);
    assert!(!removed["review_items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["item_id"] == metadata_id));
    let (status, restored) = request(
        &f.app,
        "member-token-1",
        "POST",
        &path,
        json!({"selected_addons":[addon_id],"expected_review_revision":2}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(
        restored["review_items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["selectable"] == true)
            .count(),
        2
    );
    assert!(restored["review_items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["item_id"] == metadata_id));
    assert_eq!(counts(&f.app), (0, 0, 0));
    upstream.abort();
}

#[tokio::test]
async fn cancelled_older_review_cannot_clear_newer_pending_generation() {
    let f = fixture(vec![movie("tt0000001")], login()).await;
    let preview = staged_preview(&f.app).await;
    let id = preview["preview_id"].as_str().unwrap().to_owned();
    {
        let mut slots = f.app.stremio_import.pending.lock().unwrap();
        let plan = slots.get_mut(&id).unwrap().preview.as_mut().unwrap();
        plan.review_generation = 2;
        plan.review_in_progress = true;
    }
    drop(ReviewReservation {
        service: f.app.stremio_import.clone(),
        id: id.clone(),
        generation: 1,
    });
    assert!(
        f.app
            .stremio_import
            .pending
            .lock()
            .unwrap()
            .get(&id)
            .unwrap()
            .preview
            .as_ref()
            .unwrap()
            .review_in_progress
    );
    drop(ReviewReservation {
        service: f.app.stremio_import.clone(),
        id: id.clone(),
        generation: 2,
    });
    assert!(
        !f.app
            .stremio_import
            .pending
            .lock()
            .unwrap()
            .get(&id)
            .unwrap()
            .preview
            .as_ref()
            .unwrap()
            .review_in_progress
    );
}

#[tokio::test]
async fn addon_only_scope_requires_a_selected_import() {
    let f = fixture(vec![movie("tt0000001")], login()).await;
    let mut body = credentials();
    body["import_library"] = json!(false);
    body["import_progress"] = json!(false);
    body["inspect_addons"] = json!(true);
    let (status, preview) = request(&f.app, "member-token-1", "POST", PATH, body).await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    let (status, reviewed) = review_selection(&f.app, &preview, json!([])).await;
    assert_eq!(status, StatusCode::OK, "{reviewed}");
    assert!(reviewed["review_items"].as_array().unwrap().is_empty());
    assert_eq!(reviewed["addons_to_add"], 0);
    let path = format!(
        "/api/profiles/1/imports/stremio/{}/apply",
        preview["preview_id"].as_str().unwrap()
    );
    let (status, _) = request(
        &f.app,
        "member-token-1",
        "POST",
        &path,
        json!({"confirm":true,"review_revision":reviewed["review_revision"]}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(counts(&f.app), (0, 0, 0));
}

#[tokio::test]
async fn named_review_exclusions_replay_and_scope() {
    let f = fixture(vec![movie("tt0000001"), movie("tt0000002")], login()).await;
    let preview = staged_preview(&f.app).await;
    assert_eq!(preview["stage"], "addons");
    assert_eq!(preview["addons"], json!([]));
    assert_eq!(counts(&f.app), (0, 0, 0));
    let id = preview["preview_id"].as_str().unwrap();
    let path = format!("/api/profiles/1/imports/stremio/{id}/apply");
    assert_eq!(
        request(
            &f.app,
            "member-token-1",
            "POST",
            &path,
            json!({"confirm":true})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        review_selection(&f.app, &preview, json!(["unknown"]))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let (status, reviewed) = review_selection(&f.app, &preview, json!([])).await;
    assert_eq!(status, StatusCode::OK, "{reviewed}");
    let rows = reviewed["review_items"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["name"], "Synthetic title");
    assert_eq!(rows[0]["counts"]["favorites_to_add"], 1);
    assert_eq!(rows[0]["counts"]["progress_to_add"], 1);
    let excluded = rows[0]["item_id"].clone();
    assert_eq!(counts(&f.app), (0, 0, 0));
    assert_eq!(
        request(
            &f.app,
            "member-token-1",
            "POST",
            &path,
            json!({"confirm":true,"review_revision":reviewed["review_revision"],"excluded_items":["unknown"]})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            &f.app,
            "member-token-2",
            "POST",
            &path,
            json!({"confirm":true,"review_revision":reviewed["review_revision"],"excluded_items":[excluded]})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &f.app,
            "member-token-1",
            "POST",
            &path,
            json!({"confirm":true,"excluded_items":[excluded]})
        )
        .await
        .1["error_code"],
        "stremio_preview_stale"
    );
    let body = json!({"confirm":true,"review_revision":reviewed["review_revision"],"excluded_items":[excluded]});
    let (status, result) = request(&f.app, "member-token-1", "POST", &path, body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["excluded_items"], 1);
    assert_eq!(result["summary"]["favorites_added"], 1);
    assert_eq!(counts(&f.app), (1, 1, 1));
    assert_eq!(
        request(&f.app, "member-token-1", "POST", &path, body)
            .await
            .1["already_completed"],
        true
    );
    assert_eq!(
        request(
            &f.app,
            "member-token-1",
            "POST",
            &path,
            json!({"confirm":true,"review_revision":reviewed["review_revision"],"excluded_items":[]})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn real_collection_shape_unsafe_and_duplicate_addons_remain_read_only() {
    let f = fixture(vec![movie("tt0000001")], login()).await;
    *f.addon_response.lock().unwrap() = json!({"addons":[
        {"transportUrl":"http://127.0.0.1/secret/manifest.json","manifest":{"id":"one","name":"Private","resources":["meta"]}},
        {"transportUrl":"https://user:pass@example.com/manifest.json","manifest":{"id":"two","name":"Credentials","resources":["stream"]}}]});
    let preview = staged_preview(&f.app).await;
    assert_eq!(preview["addons"][0]["status"], "unavailable");
    assert_eq!(preview["addons"][1]["status"], "unavailable");
    assert!(!preview.to_string().contains("secret") && !preview.to_string().contains("user:pass"));
    assert_eq!(
        review_selection(&f.app, &preview, json!([preview["addons"][0]["item_id"]]))
            .await
            .0,
        StatusCode::BAD_GATEWAY
    );
    assert_eq!(counts(&f.app), (0, 0, 0));
}
#[tokio::test]
async fn selected_addon_applies_encrypted_only_after_confirmation_and_addon_only() {
    use base64::Engine;
    let mut f = fixture(vec![movie("tt0000001")], login()).await;
    let vault = Arc::new(crate::secret_store::Vault::from_json(&json!({"active":"fixture","keys":{"fixture":base64::engine::general_purpose::STANDARD.encode([7u8;32])}}).to_string()).unwrap());
    f.app.secret_vault = Some(vault.clone());
    f.app.addons.vault = Some(vault);
    f.app.addons.allow_test_loopback = true;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/secret-token/manifest.json",
        listener.local_addr().unwrap()
    );
    let upstream = tokio::spawn(async move {
        axum::serve(listener, Router::new().route("/secret-token/manifest.json",get(||async {
        Json(json!({"id":"synthetic.meta","name":"Verified addon","types":["movie","series"],"resources":["meta"]}))
    }))).await.unwrap();
    });
    *f.addon_response.lock().unwrap() = json!({"addons":[{"manifest":{"id":"synthetic.meta","name":"Verified addon","resources":["meta"]},"transportUrl":url}]});
    let preview = staged_preview(&f.app).await;
    assert_eq!(preview["addons"][0]["status"], "add", "{preview}");
    assert!(!preview.to_string().contains("secret-token"));
    let (status, review) =
        review_selection(&f.app, &preview, json!([preview["addons"][0]["item_id"]])).await;
    assert_eq!(status, StatusCode::OK, "{review}");
    assert_eq!(review["addons_to_add"], 1);
    assert!(!review.to_string().contains("secret-token"));
    assert_eq!(counts(&f.app), (0, 0, 0));
    let rows = review["review_items"].as_array().unwrap();
    let path = format!(
        "/api/profiles/1/imports/stremio/{}/apply",
        preview["preview_id"].as_str().unwrap()
    );
    let (status, result) = request(
        &f.app,
        "member-token-1",
        "POST",
        &path,
        json!({"confirm":true,"review_revision":review["review_revision"],"excluded_items":[rows[0]["item_id"]]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["addons_added"], 1);
    assert_eq!(counts(&f.app), (0, 0, 0));
    let db = f.app.db.lock().unwrap();
    let secret: String = db
        .query_row(
            "SELECT secret FROM addon_credentials_v2 WHERE account_id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!secret.contains("secret-token"));
    assert!(db
        .query_row(
            "SELECT manifest_url FROM addons WHERE account_id=1",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap()
        .starts_with("sealed:addon:"));
    upstream.abort();
}

#[test]
fn metadata_matches_only_original_identity_and_exact_episode_without_guessing_dates() {
    let mut item = movie("opaque:series");
    item["type"] = json!("series");
    item["state"]["video_id"] = json!("opaque:episode:4");
    let mut evidence = HashMap::new();
    evidence.insert(("series".to_owned(),"opaque:series".to_owned()),
        json!({"id":"opaque:series","type":"series","videos":[{"id":"opaque:episode:4","season":2,"episode":4}]}));
    let (mapped, _) = mapper::map_verified(&[item.clone()], true, true, util::now(), &evidence);
    assert_eq!(mapped.len(), 2);
    assert_eq!(mapped[0].id, "opaque:series");
    assert_eq!(mapped[1].id, "opaque:episode:4");
    assert_eq!(
        mapped[1].progress.as_ref().unwrap().context,
        json!({"series_id":"opaque:series","season":2,"episode":4})
    );
    evidence
        .get_mut(&("series".into(), "opaque:series".into()))
        .unwrap()["videos"] = json!([]);
    let (mapped, summary) =
        mapper::map_verified(&[item.clone()], true, true, util::now(), &evidence);
    assert_eq!(mapped.len(), 1);
    assert!(mapped[0].progress.is_none());
    assert_eq!(summary.needs_review, 1);
    item["state"]["lastWatched"] = Value::Null;
    let (mapped, summary) = mapper::map_verified(&[item], true, true, util::now(), &evidence);
    assert_eq!(mapped.len(), 1);
    assert!(mapped[0].progress.is_none());
    assert_eq!(summary.needs_review, 1);
}

#[tokio::test]
async fn undated_series_watch_flags_are_review_only() {
    let mut item = movie("tt0000001");
    item["type"] = json!("series");
    item["state"]["watched"] = json!("synthetic-bitfield");
    item["state"]["timeOffset"] = json!(0);
    item["state"]["timesWatched"] = json!(0);
    let f = fixture(vec![item], login()).await;
    let preview = staged_preview(&f.app).await;
    let (_, review) = review_selection(&f.app, &preview, json!([])).await;
    let row = review["review_items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["reason"] == "watch_date_unknown")
        .unwrap();
    assert_eq!(row["selectable"], false);
    assert_eq!(row["status"], "needs_review");
    assert_eq!(counts(&f.app), (0, 0, 0));
}

#[test]
fn mapper_seconds_rewatch_exact_episode_and_movie_completion() {
    let mut series = movie("tt1234567");
    series["type"] = json!("series");
    series["state"]["video_id"] = json!("tt1234567:2:3");
    series["state"]["watched"] = json!("bulk-bitfield-deferred");
    let mut complete = movie("tt7654321");
    complete["state"]["timeOffset"] = json!(0);
    complete["state"]["duration"] = json!(0);
    let (candidates, summary) = mapper::map(
        &[movie("tt0000001"), series, complete],
        true,
        true,
        util::now(),
    );
    assert_eq!(candidates.len(), 4);
    let resume = candidates[0].progress.as_ref().unwrap();
    assert_eq!(resume.position, 12.345);
    assert_eq!(resume.duration, 100.0);
    assert_eq!(resume.timestamp, TIME);
    assert!(resume.context.get("watched_override").is_none());
    assert_eq!(candidates[1].id, "tt1234567");
    assert!(candidates[1].favorite);
    assert_eq!(candidates[2].id, "tt1234567:2:3");
    assert_eq!(candidates[2].title, "tt1234567");
    assert_eq!(
        candidates[2].progress.as_ref().unwrap().context,
        json!({"series_id":"tt1234567","imdb_id":"tt1234567","season":2,"episode":3})
    );
    let watched = candidates[3].progress.as_ref().unwrap();
    assert_eq!(watched.duration, 0.0);
    assert_eq!(watched.position, 0.0);
    assert_eq!(watched.context["watched_override"], true);
    assert_eq!(watched.context["stremio_import_watched"], true);
    assert_eq!(summary.needs_review, 1);
}
#[test]
fn mapper_rejects_missing_future_invalid_dates_offsets_and_episode_ids() {
    for date in [
        Value::Null,
        json!("invalid"),
        json!("2099-01-01T00:00:00Z"),
        json!("1960-01-01T00:00:00Z"),
        json!(TIME),
    ] {
        let mut item = movie("tt0000001");
        item["state"]["lastWatched"] = date;
        let (c, s) = mapper::map(&[item], true, true, util::now());
        assert!(c[0].progress.is_none());
        assert_eq!(s.needs_review, 1);
    }
    for offset in [json!(-1), json!(100001), json!(1.5), json!("1000")] {
        let mut item = movie("tt0000001");
        item["state"]["timeOffset"] = offset;
        let (c, _) = mapper::map(&[item], true, true, util::now());
        assert!(c[0].progress.is_none());
    }
    for video in [
        "tt9999999:1:1",
        "tt0000001",
        "tt0000001:1:no",
        "tt0000001:100001:1",
        "opaque:episode",
    ] {
        let mut item = movie("tt0000001");
        item["type"] = json!("series");
        item["state"]["video_id"] = json!(video);
        let (c, s) = mapper::map(&[item], true, true, util::now());
        assert!(c[0].progress.is_none());
        assert_eq!(s.needs_review, 1);
    }
}
#[test]
fn mapper_cleared_removed_temp_unsupported_duplicates_and_options() {
    let mut cleared = movie("tt0000001");
    cleared["removed"] = json!(true);
    cleared["state"]["timeOffset"] = json!(0);
    cleared["state"]["timesWatched"] = json!(0);
    let mut retained = movie("tt0000002");
    retained["removed"] = json!(true);
    let mut temp = movie("tt0000003");
    temp["temp"] = json!(true);
    let unsupported = movie("kitsu:42");
    let (c, s) = mapper::map(
        &[
            cleared,
            retained,
            temp,
            unsupported,
            movie("tt0000004"),
            movie("tt0000004"),
        ],
        true,
        true,
        util::now(),
    );
    assert_eq!(c.len(), 2);
    assert!(!c[0].favorite);
    assert!(!c[1].favorite);
    assert_eq!(s.needs_review, 3);
    assert_eq!(s.skipped_items, 1);
    let (c, _) = mapper::map(&[movie("tt0000001")], false, true, util::now());
    assert!(!c[0].favorite);
    let (c, _) = mapper::map(&[movie("tt0000001")], true, false, util::now());
    assert!(c[0].progress.is_none());
}

#[tokio::test]
async fn preview_is_read_only_confirmation_backup_apply_replay_and_removed_favorite() {
    let f = fixture(vec![movie("tt0000001")], login()).await;
    let p = get_preview(&f.app).await;
    assert_eq!(counts(&f.app), (0, 0, 0));
    assert_eq!(p["summary"]["favorites_to_add"], 1);
    assert_eq!(p["summary"]["progress_to_add"], 1);
    assert!(p["preview_id"].as_str().unwrap().len() >= 64);
    assert!(!f._directory.path().join("backups").exists());
    let path = format!(
        "/api/profiles/1/imports/stremio/{}/apply",
        p["preview_id"].as_str().unwrap()
    );
    for body in [
        json!({"confirm":false}),
        json!({}),
        json!({"confirm":true,"email":"private"}),
    ] {
        assert_eq!(
            request(&f.app, "member-token-1", "POST", &path, body)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(counts(&f.app), (0, 0, 0));
    }
    let (status, result) = apply_preview(&f.app, &p).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["summary"]["favorites_added"], 1);
    assert_eq!(result["summary"]["progress_added"], 1);
    assert_eq!(counts(&f.app), (1, 1, 1));
    let backup_path = std::fs::read_dir(f._directory.path().join("backups"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let backup = Connection::open(&backup_path).unwrap();
    assert_eq!(
        backup
            .query_row("SELECT COUNT(*) FROM favorites", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        backup
            .query_row("SELECT COUNT(*) FROM stremio_import_receipts", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(backup);
    assert_eq!(apply_preview(&f.app, &p).await.1["already_completed"], true);
    assert_eq!(
        std::fs::read_dir(f._directory.path().join("backups"))
            .unwrap()
            .count(),
        1
    );
    f.app
        .db
        .lock()
        .unwrap()
        .execute("DELETE FROM favorites WHERE profile_id=1", [])
        .unwrap();
    let p2 = get_preview(&f.app).await;
    assert_eq!(p2["summary"]["favorites_to_add"], 0);
    assert_eq!(p2["summary"]["already_imported"], 2);
    assert_eq!(apply_preview(&f.app, &p2).await.0, StatusCode::OK);
    assert_eq!(counts(&f.app), (0, 1, 1));
}
#[tokio::test]
async fn merge_preserves_newer_equal_manual_source_context_and_queue_hiding() {
    let items = (1..=5).map(|n| movie(&format!("tt000000{n}"))).collect();
    let f = fixture(items, login()).await;
    {
        let db = f.app.db.lock().unwrap();
        for n in 1..=4 {
            let time = if n == 1 {
                TIME + 1
            } else if n == 2 {
                TIME
            } else {
                TIME - 1
            };
            let context = if n == 3 {
                json!({"progress_corrected":true,"source_name":"local","watched_override":false})
            } else {
                json!({"source_name":"local","source_fingerprint":"original","audio_language":"en"})
            };
            db.execute("INSERT INTO progress(profile_id,type,id,name,poster,position,duration,updated_at,context,title_id) VALUES(1,'movie',?1,'Local name','local-poster',7,77,?2,?3,?1)",params![format!("tt000000{n}"),time,context.to_string()]).unwrap();
        }
        db.execute("INSERT INTO queue_hidden VALUES(1,'movie','tt0000004')", [])
            .unwrap();
    }
    let p = get_preview(&f.app).await;
    assert_eq!(p["summary"]["existing_preserved"], 3);
    assert_eq!(p["summary"]["progress_to_update"], 1);
    let (status, r) = apply_preview(&f.app, &p).await;
    assert_eq!(status, StatusCode::OK, "{r}");
    assert_eq!(r["summary"]["progress_updated"], 1);
    assert_eq!(r["summary"]["progress_added"], 1);
    let db = f.app.db.lock().unwrap();
    for n in 1..=3 {
        assert_eq!(
            db.query_row(
                "SELECT position FROM progress WHERE id=?1",
                [format!("tt000000{n}")],
                |r| r.get::<_, f64>(0)
            )
            .unwrap(),
            7.0
        );
    }
    let row: (String, String, String, f64, i64) = db
        .query_row(
            "SELECT name,poster,context,position,updated_at FROM progress WHERE id='tt0000004'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(row.0, "Local name");
    assert_eq!(row.1, "local-poster");
    assert_eq!(
        serde_json::from_str::<Value>(&row.2).unwrap()["source_fingerprint"],
        "original"
    );
    assert_eq!(row.3, 12.345);
    assert_eq!(row.4, TIME);
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM queue_hidden", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}
#[tokio::test]
async fn stale_snapshot_cancel_expiry_and_session_binding() {
    let f = fixture(vec![movie("tt0000001")], login()).await;
    let p = get_preview(&f.app).await;
    f.app
        .db
        .lock()
        .unwrap()
        .execute("INSERT INTO queue_hidden VALUES(1,'movie','tt0000001')", [])
        .unwrap();
    let (s, r) = apply_preview(&f.app, &p).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(r["error_code"], "stremio_preview_stale");
    assert_eq!(counts(&f.app), (0, 0, 0));
    let id = p["preview_id"].as_str().unwrap();
    let path = format!("/api/profiles/1/imports/stremio/{id}");
    assert_eq!(
        request(&f.app, "member-token-1", "DELETE", &path, json!({}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        apply_preview(&f.app, &p).await.1["error_code"],
        "stremio_preview_not_found"
    );
    let p = get_preview(&f.app).await;
    f.app
        .stremio_import
        .pending
        .lock()
        .unwrap()
        .get_mut(p["preview_id"].as_str().unwrap())
        .unwrap()
        .expires = util::now() - 1;
    assert_eq!(
        apply_preview(&f.app, &p).await.1["error_code"],
        "stremio_preview_expired"
    );
    let p = get_preview(&f.app).await;
    {
        let db = f.app.db.lock().unwrap();
        db.execute("INSERT INTO auth_sessions(id,account_id,profile_id,access_hash,refresh_hash,csrf_hash,kind,device_name,access_expires,refresh_expires,created_at) VALUES('s-other',1,1,?1,'refresh-other','unused','browser','test',?2,?2,0)",params![auth::hash("other-token"),util::now()+3600]).unwrap();
    }
    let path = format!(
        "/api/profiles/1/imports/stremio/{}/apply",
        p["preview_id"].as_str().unwrap()
    );
    assert_eq!(
        request(
            &f.app,
            "other-token",
            "POST",
            &path,
            json!({"confirm":true})
        )
        .await
        .1["error_code"],
        "stremio_preview_not_found"
    );
    assert_eq!(counts(&f.app), (0, 0, 0));
}
#[tokio::test]
async fn authorization_unauthorized_foreign_replaced_device_restricted_and_parent() {
    let f = fixture(vec![movie("tt0000001")], login()).await;
    assert_eq!(
        request(&f.app, "invalid-token", "POST", PATH, credentials())
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert!(
        request(&f.app, "member-token-2", "POST", PATH, credentials())
            .await
            .0
            .is_client_error()
    );
    assert_eq!(
        request(
            &f.app,
            "member-token-1",
            "POST",
            "/api/profiles/2/imports/stremio/preview",
            credentials()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    let p = get_preview(&f.app).await;
    f.app
        .db
        .lock()
        .unwrap()
        .execute("UPDATE auth_sessions SET profile_id=NULL WHERE id='s1'", [])
        .unwrap();
    assert_eq!(apply_preview(&f.app, &p).await.0, StatusCode::FORBIDDEN);
    f.app
        .db
        .lock()
        .unwrap()
        .execute(
            "UPDATE auth_sessions SET profile_id=1,kind='device' WHERE id='s1'",
            [],
        )
        .unwrap();
    assert_eq!(
        request(&f.app, "member-token-1", "POST", PATH, credentials())
            .await
            .1["error_code"],
        "account_session_required"
    );
    {
        let db = f.app.db.lock().unwrap();
        db.execute("UPDATE auth_sessions SET kind='browser' WHERE id='s1'", [])
            .unwrap();
        db.execute(
            "INSERT INTO kids_profiles(profile_id,enabled) VALUES(1,1)",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO parent_grants VALUES('s1',?1)",
            [util::now() + 3600],
        )
        .unwrap();
    }
    assert_eq!(
        request(&f.app, "member-token-1", "POST", PATH, credentials())
            .await
            .1["error_code"],
        "stremio_restricted_profile"
    );
    assert_eq!(
        apply_preview(&f.app, &p).await.1["error_code"],
        "stremio_restricted_profile"
    );
    assert_eq!(counts(&f.app), (0, 0, 0));
}
#[tokio::test]
async fn credentials_and_parser_errors_do_not_echo_secrets_and_failed_reservations_are_disposed() {
    let f=fixture(vec![],json!({"error":{"message":"synthetic-private-password synthetic@example.invalid https://private.invalid"}})).await;
    for _ in 0..3 {
        let (s, r) = request(&f.app, "member-token-1", "POST", PATH, credentials()).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(r["error_code"], "stremio_credentials_invalid");
        assert!(!r.to_string().contains("synthetic"));
        assert!(!r.to_string().contains("https://"));
        assert!(f.app.stremio_import.pending.lock().unwrap().is_empty());
    }
    let mut body = credentials();
    body["import_library"] = json!("synthetic-private-password");
    let (_, r) = request(&f.app, "member-token-1", "POST", PATH, body).await;
    assert_eq!(r["error_code"], "stremio_invalid_request");
    assert!(!r.to_string().contains("synthetic"));
    assert_eq!(counts(&f.app), (0, 0, 0));
}
#[tokio::test]
async fn backup_and_transaction_failures_abort_every_write_and_receipt() {
    let mut f = fixture(vec![movie("tt0000001")], login()).await;
    let p = get_preview(&f.app).await;
    let bad = f._directory.path().join("not-a-directory");
    std::fs::write(&bad, b"fixture").unwrap();
    Arc::get_mut(&mut f.app.stremio_import)
        .unwrap()
        .backup_directory = Some(bad);
    let (s, r) = apply_preview(&f.app, &p).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(r["error_code"], "stremio_backup_failed");
    assert_eq!(counts(&f.app), (0, 0, 0));
    Arc::get_mut(&mut f.app.stremio_import)
        .unwrap()
        .backup_directory = Some(f._directory.path().join("backups"));
    f.app.db.lock().unwrap().execute_batch("CREATE TRIGGER fail_import_receipt BEFORE INSERT ON stremio_import_receipts BEGIN SELECT RAISE(ABORT,'synthetic private source'); END;").unwrap();
    let (s, r) = apply_preview(&f.app, &p).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(r["error_code"], STORAGE);
    assert!(!r.to_string().contains("synthetic"));
    assert_eq!(counts(&f.app), (0, 0, 0));
    f.app
        .db
        .lock()
        .unwrap()
        .execute_batch("DROP TRIGGER fail_import_receipt")
        .unwrap();
    assert_eq!(apply_preview(&f.app, &p).await.0, StatusCode::OK);
}
#[tokio::test]
async fn preview_capacity_is_bounded_and_cancel_reclaims_slot() {
    let f = fixture(vec![], login()).await;
    let p = get_preview(&f.app).await;
    get_preview(&f.app).await;
    assert_eq!(
        request(&f.app, "member-token-1", "POST", PATH, credentials())
            .await
            .1["error_code"],
        "stremio_import_busy"
    );
    assert_eq!(f.calls.load(Ordering::SeqCst), 2);
    let path = format!(
        "/api/profiles/1/imports/stremio/{}",
        p["preview_id"].as_str().unwrap()
    );
    assert_eq!(
        request(&f.app, "member-token-1", "DELETE", &path, json!({}))
            .await
            .0,
        StatusCode::OK
    );
    get_preview(&f.app).await;
}
#[tokio::test]
async fn network_reauthorization_rejects_profile_replacement_before_publishing_preview() {
    let app = crate::auth_integration_tests::fixture();
    let db = app.db.clone();
    let action: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        db.lock()
            .unwrap()
            .execute("UPDATE auth_sessions SET profile_id=NULL WHERE id='s1'", [])
            .unwrap();
    });
    let mut f = fixture_with(vec![movie("tt0000001")], login(), Some(action)).await;
    // Use the same DB the synthetic source callback changes, retaining the isolated service.
    f.app.db = app.db;
    let (s, _) = request(&f.app, "member-token-1", "POST", PATH, credentials()).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(counts(&f.app), (0, 0, 0));
    assert!(f.app.stremio_import.pending.lock().unwrap().is_empty());
}
#[tokio::test]
async fn source_redirect_oversize_invalid_and_item_limits_fail_closed() {
    for mode in ["redirect", "oversize", "invalid", "items"] {
        let mut app = crate::auth_integration_tests::fixture();
        let directory = artifact_directory();
        let source = Router::new()
            .route(
                "/login",
                post(move || async move {
                    match mode {
                        "redirect" => HttpResponse::builder()
                            .status(302)
                            .header("location", "http://127.0.0.1:1/never")
                            .body(Body::empty())
                            .unwrap(),
                        "oversize" => HttpResponse::builder()
                            .header("content-length", "8000001")
                            .body(Body::from(vec![b'x'; 8_000_001]))
                            .unwrap(),
                        "invalid" => HttpResponse::builder()
                            .body(Body::from("not-json synthetic-private-password"))
                            .unwrap(),
                        _ => HttpResponse::builder()
                            .header("content-type", "application/json")
                            .body(Body::from(login().to_string()))
                            .unwrap(),
                    }
                }),
            )
            .route(
                "/datastoreGet",
                post(|| async { Json(json!({"result":vec![Value::Null;MAX_ITEMS+1]})) }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        Arc::get_mut(&mut app.stremio_import).unwrap().endpoint =
            Some(format!("http://{}", listener.local_addr().unwrap()));
        let task = tokio::spawn(async move {
            axum::serve(listener, source).await.unwrap();
        });
        let (s, r) = request(&app, "member-token-1", "POST", PATH, credentials()).await;
        assert_eq!(s, StatusCode::BAD_GATEWAY);
        assert_eq!(r["error_code"], UNAVAILABLE);
        assert!(!r.to_string().contains("synthetic"));
        assert_eq!(counts(&app), (0, 0, 0));
        task.abort();
        drop(directory);
    }
}

#[tokio::test]
async fn episode_identity_survives_http_apply_and_movie_watched_keeps_known_local_runtime() {
    let mut series = movie("tt1234567");
    series["type"] = json!("series");
    series["state"]["video_id"] = json!("tt1234567:2:3");
    let mut completed = movie("tt7654321");
    completed["state"]["timeOffset"] = json!(0);
    completed["state"]["duration"] = json!(0);
    let f = fixture(vec![series, completed], login()).await;
    f.app.db.lock().unwrap().execute("INSERT INTO progress(profile_id,type,id,name,poster,position,duration,updated_at,context,title_id) VALUES(1,'movie','tt7654321','Local',NULL,7,77,?1,'{\"source_name\":\"local\"}','tt7654321')",[TIME-1]).unwrap();
    let p = get_preview(&f.app).await;
    assert_eq!(p["summary"]["favorites_to_add"], 2);
    let (status, result) = apply_preview(&f.app, &p).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let db = f.app.db.lock().unwrap();
    let (title,context,position,duration,time):(String,String,f64,f64,i64)=db.query_row("SELECT title_id,context,position,duration,updated_at FROM progress WHERE id='tt1234567:2:3'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).unwrap();
    assert_eq!(title, "tt1234567");
    assert_eq!(
        serde_json::from_str::<Value>(&context).unwrap(),
        json!({"series_id":"tt1234567","imdb_id":"tt1234567","season":2,"episode":3})
    );
    assert_eq!((position, duration, time), (12.345, 100.0, TIME));
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM progress WHERE id='tt1234567'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    let (position, duration, context): (f64, f64, String) = db
        .query_row(
            "SELECT position,duration,context FROM progress WHERE id='tt7654321'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!((position, duration), (77.0, 77.0));
    let context: Value = serde_json::from_str(&context).unwrap();
    assert_eq!(context["watched_override"], true);
    assert_eq!(context["source_name"], "local");
}

#[tokio::test]
async fn fetch_concurrency_and_global_preview_capacity_are_bounded() {
    let f = fixture(vec![], login()).await;
    let permits = f.app.stremio_import.fetches.acquire_many(4).await.unwrap();
    let (status, body) = request(&f.app, "member-token-1", "POST", PATH, credentials()).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error_code"], "stremio_import_busy");
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    assert!(f.app.stremio_import.pending.lock().unwrap().is_empty());
    drop(permits);
    {
        let mut slots = f.app.stremio_import.pending.lock().unwrap();
        for account in 100..164 {
            slots.insert(
                format!("fixture-{account}"),
                Slot {
                    account,
                    scope: "synthetic".into(),
                    session: None,
                    revision: 0,
                    profile: 1,
                    expires: util::now() + TTL,
                    preview: None,
                    completed: None,
                    completed_exclusions: None,
                    completed_addons: 0,
                    completed_existing: 0,
                    completed_review_revision: None,
                },
            );
        }
    }
    assert_eq!(
        request(&f.app, "member-token-1", "POST", PATH, credentials())
            .await
            .1["error_code"],
        "stremio_import_busy"
    );
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn upstream_401_is_422_not_viptv_session_expiry() {
    let mut app = crate::auth_integration_tests::fixture();
    let source = Router::new().route(
        "/login",
        post(|| async { (StatusCode::UNAUTHORIZED, "synthetic-private-password") }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    Arc::get_mut(&mut app.stremio_import).unwrap().endpoint =
        Some(format!("http://{}", listener.local_addr().unwrap()));
    let task = tokio::spawn(async move {
        axum::serve(listener, source).await.unwrap();
    });
    let (status, body) = request(&app, "member-token-1", "POST", PATH, credentials()).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error_code"], "stremio_credentials_invalid");
    assert!(!body.to_string().contains("synthetic"));
    assert_eq!(
        request(&app, "member-token-1", "GET", "/api/profiles", json!({}))
            .await
            .0,
        StatusCode::OK
    );
    task.abort();
}

#[tokio::test]
async fn real_file_wal_backup_uses_private_directory_and_contains_no_import_credentials_or_preview()
{
    let mut f = fixture(vec![movie("tt0000001")], login()).await;
    let database = f._directory.path().join("fixture.sqlite");
    let mut disk = Connection::open(&database).unwrap();
    {
        let db = f.app.db.lock().unwrap();
        let backup = rusqlite::backup::Backup::new(&db, &mut disk).unwrap();
        backup
            .run_to_completion(256, Duration::from_millis(1), None)
            .unwrap();
    }
    disk.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; PRAGMA foreign_keys=ON; INSERT INTO favorites VALUES(1,'tt9999999','movie','Existing WAL title',NULL)").unwrap();
    f.app.db = Arc::new(Mutex::new(disk));
    Arc::get_mut(&mut f.app.stremio_import)
        .unwrap()
        .backup_directory = None;
    let p = get_preview(&f.app).await;
    assert_eq!(apply_preview(&f.app, &p).await.0, StatusCode::OK);
    let directory = f._directory.path().join("stremio-import-backups");
    let file = std::fs::read_dir(&directory)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let saved = Connection::open(&file).unwrap();
    assert_eq!(
        saved
            .query_row(
                "SELECT COUNT(*) FROM favorites WHERE id='tt9999999'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        saved
            .query_row(
                "SELECT COUNT(*) FROM favorites WHERE id='tt0000001'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        saved
            .query_row("SELECT COUNT(*) FROM stremio_import_receipts", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(saved);
    let bytes = std::fs::read(&file).unwrap();
    for private in [
        "synthetic-private-password",
        "synthetic@example.invalid",
        "synthetic-token",
        "synthetic-source-account",
        p["preview_id"].as_str().unwrap(),
    ] {
        assert!(!bytes
            .windows(private.len())
            .any(|part| part == private.as_bytes()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn imported_completion_is_replaced_by_newer_rewatch_resume_and_replay_is_idempotent() {
    let mut completion = movie("tt0000001");
    completion["state"]["timeOffset"] = json!(0);
    let first = fixture(vec![completion], login()).await;
    first.app.db.lock().unwrap().execute("INSERT INTO progress(profile_id,type,id,name,poster,position,duration,updated_at,context,title_id) VALUES(1,'movie','tt0000001','Local title',NULL,7,100,?1,'{\"source_name\":\"local\",\"source_fingerprint\":\"original\"}','tt0000001')",[TIME-1]).unwrap();
    let preview = get_preview(&first.app).await;
    assert_eq!(apply_preview(&first.app, &preview).await.0, StatusCode::OK);
    let (status, history) = request(
        &first.app,
        "member-token-1",
        "GET",
        "/api/profiles/1/progress/page",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(history["items"][0]["watched"], true);
    let (_, queue) = request(
        &first.app,
        "member-token-1",
        "GET",
        "/api/profiles/1/continue/page",
        Value::Null,
    )
    .await;
    assert!(queue["items"].as_array().unwrap().is_empty());

    let mut resume = movie("tt0000001");
    resume["state"]["lastWatched"] = json!("2025-01-01T01:00:00.123Z");
    // The nonzero historical watched counter must not suppress this rewatch position.
    let mut second = fixture(vec![resume], login()).await;
    second.app.db = first.app.db.clone();
    let preview = get_preview(&second.app).await;
    let (status, result) = apply_preview(&second.app, &preview).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let row: (f64, f64, i64, String) = second
        .app
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT position,duration,updated_at,context FROM progress WHERE id='tt0000001'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!((row.0, row.1, row.2), (12.345, 100.0, TIME + 3600));
    assert_eq!(preview["summary"]["progress_to_update"], 1);
    assert_eq!(result["summary"]["progress_updated"], 1);
    let context: Value = serde_json::from_str(&row.3).unwrap();
    assert!(context.get("watched_override").is_none());
    assert!(context.get("stremio_import_watched").is_none());
    assert_eq!(context["source_name"], "local");
    assert_eq!(context["source_fingerprint"], "original");
    let (_, history) = request(
        &second.app,
        "member-token-1",
        "GET",
        "/api/profiles/1/progress/page",
        Value::Null,
    )
    .await;
    assert_eq!(history["items"][0]["watched"], false);
    let (_, queue) = request(
        &second.app,
        "member-token-1",
        "GET",
        "/api/profiles/1/continue/page",
        Value::Null,
    )
    .await;
    assert_eq!(queue["items"].as_array().unwrap().len(), 1);
    assert_eq!(queue["items"][0]["id"], "tt0000001");
    assert_eq!(queue["items"][0]["position"], 12.345);
    assert_eq!(queue["items"][0]["watched"], false);
    assert_eq!(
        apply_preview(&second.app, &preview).await.1["already_completed"],
        true
    );
    let repeated = get_preview(&second.app).await;
    assert_eq!(repeated["summary"]["progress_to_update"], 0);
    assert_eq!(repeated["summary"]["already_imported"], 2);
    assert_eq!(
        apply_preview(&second.app, &repeated).await.0,
        StatusCode::OK
    );
    assert_eq!(counts(&second.app), (1, 1, 1));
    let repeated_row: (f64, f64, i64, String) = second
        .app
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT position,duration,updated_at,context FROM progress WHERE id='tt0000001'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(repeated_row, row);
}

#[tokio::test]
async fn genuine_manual_watched_correction_is_preserved_even_with_import_owned_assertion() {
    let mut items = Vec::new();
    for id in ["tt0000001", "tt0000002"] {
        let mut resume = movie(id);
        resume["state"]["lastWatched"] = json!("2025-01-01T01:00:00.123Z");
        items.push(resume);
    }
    let f = fixture(items, login()).await;
    for (id, previously_imported) in [("tt0000001", false), ("tt0000002", true)] {
        let mut context = json!({"source_name":"local","source_fingerprint":"original"});
        if previously_imported {
            context["watched_override"] = json!(true);
            context["stremio_import_watched"] = json!(true);
        }
        f.app.db.lock().unwrap().execute("INSERT INTO progress(profile_id,type,id,name,position,duration,updated_at,context,title_id) VALUES(1,'movie',?1,'Local',7,100,?2,?3,?1)",params![id,TIME-1,context.to_string()]).unwrap();
        assert_eq!(
            request(
                &f.app,
                "member-token-1",
                "PUT",
                "/api/profiles/1/progress/correct",
                json!({"id":id,"type":"movie","name":"Local","action":"watched"})
            )
            .await
            .0,
            StatusCode::OK
        );
        // Historical synthetic correction is older than the source resume. Its manual
        // authority, rather than recency, must protect it even if an import marker remains.
        f.app
            .db
            .lock()
            .unwrap()
            .execute(
                "UPDATE progress SET updated_at=?1 WHERE id=?2",
                params![TIME - 1, id],
            )
            .unwrap();
    }
    let preview = get_preview(&f.app).await;
    assert_eq!(preview["summary"]["existing_preserved"], 2);
    assert_eq!(preview["summary"]["progress_to_update"], 0);
    let (status, result) = apply_preview(&f.app, &preview).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["summary"]["existing_preserved"], 2);
    for id in ["tt0000001", "tt0000002"] {
        let row: (f64, i64, String) = f
            .app
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT position,updated_at,context FROM progress WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((row.0, row.1), (100.0, TIME - 1));
        let context: Value = serde_json::from_str(&row.2).unwrap();
        assert_eq!(context["progress_corrected"], true);
        assert_eq!(context["watched_override"], true);
        assert_eq!(context["source_fingerprint"], "original");
    }
    let (_, history) = request(
        &f.app,
        "member-token-1",
        "GET",
        "/api/profiles/1/progress/page",
        Value::Null,
    )
    .await;
    assert!(history["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["watched"] == true));
    let (_, queue) = request(
        &f.app,
        "member-token-1",
        "GET",
        "/api/profiles/1/continue/page",
        Value::Null,
    )
    .await;
    assert!(queue["items"].as_array().unwrap().is_empty());
}
