use crate::{auth_integration_tests::fixture, test_support::request, *};

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
