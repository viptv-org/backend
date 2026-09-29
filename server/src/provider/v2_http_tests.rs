use crate::{auth_integration_tests::fixture, test_support::request, *};

fn seeded() -> App {
    let mut app = fixture();
    app.providers.allow_test_loopback = true;
    {
        let db = app.db.lock().unwrap();
        for id in 1..=4 {
            db.execute("INSERT INTO providers(id,name,url,username,password,enabled) VALUES(?1,?2,'http://fixture.invalid','private-user','private-password',1)", params![id,format!("Provider {id}")]).unwrap();
        }
        db.execute_batch("INSERT INTO provider_ownership VALUES(1,1),(2,1),(3,2)")
            .unwrap();
        for index in 0..235 {
            db.execute("INSERT INTO provider_vod(id,provider_id,stream_id,kind,name,normalized,extension) VALUES(?1,1,?2,'movie',?3,'fixture','mp4')", params![format!("vod:1:{index:04}"),index.to_string(),format!("Fixture movie {index}")]).unwrap();
        }
        for provider in [3, 4] {
            db.execute("INSERT INTO provider_vod(id,provider_id,stream_id,kind,name,normalized,extension) VALUES(?1,?2,'1','movie','Private hidden title','private','mp4')", params![format!("vod:{provider}:1"),provider]).unwrap();
        }
    }
    app
}

#[tokio::test]
async fn account_matches_are_paged_and_cursors_cannot_cross_accounts() {
    let app = seeded();
    let (status, first) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v2/iptv/matches",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["items"].as_array().unwrap().len(), 50);
    let cursor = first["next_cursor"].as_str().unwrap();
    let path = format!("/api/v2/iptv/matches?cursor={cursor}");
    let (status, second) = request(&app, "member-token-1", "GET", &path, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(first["items"][0]["vod_id"], second["items"][0]["vod_id"]);
    assert_eq!(
        request(&app, "member-token-2", "GET", &path, Value::Null)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let (_, own) = request(
        &app,
        "member-token-2",
        "GET",
        "/api/v2/iptv/matches",
        Value::Null,
    )
    .await;
    assert_eq!(own["items"].as_array().unwrap().len(), 1);
    assert_eq!(own["items"][0]["provider_id"], 3);
    let (_, hidden) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v2/iptv/matches?search=Private",
        Value::Null,
    )
    .await;
    assert!(hidden["items"].as_array().unwrap().is_empty());
    assert!(!first.to_string().contains("private-password"));
    for query in [
        "limit=201",
        "limit=0",
        "limit=abc",
        "offset=20",
        "kind=episode",
    ] {
        assert_eq!(
            request(
                &app,
                "member-token-1",
                "GET",
                &format!("/api/v2/iptv/matches?{query}"),
                Value::Null
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
}

#[tokio::test]
async fn match_edits_and_defaults_are_account_owned_not_owner_role_global() {
    let app = seeded();
    let body = json!({"vod_id":"vod:1:0000","metadata_id":"tt1234567","type":"movie"});
    assert_eq!(
        request(&app, "member-token-1", "PUT", "/api/v2/iptv/matches", body)
            .await
            .0,
        StatusCode::OK
    );
    let (_, first) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v2/iptv/matches?limit=1",
        Value::Null,
    )
    .await;
    assert_eq!(first["items"][0]["vod_id"], "vod:1:0001");
    let mut failures = Vec::new();
    for id in ["vod:3:1", "unknown"] {
        failures.push(
            request(
                &app,
                "member-token-1",
                "PUT",
                "/api/v2/iptv/matches",
                json!({"vod_id":id,"metadata_id":"tt9999999","type":"movie"}),
            )
            .await,
        );
    }
    assert_eq!(failures[0], failures[1]);
    assert_eq!(failures[0].0, StatusCode::NOT_FOUND);
    let (_, default) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v2/iptv/live-default",
        Value::Null,
    )
    .await;
    assert_eq!(default["catalog_id"], 1);
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/v2/iptv/live-default",
            json!({"catalog_id":3})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/v2/iptv/live-default",
            json!({"catalog_id":2})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, default) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v2/iptv/live-default",
        Value::Null,
    )
    .await;
    assert_eq!(default["catalog_id"], 2);
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE providers SET enabled=0 WHERE id=2", [])
        .unwrap();
    let (_, default) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v2/iptv/live-default",
        Value::Null,
    )
    .await;
    assert_eq!(default["catalog_id"], 1);
    // Operator role does not expand the account-owned data boundary.
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE auth_accounts SET role='owner' WHERE id=1", [])
        .unwrap();
    let (_, hidden) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v2/iptv/matches?provider_id=3",
        Value::Null,
    )
    .await;
    assert!(hidden["items"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn locked_kids_and_paired_devices_cannot_manage_account_sources() {
    let app = seeded();
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO kids_profiles(profile_id,enabled) VALUES(1,1)",
            [],
        )
        .unwrap();
    let (status, error) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v2/iptv/matches",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error["error_code"], "parent_required");
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE kids_profiles SET enabled=0 WHERE profile_id=1", [])
        .unwrap();
    app.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE auth_sessions SET kind='device' WHERE account_id=1",
            [],
        )
        .unwrap();
    let (status, error) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v2/iptv/matches",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error["error_code"], "account_session_required");
}

