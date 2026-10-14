use crate::{auth_integration_tests::fixture, test_support::request, *};

#[test]
fn remote_projection_preserves_episode_identity_and_keeps_progress_in_library() {
    let value = json!({"id":"tt123:2:4","type":"series","name":"Episode","season":2,"episode":4,"position":30,"duration":60});
    let provider = super::projection::card(&value, "addon:1").unwrap();
    let episode = provider.identity.episode.unwrap();
    assert_eq!(episode.season, Some(2));
    assert_eq!(episode.episode, 4);
    assert_eq!(provider.progress, None);
    assert_eq!(
        super::projection::card(&value, "library").unwrap().progress,
        Some(0.5)
    );
    let oversized = json!({"id":"x".repeat(513),"type":"movie","name":"Invalid"});
    assert!(super::projection::card(&oversized, "addon:1").is_none());
}

#[tokio::test]
async fn detail_rejects_a_provider_response_for_a_different_title() {
    let mock = axum::Router::new().route(
        "/meta/movie/tt123.json",
        axum::routing::get(|| async {
            axum::Json(json!({"meta":{"id":"tt999","type":"movie","name":"Wrong title"}}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let mut app = fixture();
    app.addons.allow_test_loopback = true;
    let manifest =
        json!({"id":"test","name":"Test","version":"1","resources":["meta"],"types":["movie"]});
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO addons(name,manifest_url,manifest,account_id) VALUES('Test',?1,?2,1)",
            params![
                format!("http://{address}/manifest.json"),
                manifest.to_string()
            ],
        )
        .unwrap();
    crate::test_support::encrypt_fixture_sources(&app);
    let (status, body) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v3/meta/movie/tt123",
        json!({}),
    )
    .await;
    server.abort();
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert!(!body.to_string().contains("Wrong title"));
}

#[tokio::test]
async fn bootstrap_has_prepared_home_without_per_card_requests() {
    let app = fixture();
    let (identity_status, identity) =
        request(&app, "member-token-1", "GET", "/api/auth/me", json!({})).await;
    assert_eq!(identity_status, StatusCode::OK, "identity: {identity}");
    let (status, body) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v3/bootstrap",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["contract_version"], 3);
    assert_eq!(body["home"]["scope"]["profile_id"], "1");
    assert_eq!(body["home"]["rows"][0]["id"], "continue");
    assert!(body["identity"]["profiles"].is_array());
}

#[tokio::test]
async fn home_rejects_cross_profile_access() {
    let app = fixture();
    let (status, _) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v3/profiles/2/home",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn profile_configuration_is_scoped_and_rejects_stale_updates() {
    let app = fixture();
    let (status, first) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v3/profiles/1/integrations",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let update = json!({"revision":0,"browsing_plugin":"stremio","assignments":[]});
    let (status, saved) = request(
        &app,
        "member-token-1",
        "PUT",
        "/api/v3/profiles/1/integrations",
        update.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["configuration"]["scope"]["revision"], 1);
    let (status, _) = request(
        &app,
        "member-token-1",
        "PUT",
        "/api/v3/profiles/1/integrations",
        update,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = request(
        &app,
        "member-token-2",
        "GET",
        "/api/v3/profiles/1/integrations",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn paired_device_can_change_selected_profile_integrations_but_not_restricted_or_other_profiles(
) {
    let app = fixture();
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE auth_sessions SET kind='device' WHERE id='s1'", [])
        .unwrap();
    let update = json!({"revision":0,"browsing_plugin":"stremio","assignments":[]});
    let (status, body) = request(
        &app,
        "member-token-1",
        "PUT",
        "/api/v3/profiles/1/integrations",
        update.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["configuration"]["scope"]["revision"], 1);
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/v3/profiles/2/integrations",
            update.clone()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO kids_profiles(profile_id,enabled,max_age,revision) VALUES(1,1,12,1)",
            [],
        )
        .unwrap();
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/v3/profiles/1/integrations",
            json!({"revision":1,"browsing_plugin":"stremio","assignments":[]})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn bootstrap_advertises_m3u_only_with_live_runtime_and_credential_vault() {
    use base64::Engine;
    for vault_available in [false,true] {
        let mut app=fixture();
        app.secret_vault=if vault_available {Some(std::sync::Arc::new(crate::secret_store::Vault::from_json(
            &json!({"active":"test","keys":{"test":base64::engine::general_purpose::STANDARD.encode([7u8;32])}}).to_string()).unwrap()))}else{None};
        let (status,body)=request(&app,"member-token-1","GET","/api/v3/bootstrap",Value::Null).await;
        assert_eq!(status,StatusCode::OK,"{body}");
        let descriptor=body["integrations"].as_array().unwrap().iter().find(|p|p["id"]=="m3u");
        assert_eq!(descriptor.is_some(),vault_available && cfg!(feature="live-runtime"));
        if let Some(descriptor)=descriptor {
            assert_eq!(descriptor["capabilities"],json!(["LiveTv","Sources"]));
            assert!(descriptor["locations"].as_array().unwrap().contains(&json!("Server")));
            assert!(!descriptor["capabilities"].as_array().unwrap().contains(&json!("Metadata")));
        }
    }
}
