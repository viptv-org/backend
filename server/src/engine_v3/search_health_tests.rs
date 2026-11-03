use crate::{auth_integration_tests::fixture, test_support::request, *};
use std::sync::atomic::{AtomicUsize, Ordering};

fn install_search_addon(app: &App, address: std::net::SocketAddr, id: i64, name: &str, catalog: &str) {
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

async fn locked_stack() -> (App, tokio::task::JoinHandle<()>) {
    let routes = Router::new().route(
        "/addon/:owner/catalog/:kind/:catalog/:extras",
        get(|Path((_, _, _, _)): Path<(String, String, String, String)>| async move {
            (StatusCode::UNAUTHORIZED, axum::Json(json!({"error":"unauthorized"})))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    let mut app = fixture();
    app.addons.allow_test_loopback = true;
    install_search_addon(&app, address, 301, "Locked 301", "locked");
    crate::test_support::encrypt_fixture_sources(&app);
    (app, task)
}

async fn flaky_stack() -> (App, tokio::task::JoinHandle<()>, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let routes = Router::new().route(
        "/addon/:owner/catalog/:kind/:catalog/:extras",
        get(move |Path((_, kind, _, _)): Path<(String, String, String, String)>| {
            let counted = counted.clone();
            async move {
                if counted.fetch_add(1, Ordering::SeqCst) == 0 {
                    (StatusCode::FORBIDDEN, axum::Json(json!({"error":"forbidden"})))
                } else {
                    (
                        StatusCode::OK,
                        axum::Json(json!({"metas":[{"id":"tt301","type":kind,"name":"Recovered fixture"}]})),
                    )
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
    install_search_addon(&app, address, 302, "Flaky 302", "flaky");
    crate::test_support::encrypt_fixture_sources(&app);
    (app, task, calls)
}

fn find_row<'a>(page: &'a Value, id: &str) -> Option<&'a Value> {
    page["rows"]
        .as_array()?
        .iter()
        .find(|row| row["id"] == id)
}

#[tokio::test]
async fn an_addon_that_always_rejects_access_stops_being_planned() {
    let (app, task) = locked_stack().await;
    for _ in 0..3 {
        let (status, page) = request(
            &app,
            "member-token-1",
            "GET",
            "/api/search?query=Fixture",
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{page}");
        let row = find_row(&page, "addon:301:movie:locked").unwrap_or_else(|| panic!("{page}"));
        assert_eq!(row["state"], "Failed");
        assert_eq!(
            row["message"],
            "This add-on needs valid credentials to search."
        );
        assert_eq!(row["error_code"], "credentials_required");
        assert_eq!(page["complete"], false);
    }
    let (status, page) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/search?query=Fixture",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(
        page["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["id"] != "addon:301:movie:locked"),
        "{page}",
    );
    assert_eq!(page["complete"], true);
    task.abort();
}

#[tokio::test]
async fn a_single_rejected_search_does_not_hide_an_addon_that_recovers() {
    let (app, task, _) = flaky_stack().await;
    let (_, first) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/search?query=Fixture",
        json!({}),
    )
    .await;
    let failed = find_row(&first, "addon:302:movie:flaky").unwrap_or_else(|| panic!("{first}"));
    assert_eq!(failed["state"], "Failed");
    assert_eq!(
        failed["message"],
        "This add-on needs valid credentials to search."
    );
    assert_eq!(failed["error_code"], "credentials_required");
    app.addons.forget_responses();
    let (status, second) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/search?query=Fixture",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    let recovered = find_row(&second, "addon:302:movie:flaky").unwrap_or_else(|| panic!("{second}"));
    assert_eq!(recovered["state"], "Ready");
    assert_eq!(recovered["error_code"], Value::Null);
    assert_eq!(recovered["cards"][0]["title"], "Recovered fixture");
    let (_, third) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/search?query=Fixture",
        json!({}),
    )
    .await;
    assert_eq!(
        find_row(&third, "addon:302:movie:flaky").and_then(|row| row["state"].as_str()),
        Some("Ready"),
        "one transient failure must not hide the add-on",
    );
    task.abort();
}