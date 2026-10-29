use super::*;

async fn fixture_server(delay: Duration) -> (App, tokio::task::JoinHandle<()>, Arc<AtomicUsize>) {
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    let mock = Router::new().route("/discover/trending/today_100.json", get(move |headers: axum::http::HeaderMap| {
        let calls = calls.clone();
        async move {
            assert!(!headers.contains_key(header::AUTHORIZATION));
            assert!(!headers.contains_key("simkl-api-key"));
            calls.fetch_add(1,Ordering::SeqCst);
            tokio::time::sleep(delay).await;
            axum::Json(json!({"movies":[{"title":"Trending Movie","ids":{"simkl_id":10}}],"tv":[{"title":"Trending TV","ids":{"simkl_id":20}}],"anime":[{"title":"Trending Anime","ids":{"simkl_id":30}}]}))
        }
    })).route("/search/movie",get(||async {axum::Json(json!([{"title":"Search Movie","ids":{"simkl_id":10}}]))}))
    .route("/movies/10",get(||async {axum::Json(json!({"title":"Movie Detail","ids":{"simkl":10}}))}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let mut app = fixture();
    app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
    (app, task, count)
}

#[tokio::test]
async fn configured_default_routes_home_discovery_search_and_detail_without_user_token() {
    let (app, task, count) = fixture_server(Duration::from_millis(10)).await;
    let (status, bootstrap) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/bootstrap",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{bootstrap}");
    let rows = bootstrap["home"]["rows"].as_array().unwrap();
    assert_eq!(
        rows.iter()
            .filter(|r| r["provider_id"] == "simkl" && r["state"] == "Ready")
            .count(),
        3
    );
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "one public document shared by three Home rows"
    );
    for (provider_kind, query_kind) in [("movie", "movie"), ("tv", "series"), ("anime", "anime")] {
        let row = rows
            .iter()
            .find(|r| r["id"] == format!("simkl:{provider_kind}:trending"))
            .unwrap();
        assert_eq!(row["browse"]["target"], "Discover");
        let query = row["browse"]["query"].as_array().unwrap();
        assert!(query
            .iter()
            .any(|v| v["id"] == "type" && v["values"][0] == query_kind));
        assert!(query
            .iter()
            .any(|v| v["id"] == "catalog" && v["values"][0] == "simkl-trending"));
    }

    for (path, title) in [
        ("/api/discover?type=movie", "Trending Movie"),
        ("/api/discover?type=movie&search=Movie", "Search Movie"),
    ] {
        let (status, body) = request(&app, "member-token-1", "GET", path, json!({})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["cards"][0]["title"], title);
    }
    let (status, detail) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/meta/movie/simkl:movie:10",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["item"]["title"], "Movie Detail");
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "warm discovery reuses public metadata"
    );
    let (status, legacy) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/meta/movie/simkl:movies:10",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{legacy}");
    assert_eq!(legacy["item"]["identity"]["id"], "simkl:movie:10");
    let (_, config) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/integrations",
        json!({}),
    )
    .await;
    assert_eq!(config["configuration"]["browsing_plugin"], "simkl");
    let assignments = config["configuration"]["assignments"].clone();
    let (status, body) = request(
        &app,
        "member-token-1",
        "PUT",
        "/api/profiles/1/integrations",
        json!({"revision":0,"browsing_plugin":"stremio","assignments":assignments}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, home) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/home",
        json!({}),
    )
    .await;
    assert_eq!(
        home["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["provider_id"] == "simkl")
            .count(),
        0
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/profiles/1/simkl/watched",
            json!({"category":"movie","id":10})
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    task.abort();
}

#[tokio::test]
async fn slow_simkl_home_returns_visible_failed_rows_within_assembly_budget() {
    let (app, task, _) = fixture_server(Duration::from_secs(5)).await;
    let start = Instant::now();
    let (status, body) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/bootstrap",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(start.elapsed() < Duration::from_secs(3));
    let rows = body["home"]["rows"].as_array().unwrap();
    assert_eq!(
        rows.iter()
            .filter(|r| r["provider_id"] == "simkl" && r["state"] == "Failed")
            .count(),
        3
    );
    task.abort();
}
