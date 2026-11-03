use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

async fn slow_not_found(hits: Arc<AtomicUsize>) -> (tokio::task::JoinHandle<()>, std::net::SocketAddr) {
    use axum::{routing::get, Router};
    let mock = Router::new().route(
        "/addon/meta/series/tt-missing.json",
        get(move || {
            let hits = hits.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_secs(3)).await;
                axum::Json(json!({"meta": null}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    (server, address)
}

fn install(app: &App, name: &str, address: std::net::SocketAddr, manifest: Value) -> i64 {
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO addons(name,manifest_url,manifest,account_id) VALUES(?1,?2,?3,1)",
            params![
                name,
                format!("http://{address}/addon/manifest.json"),
                manifest.to_string()
            ],
        )
        .unwrap();
    app.db.lock().unwrap().last_insert_rowid()
}

#[tokio::test]
async fn not_found_meta_fails_fast_and_never_enriches() {
    let hits = Arc::new(AtomicUsize::new(0));
    let (server, address) = slow_not_found(hits.clone()).await;
    let mut app = fixture();
    app.addons.allow_test_loopback = true;
    let manifest =
        json!({"id":"slow-not-found","name":"Slow not found","resources":["meta"],"types":["series"]});
    install(&app, "Slow not found", address, manifest);
    crate::test_support::encrypt_fixture_sources(&app);
    let begin = Instant::now();
    let (status, first) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/meta/series/tt-missing",
        Value::Null,
    )
    .await;
    let first_elapsed = begin.elapsed();
    assert_eq!(status, StatusCode::NOT_FOUND, "{first}");
    assert_eq!(first["error_code"], "metadata_not_found");
    assert!(
        first_elapsed < Duration::from_secs_f32(3.5),
        "first lookup took {first_elapsed:?}"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    let begin = Instant::now();
    let (status, second) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/meta/series/tt-missing",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{second}");
    assert_eq!(second["error_code"], "metadata_not_found");
    assert!(
        begin.elapsed() < Duration::from_secs(1),
        "repeat lookup took {:?}",
        begin.elapsed()
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn selected_not_found_stops_before_enriching_others() {
    use axum::{routing::get, Router};
    let series_hits = Arc::new(AtomicUsize::new(0));
    let counter = series_hits.clone();
    let mock = Router::new().route(
        "/addon/meta/series/tt-missing.json",
        get(move || {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                axum::Json(
                    json!({"meta": {"id":"tt-missing","type":"series","videos":[{"thumbnail":""}]}}),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let series_server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let not_found_hits = Arc::new(AtomicUsize::new(0));
    let (not_found_server, not_found_address) = slow_not_found(not_found_hits).await;
    let mut app = fixture();
    app.addons.allow_test_loopback = true;
    let slow = install(
        &app,
        "Slow not found",
        not_found_address,
        json!({"id":"slow-not-found","name":"Slow not found","resources":["meta"],"types":["series"],"idPrefixes":["tt"]}),
    );
    install(
        &app,
        "Fast series",
        address,
        json!({"id":"fast-series","name":"Fast series","resources":["meta"],"types":["series"],"idPrefixes":["tt"]}),
    );
    crate::test_support::encrypt_fixture_sources(&app);
    let begin = Instant::now();
    let (status, body) = request(
        &app,
        "member-token-1",
        "GET",
        &format!("/api/meta/series/tt-missing?provider_id=stremio&addon_id={slow}"),
        Value::Null,
    )
    .await;
    let elapsed = begin.elapsed();
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(
        body["error"],
        "The selected metadata add-on did not return this title"
    );
    assert!(
        elapsed < Duration::from_secs_f32(3.5),
        "selected lookup took {elapsed:?}"
    );
    assert_eq!(series_hits.load(Ordering::SeqCst), 0);
    series_server.abort();
    not_found_server.abort();
}
