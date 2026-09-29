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
