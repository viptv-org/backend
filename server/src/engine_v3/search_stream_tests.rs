use crate::{auth_integration_tests::fixture, test_support::request, *};
use axum::body::BodyDataStream;
use futures::StreamExt;
use tower::ServiceExt;

async fn stack() -> (
    App,
    tokio::task::JoinHandle<()>,
    Arc<Notify>,
    Arc<tokio::sync::Semaphore>,
) {
    let started = Arc::new(Notify::new());
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let routes = Router::new().route(
        "/addon/:owner/catalog/:kind/:catalog/:extras",
        get({
            let started = started.clone();
            let gate = gate.clone();
            move |Path((_, kind, catalog, _)): Path<(String, String, String, String)>| {
                let started = started.clone();
                let gate = gate.clone();
                async move {
                    match catalog.as_str() {
                        "broken" => (
                            StatusCode::BAD_GATEWAY,
                            axum::Json(json!({"error":"private provider reflection"})),
                        ),
                        "slow" => {
                            started.notify_one();
                            let _permit = gate.acquire().await.unwrap();
                            (
                                StatusCode::OK,
                                axum::Json(json!({"metas":[{"id":"tt302","type":kind,"name":"Slow fixture"}]})),
                            )
                        }
                        _ => (
                            StatusCode::OK,
                            axum::Json(json!({"metas":[{"id":"tt301","type":kind,"name":"Fast fixture"}]})),
                        ),
                    }
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    let mut app = fixture();
    app.addons.allow_test_loopback = true;
    for (id, name, catalog) in [
        (201, "Fast 201", "fast"),
        (202, "Slow 202", "slow"),
        (203, "Broken 203", "broken"),
    ] {
        let manifest = json!({"id":format!("fixture{id}"),"name":name,"resources":["catalog"],"types":["movie"],
            "catalogs":[{"id":catalog,"type":"movie","extra":[{"name":"search","isRequired":true}]}]});
        app.db
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO addons(id,name,manifest_url,manifest,account_id,enabled) VALUES(?1,?2,?3,?4,1,1)",
                params![
                    id,
                    name,
                    format!("http://{address}/addon/{id}/manifest.json"),
                    manifest.to_string()
                ],
            )
            .unwrap();
    }
    crate::test_support::encrypt_fixture_sources(&app);
    (app, task, started, gate)
}

struct Frames {
    body: std::pin::Pin<Box<BodyDataStream>>,
    buffer: String,
}
impl Frames {
    async fn next(&mut self) -> (String, Value) {
        loop {
            if let Some(end) = self.buffer.find("\n\n") {
                let frame = self.buffer[..end].to_string();
                self.buffer.drain(..end + 2);
                let field = |name: &str| {
                    frame
                        .lines()
                        .find_map(|line| line.strip_prefix(name))
                        .unwrap_or_default()
                        .to_string()
                };
                let event = field("event: ");
                let data = field("data: ");
                return (event, serde_json::from_str(&data).expect("json event"));
            }
            let chunk = tokio::time::timeout(Duration::from_secs(15), self.body.as_mut().next())
                .await
                .expect("a search event inside the provider budget")
                .expect("the search stream stays open")
                .expect("a search stream chunk");
            self.buffer
                .push_str(std::str::from_utf8(&chunk).expect("utf-8 event"));
        }
    }
    async fn ended(&mut self) -> bool {
        tokio::time::timeout(Duration::from_secs(15), self.body.as_mut().next())
            .await
            .expect("the search stream ends")
            .is_none()
    }
}

async fn open(app: &App, path: &str) -> Frames {
    let response = router(app.clone())
        .oneshot(
            Request::builder()
                .uri(path)
                .header(header::AUTHORIZATION, "Bearer member-token-1")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/event-stream; charset=utf-8"
    );
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    Frames {
        body: Box::pin(response.into_body().into_data_stream()),
        buffer: String::new(),
    }
}

fn row_ids(plan: &Value) -> Vec<String> {
    plan["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn each_row_arrives_as_its_provider_completes() {
    let (app, task, started, gate) = stack().await;
    let mut frames = open(&app, "/api/search/stream?query=Fixture").await;
    let (event, plan) = frames.next().await;
    assert_eq!(event, "plan");
    assert_eq!(
        row_ids(&plan),
        vec!["addon:201:movie:fast", "addon:202:movie:slow", "addon:203:movie:broken"],
        "the plan fixes the row order before any provider answers"
    );
    for row in plan["rows"].as_array().unwrap() {
        assert_eq!(row["state"], "Loading");
        assert_eq!(row["cards"], json!([]));
        assert!(row["title"].as_str().is_some_and(|title| !title.is_empty()));
    }
    tokio::time::timeout(Duration::from_secs(15), started.notified())
        .await
        .expect("the slow provider is still running");
    let (event, first) = frames.next().await;
    assert_eq!(event, "row");
    let (event, second) = frames.next().await;
    assert_eq!(event, "row");
    let answered = [first, second];
    assert_eq!(
        answered
            .iter()
            .filter_map(|row| row["id"].as_str())
            .collect::<HashSet<_>>(),
        HashSet::from(["addon:201:movie:fast", "addon:203:movie:broken"]),
        "the fast and the failed row arrive while the slow provider is still gated"
    );
    let fast = answered
        .iter()
        .find(|row| row["id"] == "addon:201:movie:fast")
        .expect("fast row");
    assert_eq!(fast["state"], "Ready");
    assert_eq!(fast["cards"][0]["title"], "Fast fixture");
    let failed = answered
        .iter()
        .find(|row| row["id"] == "addon:203:movie:broken")
        .expect("failed row");
    assert_eq!(failed["state"], "Failed");
    assert_eq!(
        failed["message"],
        "Some search results are unavailable. Try again."
    );
    gate.add_permits(1);
    let (event, slow) = frames.next().await;
    assert_eq!(event, "row");
    assert_eq!(slow["id"], "addon:202:movie:slow");
    assert_eq!(slow["state"], "Ready");
    let (event, done) = frames.next().await;
    assert_eq!(event, "done");
    assert_eq!(done["complete"], false);
    assert!(frames.ended().await);
    task.abort();
}

#[tokio::test]
async fn a_profile_configuration_change_ends_the_stream_without_a_completion() {
    let (app, task, started, gate) = stack().await;
    let mut frames = open(&app, "/api/search/stream?query=Fixture").await;
    assert_eq!(frames.next().await.0, "plan");
    tokio::time::timeout(Duration::from_secs(15), started.notified())
        .await
        .expect("the slow provider is still running");
    assert_eq!(frames.next().await.0, "row");
    assert_eq!(frames.next().await.0, "row");
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO engine_profile_configuration(profile_id,account_id,revision,value) VALUES(1,1,1,?1)
             ON CONFLICT(profile_id) DO UPDATE SET revision=1,value=excluded.value",
            [json!({"scope":{"revision":1},"mode":"Server","name":"Owned","browsing_plugin":"stremio","assignments":[]}).to_string()],
        )
        .unwrap();
    gate.add_permits(1);
    let (event, slow) = frames.next().await;
    assert_eq!(event, "row");
    assert_eq!(slow["id"], "addon:202:movie:slow");
    let (event, failure) = frames.next().await;
    assert_eq!(event, "error");
    assert_eq!(failure["status"], 409);
    assert_eq!(failure["error"], "Profile configuration changed");
    assert!(frames.ended().await, "no completion after an authority change");
    task.abort();
}

#[tokio::test]
async fn parent_search_streams_pending_rows_and_approval_items() {
    let (app, task, _, gate) = stack().await;
    for (method, path, body) in [
        ("PUT", "/api/parent/pin", json!({"pin":"7248"})),
        ("PUT", "/api/profiles/1/kids", json!({"enabled":true,"max_age":7})),
        ("POST", "/api/parent/unlock", json!({"pin":"7248"})),
    ] {
        assert_eq!(
            request(&app, "member-token-1", method, path, body).await.0,
            StatusCode::OK,
            "{path}"
        );
    }
    let mut frames = open(&app, "/api/parent/search/stream?query=Fixture").await;
    let (event, plan) = frames.next().await;
    assert_eq!(event, "plan");
    assert_eq!(row_ids(&plan), vec!["addon:201:movie:fast", "addon:202:movie:slow", "addon:203:movie:broken"]);
    for row in plan["rows"].as_array().unwrap() {
        assert_eq!(row["state"], "Loading");
        assert!(row.get("cards").is_none(), "{row}");
        assert_eq!(row["items"], json!([]));
    }
    gate.add_permits(1);
    let mut completed = 0;
    loop {
        let (event, row) = frames.next().await;
        if event != "row" {
            assert_eq!(event, "done");
            break;
        }
        completed += 1;
        assert!(row.get("cards").is_none(), "{row}");
        let items = row["items"].as_array().expect("approval items");
        if row["state"] == "Failed" {
            assert!(items.is_empty(), "{row}");
            continue;
        }
        assert_eq!(items.len(), 1, "{row}");
        assert_eq!(items[0]["approval_supported"], false);
        assert_eq!(
            items[0]["message"],
            "Metadata is not configured for this result."
        );
        assert_eq!(
            items[0]["card"]["provider_id"],
            format!("addon:{}", row["addon_id"])
        );
    }
    assert_eq!(completed, 3);
    task.abort();
}

#[tokio::test]
async fn a_rejected_query_never_starts_a_stream() {
    let (app, task, _, _) = stack().await;
    let response = router(app.clone())
        .oneshot(
            Request::builder()
                .uri("/api/search/stream?query=")
                .header(header::AUTHORIZATION, "Bearer member-token-1")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["error"],
        "Enter a search query of at most 256 characters"
    );
    task.abort();
}