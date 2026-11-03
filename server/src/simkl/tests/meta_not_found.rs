use super::*;

#[tokio::test]
async fn not_found_meta_fails_fast_and_never_enriches() {
    use axum::{routing::get, Router};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let mock = Router::new().route(
        "/addon/meta/series/tt-missing.json",
        get(move || {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_secs(3)).await;
                axum::Json(json!({"meta": null}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let mut app = fixture();
    app.addons.allow_test_loopback = true;
    let manifest = json!({"id":"slow-not-found","name":"Slow not found","resources":["meta"],"types":["series"]});
    app.db.lock().unwrap().execute(
        "INSERT INTO addons(name,manifest_url,manifest,account_id) VALUES('Slow not found',?1,?2,1)",
        params![format!("http://{address}/addon/manifest.json"), manifest.to_string()],
    )
    .unwrap();
    crate::test_support::encrypt_fixture_sources(&app);
    let (status, first) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/meta/series/tt-missing",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{first}");
    assert_eq!(first["error_code"], "metadata_not_found");
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