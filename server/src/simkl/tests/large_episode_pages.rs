use super::*;
#[tokio::test]
async fn simkl_detail_limits_keep_one_episode_read_and_exact_large_title_cursors() {
    let reads = Arc::new(AtomicUsize::new(0));
    let counter = reads.clone();
    let episodes=(1..=2).flat_map(|season|(1..=600).rev().map(move|episode|json!({"episode":(season-1)*600+episode,"type":"episode","aired":true,"tvdb":{"season":season,"episode":episode},"ids":{"simkl_id":season*10000+episode},"title":"Episode"}))).collect::<Vec<_>>();
    let router = Router::new()
        .route(
            "/anime/42",
            get(|| async {
                axum::Json(
                    json!({"title":"Synthetic large title","anime_type":"tv","ids":{"simkl":42}}),
                )
            }),
        )
        .route(
            "/anime/episodes/42",
            get(move || {
                let episodes = episodes.clone();
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    axum::Json(json!(episodes))
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let mut app = fixture();
    app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
    let base = "/api/v3/meta/series/simkl:anime:42";
    let (status, first) = request(&app, "member-token-1", "GET", base, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["episodes"].as_array().unwrap().len(), 24);
    assert_eq!(first["next_cursor"], "24");
    assert_eq!(first["seasons"][1]["offset"], 600);
    for (offset, limit, count, next) in [
        (24, 96, 96, Some("120")),
        (600, 128, 128, Some("728")),
        (1189, 128, 11, None),
        (1200, 1, 0, None),
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
    let (status, small) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v3/profiles/1/simkl/meta/anime/42?offset=600&limit=1",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{small}");
    assert_eq!(small["episodes"][0]["identity"]["id"], "simkl:anime:42:2:1");
    for limit in ["0", "129", "-1"] {
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
    assert_eq!(
        request(
            &app,
            "member-token-2",
            "GET",
            "/api/v3/profiles/1/simkl/meta/anime/42?limit=128",
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    server.abort();
}
