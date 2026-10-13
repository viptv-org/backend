use super::*;
#[tokio::test]
async fn public_calendar_genres_and_scoped_controls_use_v2_data_without_user_tokens() {
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    let router=Router::new()
    .route("/calendar/v2/tv.json",get(move |headers:axum::http::HeaderMap, Query(query):Query<std::collections::HashMap<String,String>>|{let calls=calls.clone();async move{
        assert!(!headers.contains_key(header::AUTHORIZATION));assert!(!headers.contains_key("simkl-api-key")); assert_eq!(query.get("client_id").map(String::as_str),Some("fixture-client"));
        calls.fetch_add(1,Ordering::SeqCst);
        axum::Json(json!({"calendar":[{"simkl_id":20,"date":"2026-10-13T04:00:00Z","episode":{"season":2,"episode":3,"title":"Actual episode"}},{"simkl_id":20,"date":"2026-10-13T04:00:00Z","episode":{"season":2,"episode":4,"title":"Another episode"}},{"simkl_id":20,"date":"2026-10-21T04:00:00Z","episode":{"season":2,"episode":5}}],"metadata":{"20":{"title":"Actual series","ids":{"simkl_id":20,"imdb":"tt1234567"}}}}))
    }}))
    .route("/discover/trending/movies/genre/index.json",get(||async{axum::Json(json!([{"name":"Science fiction","slug":"science-fiction","count":2}]))}))
    .route("/discover/trending/movies/genre/science-fiction_month_500.json",get(|headers:axum::http::HeaderMap|async move{assert!(!headers.contains_key(header::AUTHORIZATION));axum::Json(json!([{"title":"Filtered movie","ids":{"simkl_id":10}}]))}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let mut app = fixture();
    app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
    let path = "/api/v3/calendar?type=series&start=2026-10-12&end=2026-10-18";
    let (status, page) = request(&app, "member-token-1", "GET", path, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let entries = page["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert_ne!(entries[0]["id"], entries[1]["id"]);
    for entry in entries {
        assert_eq!(entry["card"]["identity"]["id"], "simkl:tv:20");
        assert!(entry["card"]["identity"]["episode"].is_null());
        assert_eq!(entry["date"], "2026-10-13T04:00:00Z");
    }
    let (_, again) = request(&app, "member-token-1", "GET", path, json!({})).await;
    assert_eq!(again, page);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    let (status, controls) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v3/discovery-controls?type=movie",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{controls}");
    assert_eq!(controls["provider_id"], "simkl");
    assert_eq!(controls["calendar"]["availability"], "Available");
    assert!(
        controls["controls"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == "genre" && c["choices"][1]["value"] == "science-fiction")
    );
    let (status, page) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v3/discover?type=movie&genre=science-fiction",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["cards"][0]["identity"]["id"], "simkl:movie:10");
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/v3/discover?type=movie&genre=unadvertised",
            json!({})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(request(&app,"member-token-1","GET","/api/v3/discover?type=movie&genre=science-fiction&extras=%7B%22timeframe%22%3A%22week%22%7D",json!({})).await.0,StatusCode::BAD_REQUEST);
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO kids_profiles(profile_id,enabled,max_age,revision) VALUES(1,1,12,1)",
            [],
        )
        .unwrap();
    let (_, page) = request(&app, "member-token-1", "GET", path, json!({})).await;
    assert!(page["entries"].as_array().unwrap().is_empty());
    task.abort();
}
#[tokio::test]
async fn calendar_configuration_date_window_and_session_revocation_are_not_empty_success() {
    let app = fixture();
    let (status, _) = request(&app, "member-token-1", "GET", "/api/v3/calendar", json!({})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (_, controls) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v3/discovery-controls",
        json!({}),
    )
    .await;
    assert_eq!(controls["provider_id"], "stremio");
    assert_eq!(controls["calendar"]["availability"], "Unsupported");
    let mut app = fixture();
    app.simkl = Arc::new(Service::fixture("http://127.0.0.1:9"));
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/v3/calendar?start=2026-02-30",
            json!({})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/v3/calendar?start=2026-01-01&end=2026-12-31",
            json!({})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(&app, "wrong-token", "GET", "/api/v3/calendar", json!({}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}
