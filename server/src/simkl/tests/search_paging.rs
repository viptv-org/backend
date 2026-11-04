use super::*;

async fn pageable_fixture() -> (App, tokio::task::JoinHandle<()>, Arc<AtomicUsize>) {
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    let routes = Router::new()
        .route("/", axum::routing::any(|| async { "ok" }))
        .route(
            "/oauth2/token",
            post(|| async {
                axum::Json(json!({"access_token":"fixture-private-token","refresh_token":"fixture-refresh-token","token_type":"Bearer","expires_in":604800,"scope":"media:read media:write"}))
            }),
        )
        .route(
            "/search/:category",
            get(
                move |Path(category): Path<String>,
                      Query(query): Query<HashMap<String, String>>,
                      headers: axum::http::HeaderMap| {
                    let calls = calls.clone();
                    async move {
                        if headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            != Some("Bearer fixture-private-token")
                        {
                            return (StatusCode::UNAUTHORIZED, axum::Json(json!({"error":"unauthorized"})));
                        }
                        calls.fetch_add(1, Ordering::SeqCst);
                        let page: usize =
                            query.get("page").and_then(|s| s.parse().ok()).unwrap_or(1);
                        let start = (page - 1) * 24;
                        let end = (start + 24).min(120);
                        let items = (start..end)
                            .map(|i| {
                                json!({"title": format!("{category} {i}"), "ids": {"simkl_id": i + 1}})
                            })
                            .collect::<Vec<_>>();
                        (StatusCode::OK, axum::Json(json!(items)))
                    }
                },
            ),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    let mut app = fixture();
    app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
    (app, task, count)
}

async fn browse_simkl(app: &App) {
    let (status, body) = request(
        app,
        "member-token-1",
        "GET",
        "/api/profiles/1/integrations",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mut value = body["configuration"].clone();
    value["browsing_plugin"] = json!("simkl");
    value["scope"]["revision"] = json!(1);
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO engine_profile_configuration(profile_id,account_id,revision,value) VALUES(1,1,1,?1) ON CONFLICT(profile_id) DO UPDATE SET revision=1,value=excluded.value",
            [value.to_string()],
        )
        .unwrap();
}

fn seed_kids_movies(app: &App, all_restricted: bool) {
    let db = app.db.lock().unwrap();
    for i in 1..=120usize {
        let age = if all_restricted || (i - 1) % 24 >= 6 {
            Some(18)
        } else {
            Some(0)
        };
        db.execute(
            "INSERT INTO kids_media(account_id,kind,id,parent_id,metadata,age,conflict,updated_at) VALUES(1,'movie',?1,?1,?2,?3,0,1)",
            params![format!("simkl:movie:{i}"), json!({"name": format!("movie {i}")}).to_string(), age],
        )
        .unwrap();
    }
}

fn enable_kids(app: &App) {
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO kids_profiles(profile_id,enabled,max_age,revision) VALUES(1,1,17,1) ON CONFLICT(profile_id) DO UPDATE SET enabled=1,max_age=17,revision=kids_profiles.revision+1",
            [],
        )
        .unwrap();
}

fn allowed_index(card: &Value) -> usize {
    card["identity"]["id"]
        .as_str()
        .unwrap()
        .strip_prefix("simkl:movie:")
        .unwrap()
        .parse()
        .unwrap()
}

#[tokio::test]
async fn a_kids_profile_pages_simkl_search_into_full_filtered_pages_and_ends_cleanly() {
    let (app, task, _) = pageable_fixture().await;
    link(&app).await;
    browse_simkl(&app).await;
    seed_kids_movies(&app, false);
    enable_kids(&app);

    let (status, first) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/discover?type=movie&search=fixture",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let first_cards = first["cards"].as_array().unwrap();
    assert_eq!(first_cards.len(), 24);
    for card in first_cards {
        let index = allowed_index(card);
        assert!((index - 1) % 24 < 6, "adult title {index} leaked into a kids search page");
    }
    assert_eq!(first["next_cursor"], "96");
    assert_eq!(first["complete"], false);

    let (status, rest) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/discover?type=movie&search=fixture&skip=96",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rest}");
    assert_eq!(rest["cards"].as_array().unwrap().len(), 6);
    assert_eq!(rest["next_cursor"], Value::Null);
    assert_eq!(rest["complete"], true);
    task.abort();
}

#[tokio::test]
async fn a_fully_restricted_simkl_search_stops_at_the_page_cap_with_a_cursor_that_advances() {
    let (app, task, calls) = pageable_fixture().await;
    link(&app).await;
    browse_simkl(&app).await;
    seed_kids_movies(&app, true);
    enable_kids(&app);

    let before = calls.load(Ordering::SeqCst);
    let (status, first) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/discover?type=movie&search=fixture",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert!(first["cards"].as_array().unwrap().is_empty());
    assert_eq!(first["next_cursor"], "120");
    assert_eq!(first["complete"], false);
    assert_eq!(
        calls.load(Ordering::SeqCst) - before,
        5,
        "the search over-fetch stops at the page cap instead of paging forever"
    );

    let (status, rest) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/discover?type=movie&search=fixture&skip=120",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rest}");
    assert!(rest["cards"].as_array().unwrap().is_empty());
    assert_eq!(rest["next_cursor"], Value::Null);
    assert_eq!(rest["complete"], true);
    task.abort();
}