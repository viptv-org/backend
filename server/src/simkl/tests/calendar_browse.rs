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
    .route("/discover/trending/movies/genre/index.json",get(||async{axum::Json(json!([{"name":"All Genre","slug":"all","count":500},{"name":"Science fiction","slug":"science-fiction","count":2},{"name":"Duplicate science fiction","slug":"science-fiction","count":2}]))}))
    .route("/discover/trending/movies/genre/science-fiction_month_500.json",get(|headers:axum::http::HeaderMap|async move{assert!(!headers.contains_key(header::AUTHORIZATION));axum::Json(json!([{"title":"Filtered movie","ids":{"simkl_id":10}}]))}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let mut app = fixture();
    app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
    let path = "/api/calendar?type=series&start=2026-10-12&end=2026-10-18";
    let (status, page) = request(&app, "member-token-1", "GET", path, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let entries = page["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert_ne!(entries[0]["id"], entries[1]["id"]);
    for entry in entries {
        assert_eq!(entry["card"]["metadata_origin"]["provider_id"],"simkl");
        assert_eq!(entry["card"]["identity"]["id"], "simkl:tv:20");
        assert!(entry["card"]["identity"]["episode"].is_null());
        assert_eq!(entry["date"], "2026-10-13T04:00:00Z");
    }
    let (status, limited) = request(
        &app,
        "member-token-1",
        "GET",
        &format!("{path}&limit=1"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(limited["entries"].as_array().unwrap().len(), 1);
    assert_eq!(limited["next_cursor"], "1");
    let (status, larger) = request(
        &app,
        "member-token-1",
        "GET",
        &format!("{path}&limit=500"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(larger, page);
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            &format!("{path}&limit=501"),
            json!({})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let (_, again) = request(&app, "member-token-1", "GET", path, json!({})).await;
    assert_eq!(again, page);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    let (status, controls) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/discovery-controls?type=movie",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{controls}");
    assert_eq!(controls["provider_id"], "simkl");
    let genre_choices = controls["controls"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "genre")
        .unwrap()["choices"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(genre_choices.len(), 2);
    assert_eq!(
        genre_choices.iter().filter(|c| c["value"] == "all").count(),
        1
    );
    assert_eq!(controls["calendar"]["availability"], "Available");
    assert!(controls["controls"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["id"] == "genre" && c["choices"][1]["value"] == "science-fiction"));
    let (status, page) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/discover?type=movie&genre=science-fiction",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["cards"][0]["identity"]["id"], "simkl:movie:10");
    let (status, filtered_controls) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/discovery-controls?type=movie&genre=science-fiction",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let timeframe = filtered_controls["controls"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "timeframe")
        .unwrap();
    assert_eq!(timeframe["choices"].as_array().unwrap().len(), 1);
    assert_eq!(timeframe["choices"][0]["value"], "month");
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/discover?type=movie&genre=unadvertised",
            json!({})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(request(&app,"member-token-1","GET","/api/discover?type=movie&genre=science-fiction&extras=%7B%22timeframe%22%3A%22week%22%7D",json!({})).await.0,StatusCode::BAD_REQUEST);
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
    let (status, _) = request(&app, "member-token-1", "GET", "/api/calendar", json!({})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (_, controls) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/discovery-controls",
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
            "/api/calendar?start=2026-02-30",
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
            "/api/calendar?start=2026-01-01&end=2026-12-31",
            json!({})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(&app, "wrong-token", "GET", "/api/calendar", json!({}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn all_types_search_aggregates_only_selected_metadata_provider_and_rejects_ambiguous_paging()
{
    let calls = Arc::new(Mutex::new(Vec::new()));
    let router=Router::new().route("/search/:category",get({let calls=calls.clone();move|Path(category):Path<String>|{let calls=calls.clone();async move{
        calls.lock().unwrap().push(category.clone());
        axum::Json(json!([{"title":format!("Actual {category} title"),"ids":{"simkl_id":match category.as_str(){"movie"=>10,"tv"=>20,_=>30}}}]))
    }}}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let mut app = fixture();
    app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
    let (status, page) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/discover?type=all&search=Actual",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let cards = page["cards"].as_array().unwrap();
    assert_eq!(cards.len(), 3);
    for id in ["simkl:movie:10", "simkl:tv:20", "simkl:anime:30"] {
        assert!(cards.iter().any(|c| c["identity"]["id"] == id));
    }
    assert!(cards.iter().all(|c| c["provider_id"] == "simkl"));
    assert_eq!(calls.lock().unwrap().len(), 3);
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/discover?type=all&search=Actual&skip=24",
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
            "/api/discover?type=all",
            json!({})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        calls.lock().unwrap().len(),
        3,
        "invalid broad requests must not dispatch provider calls"
    );
    task.abort();
}

#[tokio::test]
async fn a_cold_calendar_does_not_block_genres_and_identical_fetches_coalesce() {
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let reads = Arc::new(AtomicUsize::new(0));
    let routes = Router::new()
        .route(
            "/calendar/v2/tv.json",
            get({
                let started = started.clone();
                let release = release.clone();
                let reads = reads.clone();
                move || {
                    let started = started.clone();
                    let release = release.clone();
                    let reads = reads.clone();
                    async move {
                        reads.fetch_add(1, Ordering::SeqCst);
                        started.notify_one();
                        let _permit = release.acquire().await.unwrap();
                        axum::Json(json!({"calendar":[],"metadata":{}}))
                    }
                }
            }),
        )
        .route(
            "/discover/trending/movies/genre/index.json",
            get(|| async { axum::Json(json!([{"name":"Drama","slug":"drama"}])) }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    let service = Arc::new(Service::fixture(&format!("http://{address}")));
    let first = tokio::spawn({
        let service = service.clone();
        async move {
            service
                .public_document("/calendar/v2/tv.json", Duration::from_secs(60), || Ok(()))
                .await
                .unwrap()
        }
    });
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .unwrap();
    let second = tokio::spawn({
        let service = service.clone();
        async move {
            service
                .public_document("/calendar/v2/tv.json", Duration::from_secs(60), || Ok(()))
                .await
                .unwrap()
        }
    });
    let genres = tokio::time::timeout(
        Duration::from_secs(2),
        service.public_document(
            "/discover/trending/movies/genre/index.json",
            Duration::from_secs(60),
            || Ok(()),
        ),
    )
    .await
    .expect("An unrelated document must not wait for the calendar")
    .unwrap();
    assert_eq!(genres[0]["slug"], "drama");
    release.add_permits(1);
    assert_eq!(first.await.unwrap(), second.await.unwrap());
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    task.abort();
}
#[tokio::test]
async fn every_type_in_one_request_merges_the_cached_calendars_and_pages_them() {
    let reads = Arc::new(AtomicUsize::new(0));
    let doc = |reads: &Arc<AtomicUsize>, body: Value| {
        let reads = reads.clone();
        get(move || {
            let reads = reads.clone();
            let body = body.clone();
            async move {
                reads.fetch_add(1, Ordering::SeqCst);
                axum::Json(body)
            }
        })
    };
    let router = Router::new()
        .route("/calendar/v2/tv.json", doc(&reads, json!({"calendar":[{"simkl_id":20,"date":"2026-10-13T04:00:00Z","episode":{"season":2,"episode":3,"title":"Series chapter"}}],"metadata":{"20":{"title":"Merged series","ids":{"simkl_id":20}}}})))
        .route("/calendar/v2/anime.json", doc(&reads, json!({"calendar":[{"simkl_id":30,"date":"2026-10-14T04:00:00Z","episode":{"season":1,"episode":1}}],"metadata":{"30":{"title":"Merged anime","ids":{"simkl_id":30}}}})))
        .route("/calendar/v2/movie_release.json", doc(&reads, json!({"calendar":[{"simkl_id":40,"date":"2026-10-12T04:00:00Z"}],"metadata":{"40":{"title":"Merged movie","ids":{"simkl_id":40}}}})));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let mut app = fixture();
    app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
    let path = "/api/calendar?type=all&start=2026-10-12&end=2026-10-18";
    let (status, page) = request(&app, "member-token-1", "GET", path, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["sources"], json!(["public"]));
    assert_eq!(page["complete"], true);
    assert!(page["next_cursor"].is_null());
    assert!(page.get("refresh_after_millis").is_none());
    let entries = page["entries"].as_array().unwrap();
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry["card"]["identity"]["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["simkl:movie:40", "simkl:tv:20", "simkl:anime:30"],
        "{page}"
    );
    assert!(entries.iter().all(|entry| entry["source"] == "public"));
    assert_eq!(entries[1]["episode"], 3);
    assert_eq!(entries[1]["episode_title"], "Series chapter");
    for (kind, path) in [
        ("series", "/api/calendar?type=series&start=2026-10-12&end=2026-10-18"),
        ("anime", "/api/calendar?type=anime&start=2026-10-12&end=2026-10-18"),
        ("movie", "/api/calendar?type=movie&start=2026-10-12&end=2026-10-18"),
    ] {
        let (status, single) = request(&app, "member-token-1", "GET", path, json!({})).await;
        assert_eq!(status, StatusCode::OK, "{single}");
        let single = single["entries"].as_array().unwrap();
        assert_eq!(single.len(), 1, "{kind}");
        let merged = entries
            .iter()
            .find(|entry| entry["card"]["identity"]["id"] == single[0]["card"]["identity"]["id"])
            .unwrap_or_else(|| panic!("{kind} is missing from the merged page"));
        assert_eq!(merged["id"], single[0]["id"], "{kind} keeps its row id");
    }
    assert_eq!(reads.load(Ordering::SeqCst), 3);
    let (status, first) = request(&app, "member-token-1", "GET", &format!("{path}&limit=2"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["entries"].as_array().unwrap().len(), 2);
    assert_eq!(first["next_cursor"], "2");
    assert_eq!(first["complete"], false);
    let (status, second) = request(&app, "member-token-1", "GET", &format!("{path}&skip=2&limit=2"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    let second = second["entries"].as_array().unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(second[0]["card"]["identity"]["id"], "simkl:anime:30");
    assert_eq!(second[0]["id"], entries[2]["id"]);
    assert_eq!(reads.load(Ordering::SeqCst), 3, "one cached document per type");
    task.abort();
}

#[tokio::test]
async fn a_failing_type_leaves_the_others_in_the_merged_page() {
    let router = Router::new()
        .route("/calendar/v2/tv.json", get(|| async { StatusCode::INTERNAL_SERVER_ERROR }))
        .route("/calendar/v2/anime.json", get(|| async { axum::Json(json!({"calendar":[{"simkl_id":30,"date":"2026-10-14T04:00:00Z","episode":{"season":1,"episode":1}}],"metadata":{"30":{"title":"Merged anime","ids":{"simkl_id":30}}}})) }))
        .route("/calendar/v2/movie_release.json", get(|| async { axum::Json(json!({"calendar":[{"simkl_id":40,"date":"2026-10-12T04:00:00Z"}],"metadata":{"40":{"title":"Merged movie","ids":{"simkl_id":40}}}})) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let mut app = fixture();
    app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
    let path = "/api/calendar?type=all&start=2026-10-12&end=2026-10-18";
    let (status, page) = request(&app, "member-token-1", "GET", path, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(
        page["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["card"]["identity"]["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["simkl:movie:40", "simkl:anime:30"]
    );
    assert_eq!(page["sources"], json!(["public"]));
    task.abort();
}

#[tokio::test]
async fn a_merged_page_fails_only_when_no_type_answers() {
    let router = Router::new()
        .route("/calendar/v2/tv.json", get(|| async { StatusCode::INTERNAL_SERVER_ERROR }))
        .route("/calendar/v2/anime.json", get(|| async { StatusCode::INTERNAL_SERVER_ERROR }))
        .route("/calendar/v2/movie_release.json", get(|| async { StatusCode::INTERNAL_SERVER_ERROR }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let mut app = fixture();
    app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
    let (status, _) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/calendar?type=all&start=2026-10-12&end=2026-10-18",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    task.abort();
}
