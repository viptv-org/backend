use crate::{auth_integration_tests::fixture, test_support::request, *};

fn seeded() -> App {
    let app = fixture();
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
