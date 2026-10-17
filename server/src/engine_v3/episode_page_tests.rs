use crate::{auth_integration_tests::fixture, test_support::request, *};
use std::sync::atomic::{AtomicUsize, Ordering};
#[tokio::test]
async fn stremio_detail_limits_reuse_metadata_and_preserve_defaults_and_terminal_pages() {
    let reads = Arc::new(AtomicUsize::new(0));
    let counter = reads.clone();
    let videos=(1..=1200).map(|episode|json!({"id":format!("opaque-{episode}"),"season":1,"episode":episode,"title":"Episode"})).collect::<Vec<_>>();
    let router=Router::new().route("/meta/series/large-parent.json",get(move||{let videos=videos.clone();let counter=counter.clone();async move{counter.fetch_add(1,Ordering::SeqCst);axum::Json(json!({"meta":{"id":"large-parent","type":"series","name":"Synthetic large title","videos":videos}}))}}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let mut app = fixture();
    app.addons.allow_test_loopback = true;
    app.db.lock().unwrap().execute("INSERT INTO addons(name,manifest_url,manifest,account_id) VALUES('Metadata',?1,?2,1)",params![format!("http://{address}/manifest.json"),json!({"id":"fixture","name":"Metadata","version":"1","resources":["meta"],"types":["series"]}).to_string()]).unwrap();
    crate::test_support::encrypt_fixture_sources(&app);
    let base = "/api/meta/series/large-parent";
    let (status, first) = request(&app, "member-token-1", "GET", base, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["episodes"].as_array().unwrap().len(), 24);
    for (offset, limit, count, next) in [
        (0, 96, 96, Some("96")),
        (96, 128, 128, Some("224")),
        (1190, 96, 10, None),
    ] {
        let (status, page) = request(
            &app,
            "member-token-1",
            "GET",
            &format!("{base}?offset={offset}&limit={limit}"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{page}");
        assert_eq!(page["episodes"].as_array().unwrap().len(), count);
        assert_eq!(page["next_cursor"].as_str(), next);
        assert_eq!(page["seasons"], first["seasons"]);
    }
    for limit in [0, 129] {
        assert_eq!(
            request(
                &app,
                "member-token-1",
                "GET",
                &format!("{base}?limit={limit}"),
                Value::Null
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    server.abort();
}