#[tokio::test]
async fn raw_live_pages_preserve_order_logos_and_account_default() {
    let app = seeded();
    {
        let db = app.db.lock().unwrap();
        for provider in 1..=4 {
            db.execute(
                "INSERT INTO provider_live_generations VALUES(?1,7)",
                [provider],
            )
            .unwrap();
            for index in 0..235 {
                db.execute("INSERT INTO provider_live(id,provider_id,stream_id,name,logo,category_id,category,ordinal) VALUES(?1,?2,?3,?4,'http://images.invalid/logo.png',?5,?5,?6)", params![format!("iptv:{provider}:{index}"),provider,index.to_string(),format!("Channel {:03}",235-index),if index%2==0 {"news"} else {"sports"},index]).unwrap();
            }
            db.execute("INSERT INTO provider_live_categories_v2 VALUES(?1,'sports','Sports',0),(?1,'news','News',1)", [provider]).unwrap();
        }
    }
    let root = "/api/v2/iptv/live/channels";
    let (status, first) = request(&app, "member-token-1", "GET", root, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["catalog_id"], 1);
    assert_eq!(first["generation"], 7);
    assert_eq!(first["items"].as_array().unwrap().len(), 50);
    assert_eq!(first["items"][0]["id"], "iptv:1:0");
    assert_eq!(first["items"][0]["name"], "Channel 235");
    assert_eq!(first["items"][0]["logo"], "http://images.invalid/logo.png");
    assert!(first.get("total").is_none());
    assert!(!first.to_string().contains("private-password"));
    let cursor = first["next_cursor"].as_str().unwrap();
    let next_path = format!("{root}?cursor={cursor}");
    let (status, second) = request(&app, "member-token-1", "GET", &next_path, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second["items"][0]["id"], "iptv:1:50");
    let mut ids = first["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["id"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    let mut next = first["next_cursor"].as_str().map(str::to_owned);
    while let Some(cursor) = next {
        let (status, page) = request(
            &app,
            "member-token-1",
            "GET",
            &format!("{root}?cursor={cursor}"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        ids.extend(
            page["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v["id"].as_str().unwrap().to_owned()),
        );
        next = page["next_cursor"].as_str().map(str::to_owned);
    }
    assert_eq!(
        ids,
        (0..235).map(|i| format!("iptv:1:{i}")).collect::<Vec<_>>()
    );
    let (_, overridden) = request(
        &app,
        "member-token-1",
        "GET",
        &format!("{root}?catalog_id=2&limit=1"),
        Value::Null,
    )
    .await;
    assert_eq!(overridden["items"][0]["id"], "iptv:2:0");
    let (_, default) = request(&app, "member-token-1", "GET", root, Value::Null).await;
    assert_eq!(default["catalog_id"], 1);
    let (_, filtered) = request(
        &app,
        "member-token-1",
        "GET",
        &format!("{root}?category_id=sports&limit=2"),
        Value::Null,
    )
    .await;
    assert_eq!(filtered["items"][0]["id"], "iptv:1:1");
    assert_eq!(filtered["items"][1]["id"], "iptv:1:3");
    let (_, search) = request(
        &app,
        "member-token-1",
        "GET",
        &format!("{root}?search=234"),
        Value::Null,
    )
    .await;
    assert_eq!(search["items"].as_array().unwrap().len(), 1);
    assert_eq!(search["items"][0]["id"], "iptv:1:1");
    for query in ["limit=0", "limit=201", "offset=10", "limit=abc"] {
        assert_eq!(
            request(
                &app,
                "member-token-1",
                "GET",
                &format!("{root}?{query}"),
                Value::Null
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    let mut denied = Vec::new();
    for id in [3, 4, 999] {
        denied.push(
            request(
                &app,
                "member-token-1",
                "GET",
                &format!("{root}?catalog_id={id}"),
                Value::Null,
            )
            .await,
        );
    }
    assert_eq!(denied[0], denied[1]);
    assert_eq!(denied[0], denied[2]);
    assert_eq!(denied[0].0, StatusCode::NOT_FOUND);
    {
        let db = app.db.lock().unwrap();
        db.execute_batch("INSERT INTO profiles(id,name,avatar_seed,presentation_complete) VALUES(2,'Second','second',1);
            INSERT INTO profile_owners(profile_id,account_id,created_at) VALUES(2,2,0);
            INSERT INTO auth_profiles VALUES(2,2);
            UPDATE auth_sessions SET profile_id=2 WHERE account_id=2;").unwrap();
    }
    assert_eq!(
        request(&app, "member-token-2", "GET", &next_path, Value::Null)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            &format!("{next_path}&category_id=news"),
            Value::Null
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let (_, categories) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v2/iptv/live/categories?limit=1",
        Value::Null,
    )
    .await;
    assert_eq!(categories["items"][0]["id"], "sports");
    let cat_cursor = categories["next_cursor"].as_str().unwrap();
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            &format!("{root}?cursor={cat_cursor}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let (_, categories) = request(
        &app,
        "member-token-1",
        "GET",
        &format!("/api/v2/iptv/live/categories?cursor={cat_cursor}"),
        Value::Null,
    )
    .await;
    assert_eq!(categories["items"][0]["id"], "news");
    app.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE provider_live_generations SET generation=8 WHERE provider_id=1",
            [],
        )
        .unwrap();
    let (status, error) = request(&app, "member-token-1", "GET", &next_path, Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error_code"], "catalog_changed");
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE providers SET enabled=0 WHERE id=1", [])
        .unwrap();
    assert_eq!(
        request(&app, "member-token-1", "GET", &next_path, Value::Null)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let (_, fallback) = request(&app, "member-token-1", "GET", root, Value::Null).await;
    assert_eq!(fallback["catalog_id"], 2);
}

#[tokio::test]
async fn raw_live_browsing_allows_paired_devices_but_not_locked_kids() {
    let app = seeded();
    app.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE auth_sessions SET kind='device' WHERE account_id=1",
            [],
        )
        .unwrap();
    let path = "/api/v2/iptv/live/channels";
    // Account two has no selected profile: catalog access cannot bypass that.
    assert_eq!(
        request(&app, "member-token-2", "GET", path, Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&app, "member-token-1", "GET", path, Value::Null)
            .await
            .0,
        StatusCode::OK
    );
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO kids_profiles(profile_id,enabled) VALUES(1,1)",
            [],
        )
        .unwrap();
    let (status, error) = request(&app, "member-token-1", "GET", path, Value::Null).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error["error_code"], "parent_required");
}

#[tokio::test]
async fn live_personal_subsets_bind_selected_profile_and_default_catalog_without_counts() {
    let app = seeded();
    {
        let db = app.db.lock().unwrap();
        for provider in [1, 2, 3] {
            for index in 0..4 {
                db.execute("INSERT INTO provider_live(id,provider_id,stream_id,name,ordinal) VALUES(?1,?2,?3,?4,?5)",params![format!("iptv:{provider}:{index}"),provider,index.to_string(),format!("Channel {index}"),index]).unwrap();
            }
        }
        for channel in [
            "iptv:1:1",
            "iptv:1:3",
            "iptv:2:0",
            "iptv:3:0",
            "family:removed",
        ] {
            db.execute(
                "INSERT INTO favorites(profile_id,id,type,name) VALUES(1,?1,'live','Saved')",
                [channel],
            )
            .unwrap();
        }
        db.execute("INSERT INTO progress(profile_id,id,type,name,position,duration,updated_at) VALUES(1,'iptv:1:2','live','Recent',0,0,1)",[]).unwrap();
        db.execute(
            "INSERT INTO profiles(id,name,presentation_complete) VALUES(20,'Second',1)",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO profile_owners(account_id,profile_id,created_at) VALUES(1,20,0)",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO favorites(profile_id,id,type,name) VALUES(20,'iptv:1:0','live','Other profile')",[]).unwrap();
    }
    let root = "/api/v2/iptv/live/channels?collection=favorites&limit=1";
    let (status, first) = request(&app, "member-token-1", "GET", root, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["items"][0]["id"], "iptv:1:1");
    assert!(first.get("total").is_none());
    let cursor = first["next_cursor"].as_str().unwrap();
    let path = format!("{root}&cursor={cursor}");
    let (_, second) = request(&app, "member-token-1", "GET", &path, Value::Null).await;
    assert_eq!(second["items"][0]["id"], "iptv:1:3");
    assert!(second["next_cursor"].is_null());
    let (_, recent) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v2/iptv/live/channels?collection=recent",
        Value::Null,
    )
    .await;
    assert_eq!(recent["items"][0]["id"], "iptv:1:2");
    let (_, overridden) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v2/iptv/live/channels?collection=favorites&catalog_id=2",
        Value::Null,
    )
    .await;
    assert_eq!(overridden["items"][0]["id"], "iptv:2:0");
    app.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE auth_sessions SET profile_id=20 WHERE account_id=1",
            [],
        )
        .unwrap();
    let (status, error) = request(&app, "member-token-1", "GET", &path, Value::Null).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(error["error_code"], "invalid_cursor");
    let (_, own) = request(&app, "member-token-1", "GET", root, Value::Null).await;
    assert_eq!(own["items"][0]["id"], "iptv:1:0");
    for suffix in [
        "collection=us",
        "collection=family",
        "collection=favorites&profile_id=1",
    ] {
        assert_eq!(
            request(
                &app,
                "member-token-1",
                "GET",
                &format!("/api/v2/iptv/live/channels?{suffix}"),
                Value::Null
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
}

#[tokio::test]
async fn empty_account_live_catalog_has_no_implicit_global_fallback() {
    let app = seeded();
    app.db
        .lock()
        .unwrap()
        .execute("DELETE FROM provider_ownership WHERE account_id=1", [])
        .unwrap();
    for path in ["/api/v2/iptv/live/channels", "/api/v2/iptv/live/categories"] {
        let (status, page) = request(&app, "member-token-1", "GET", path, Value::Null).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            page,
            json!({"catalog_id":null,"generation":null,"items":[],"next_cursor":null})
        );
    }
}
#[tokio::test]
async fn v2_discovery_uses_three_owned_providers_not_live_default_or_foreign_sources() {
    use axum::{routing::get, Router};
    let app = seeded();
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let observed = calls.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, Router::new().route("/player_api.php",get(move |axum::extract::Query(q):axum::extract::Query<std::collections::HashMap<String,String>>| {
            observed.lock().unwrap().push(q["username"].clone());
            assert_eq!(q["action"],"get_series_info");
            async {axum::Json(json!({"episodes":{"1":[{"id":9,"season":1,"episode_num":9},{"id":2,"season":1,"episode_num":2,"container_extension":"mp4"}]}}))}
        }))).await.unwrap();
    });
    {
        let db = app.db.lock().unwrap();
        db.execute("INSERT INTO providers(id,name,url,username,password) VALUES(5,'Third owned',?1,'u5','synthetic-secret')",[&base]).unwrap();
        db.execute("INSERT INTO provider_ownership VALUES(5,1)", [])
            .unwrap();
        for provider in 1..=5 {
            db.execute(
                "UPDATE providers SET url=?1,username=?2,password='synthetic-secret' WHERE id=?3",
                params![base, format!("u{provider}"), provider],
            )
            .unwrap();
            for kind in ["movie", "series"] {
                db.execute("INSERT INTO provider_vod(id,provider_id,stream_id,kind,name,normalized,year,imdb_id,extension) VALUES(?1,?2,'10',?3,'Exact Title','exact title',2020,'tt1234567','mp4')",params![format!("iptv:{provider}:{kind}:10"),provider,kind]).unwrap();
            }
        }
        super::v2::set_live_default(&db, 1, 2).unwrap();
    }
    async fn finish(app: &App, value: Value) -> (String, Value) {
        let (status, start) =
            request(app, "member-token-1", "POST", "/api/v2/streams", value).await;
        assert_eq!(status, StatusCode::OK, "{start}");
        let path = format!("/api/v2/streams/{}", start["id"].as_str().unwrap());
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let (status, result) =
                    request(app, "member-token-1", "GET", &path, Value::Null).await;
                assert_eq!(status, StatusCode::OK, "{result}");
                if result["done"] == true {
                    break result;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        (path, result)
    }
    for kind in ["movie", "series"] {
        let (path,result)=finish(&app,json!({"type":kind,"id":if kind=="series" {"tt1234567:1:2"} else {"tt1234567"},"name":"Exact Title","year":2020,"imdb_id":"tt1234567","tmdb_id":"123"})).await;
        {
            let db = app.db.lock().unwrap();
            db.execute_batch("INSERT OR IGNORE INTO profiles(id,name,avatar_seed,presentation_complete) VALUES(2,'Second','second',1);
                INSERT OR IGNORE INTO profile_owners(profile_id,account_id,created_at) VALUES(2,2,0);
                INSERT OR IGNORE INTO auth_profiles VALUES(2,2);
                UPDATE auth_sessions SET profile_id=2 WHERE account_id=2;").unwrap();
        }
        assert_eq!(
            request(&app, "member-token-2", "GET", &path, Value::Null)
                .await
                .0,
            StatusCode::NOT_FOUND
        );
        let mut sources = result["events"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|e| e["streams"].as_array().unwrap())
            .map(|s| s["source_addon_id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        sources.sort();
        assert_eq!(sources, vec!["iptv:1", "iptv:2", "iptv:5"], "{result}");
        assert!(!result.to_string().contains("synthetic-secret"));
        assert!(!result.to_string().contains(&base));
        if kind == "series" {
            let streams = app.streams.lock().unwrap();
            for event in result["events"].as_array().unwrap() {
                for public in event["streams"].as_array().unwrap() {
                    let entry = streams.get(public["id"].as_str().unwrap()).unwrap();
                    assert!(entry.url.ends_with("/2.mp4"));
                }
            }
        }
        // Revocation after publication strips cached metadata while preserving seq.
        app.db
            .lock()
            .unwrap()
            .execute("DELETE FROM provider_ownership WHERE provider_id=5", [])
            .unwrap();
        let (_, revoked) = request(&app, "member-token-1", "GET", &path, Value::Null).await;
        assert!(!revoked.to_string().contains("iptv:5"));
        assert!(revoked.to_string().contains("source_not_found"));
        app.db
            .lock()
            .unwrap()
            .execute("INSERT INTO provider_ownership VALUES(5,1)", [])
            .unwrap();
    }
    let mut actual = calls.lock().unwrap().clone();
    actual.sort();
    assert_eq!(actual, vec!["u1", "u2", "u5"]);
    // Explicit provider narrowing cannot turn into an ownership grant.
    let (_, foreign) = finish(
        &app,
        json!({"type":"movie","id":"tt1234567","only_provider_id":3}),
    )
    .await;
    assert!(foreign["events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["streams"].as_array().unwrap().is_empty()));
    server.abort();
}
#[tokio::test]
async fn v2_discovery_revocation_during_series_fetch_does_not_publish_or_cache() {
    use axum::{routing::get, Router};
    let app = seeded();
    let entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    let entering = entered.clone();
    let releasing = release.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/player_api.php",
                get(move || {
                    let entered = entering.clone();
                    let release = releasing.clone();
                    async move {
                        entered.notify_one();
                        release.notified().await;
                        axum::Json(json!({"episodes":{"1":[{"id":2,"season":1,"episode_num":2}]}}))
                    }
                }),
            ),
        )
        .await
        .unwrap();
    });
    {
        let db = app.db.lock().unwrap();
        db.execute("UPDATE providers SET url=?1 WHERE id=1", [base])
            .unwrap();
        db.execute("INSERT INTO provider_vod(id,provider_id,stream_id,kind,name,normalized,year,imdb_id,extension) VALUES('series1',1,'1','series','Exact Title','exact title',2020,'tt1234567','mp4')",[]).unwrap();
    }
    let (status, start) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/streams",
        json!({"type":"series","id":"tt1234567:1:2","only_provider_id":1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    app.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE provider_ownership SET account_id=2 WHERE provider_id=1",
            [],
        )
        .unwrap();
    release.notify_one();
    let path = format!("/api/v2/streams/{}", start["id"].as_str().unwrap());
    let result = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let (status, value) = request(&app, "member-token-1", "GET", &path, Value::Null).await;
            assert_eq!(status, StatusCode::OK);
            if value["done"] == true {
                break value;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!result.to_string().contains("iptv:1"));
    assert!(result["events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["streams"].as_array().unwrap().is_empty()));
    assert!(app.streams.lock().unwrap().is_empty());
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM provider_cache", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    server.abort();
}

#[tokio::test]
async fn v2_guide_uses_owned_raw_channel_and_hides_foreign_or_missing_ids() {
    let app = seeded();
    {
        let db = app.db.lock().unwrap();
        for provider in 1..=4 {
            db.execute("INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES(?1,?2,'1','Raw channel')",params![format!("iptv:{provider}:1"),provider]).unwrap();
            db.execute("INSERT INTO provider_cache VALUES(?1,'get_short_epg:1',?2,?3)",params![provider,util::now()+60,json!({"epg_listings":[{"title":"VGVzdA==","start_timestamp":"1700000000","stop_timestamp":"1700003600"}]}).to_string()]).unwrap();
        }
    }
    let (status, guide) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v2/iptv/guide/iptv:1:1",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{guide}");
    assert_eq!(guide["programs"][0]["title"], "Test");
    let (status, start) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/streams",
        json!({"type":"live","id":"iptv:1:1"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{start}");
    let path = format!("/api/v2/streams/{}", start["id"].as_str().unwrap());
    let result = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let (_, result) = request(&app, "member-token-1", "GET", &path, Value::Null).await;
            if result["done"] == true {
                break result;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let cards = result["events"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|e| e["streams"].as_array().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(cards.len(), 1, "{result}");
    let source = cards[0]["id"].as_str().unwrap();
    {
        let streams = app.streams.lock().unwrap();
        let entry = streams.get(source).unwrap();
        assert_eq!(entry.provider_id, Some(1));
        assert!(entry.live);
        assert!(entry.url.starts_with("http://"));
        assert!(entry.url.ends_with("/1.ts"));
    }
    let mut denied = vec![];
    for id in ["iptv:3:1", "iptv:4:1", "iptv:999:1", "family:1"] {
        denied.push(
            request(
                &app,
                "member-token-1",
                "GET",
                &format!("/api/v2/iptv/guide/{id}"),
                Value::Null,
            )
            .await,
        );
    }
    assert!(denied.iter().all(|v| v == &denied[0]));
    assert_eq!(denied[0].0, StatusCode::NOT_FOUND);
    assert_eq!(denied[0].1["error_code"], "source_not_found");
}

#[tokio::test]
async fn exact_live_source_is_owned_private_and_independent_of_addon_discovery() {
    let app = seeded();
    {
        let db = app.db.lock().unwrap();
        for provider in 1..=4 {
            db.execute("INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES(?1,?2,'7','Selected channel')",params![format!("iptv:{provider}:7"),provider]).unwrap();
        }
        db.execute(
            "UPDATE auth_sessions SET kind='device' WHERE account_id=1",
            [],
        )
        .unwrap();
    }
    let (status, response) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/iptv/live/iptv:1:7/source",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let id = response["source"]["id"].as_str().unwrap();
    assert_eq!(response["source"]["source_addon_id"], "iptv:1");
    assert!(!response.to_string().contains("private-password"));
    assert!(!response.to_string().contains("private-user"));
    assert!(response["source"]["url"].is_null());
    assert!(
        app.jobs.lock().unwrap().is_empty(),
        "Exact live selection must not fan out discovery jobs"
    );
    {
        let streams = app.streams.lock().unwrap();
        let entry = streams.get(id).unwrap();
        assert!(entry.live);
        assert_eq!(entry.provider_id, Some(1));
        assert_eq!(entry.kind, "live");
        assert!(entry.url.ends_with("/7.ts"));
        assert!(entry.url.starts_with("http://"));
    }
    let playback = json!({"request_id":"exact_live","stream_id":id,"client":{"platform":"android","can_play_direct":true,"max_width":3840,"max_height":2160,"video_codecs":["h264"],"audio_codecs":["aac"]}});
    let (status, started) =
        request(&app, "member-token-1", "POST", "/api/v2/playback", playback).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{started}");
    let playback_id = started["id"].as_str().unwrap();
    let (status, ready) = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let result = request(
                &app,
                "member-token-1",
                "GET",
                &format!("/api/v2/playback/{playback_id}"),
                Value::Null,
            )
            .await;
            if result.1["status"] != "starting" {
                break result;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(status, StatusCode::OK, "{ready}");
    assert_eq!(ready["delivery"]["kind"], "direct");
    assert_eq!(ready["delivery"]["live"], true);
    assert!(ready["delivery"]["url"]
        .as_str()
        .unwrap()
        .ends_with("/7.ts"));
    let (status, _) = request(
        &app,
        "member-token-1",
        "DELETE",
        &format!("/api/v2/playback/{playback_id}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let mut denied = vec![];
    for channel in ["iptv:3:7", "iptv:4:7", "iptv:999:7", "family:7"] {
        denied.push(
            request(
                &app,
                "member-token-1",
                "POST",
                &format!("/api/v2/iptv/live/{channel}/source"),
                Value::Null,
            )
            .await,
        );
    }
    assert!(denied.iter().all(|value| value == &denied[0]));
    assert_eq!(denied[0].0, StatusCode::NOT_FOUND);
    assert_eq!(denied[0].1["error_code"], "source_not_found");
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE providers SET enable_live=0 WHERE id=1", [])
        .unwrap();
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/v2/iptv/live/iptv:1:7/source",
            Value::Null
        )
        .await,
        denied[0]
    );
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO kids_profiles(profile_id,enabled) VALUES(1,1)",
            [],
        )
        .unwrap();
    let (status, error) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/iptv/live/iptv:2:7/source",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error["error_code"], "parent_required");
}

#[tokio::test]
async fn live_registration_does_not_stamp_old_urls_with_updated_credentials() {
    let app = seeded();
    let original = crate::sources::source_configuration(&app.db.lock().unwrap(), "iptv:1")
        .unwrap()
        .unwrap();
    app.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE providers SET password='replaced-private' WHERE id=1",
            [],
        )
        .unwrap();
    let (sources, error) = app.register_with_configuration(
        "iptv:1",
        vec![json!({"url":"http://fixture.invalid/live/private-user/private-password/7.ts"})],
        "live",
        Some(original),
    );
    assert!(sources.is_empty());
    assert_eq!(error.as_deref(), Some("source_configuration_changed"));
    assert!(app.streams.lock().unwrap().is_empty());
}
#[tokio::test]
async fn approved_kids_vod_uses_v2_without_unlock_and_cannot_smuggle_other_titles() {
    let app = seeded();
    {
        let db = app.db.lock().unwrap();
        db.execute(
            "INSERT INTO kids_profiles(profile_id,enabled,max_age) VALUES(1,1,12)",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO kids_media(account_id,kind,id,parent_id,metadata,age,updated_at) VALUES(1,'movie','tt1234567','tt1234567',?1,5,?2)",params![json!({"id":"tt1234567","type":"movie","name":"Approved","year":2020,"imdb_id":"tt1234567","tmdb_id":"123"}).to_string(),util::now()]).unwrap();
        db.execute("INSERT INTO provider_vod(id,provider_id,stream_id,kind,name,normalized,year,imdb_id,extension) VALUES('approved',1,'9','movie','Approved','approved',2020,'tt1234567','mp4'),('adult',1,'10','movie','Adult','adult',2020,'tt9999999','mp4')",[]).unwrap();
    }
    let (status,started)=request(&app,"member-token-1","POST","/api/v2/streams",json!({"type":"movie","id":"tt1234567","name":"Adult","imdb_id":"tt9999999","only_provider_id":3})).await;
    assert_eq!(status, StatusCode::OK, "{started}");
    let path = format!("/api/v2/streams/{}", started["id"].as_str().unwrap());
    let result = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let (status, value) = request(&app, "member-token-1", "GET", &path, Value::Null).await;
            assert_eq!(status, StatusCode::OK, "{value}");
            if value["done"] == true {
                break value;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let sources = result["events"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|e| e["streams"].as_array().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(sources.len(), 1, "{result}");
    assert!(app
        .streams
        .lock()
        .unwrap()
        .get(sources[0]["id"].as_str().unwrap())
        .unwrap()
        .url
        .ends_with("/9.mp4"));
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/v2/streams",
            json!({"type":"movie","id":"tt9999999"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    app.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE kids_profiles SET revision=revision+1 WHERE profile_id=1",
            [],
        )
        .unwrap();
    assert_eq!(
        request(&app, "member-token-1", "GET", &path, Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}
