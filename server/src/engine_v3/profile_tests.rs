use crate::{auth_integration_tests::fixture, test_support::request, *};
#[tokio::test]
async fn typed_profile_crud_preserves_ownership_avatar_and_primary_protection() {
    let app = fixture();
    let (status, list) =
        request(&app, "member-token-1", "GET", "/api/v3/profiles", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(
        list["capabilities"]["profile_pin"]["availability"],
        "Unsupported"
    );
    assert_eq!(list["profiles"][0]["locked"], false);
    let (status, profile) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v3/profiles",
        json!({"name":"Typed profile","avatar_id":"zayne"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{profile}");
    let id = profile["id"].as_str().unwrap();
    assert_eq!(profile["avatar_id"], "zayne");
    assert_eq!(profile["can_delete"], true);
    let path = format!("/api/v3/profiles/{id}");
    let (status, renamed) = request(
        &app,
        "member-token-1",
        "PATCH",
        &path,
        json!({"name":"Renamed"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{renamed}");
    assert_eq!(renamed["avatar_id"], "zayne");
    for method in ["PATCH", "DELETE"] {
        assert_eq!(
            request(
                &app,
                "member-token-2",
                method,
                &path,
                json!({"name":"Intruder"})
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            &format!("{path}/pin"),
            json!({"pin":"1234"})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let (_, pin) = request(
        &app,
        "member-token-1",
        "GET",
        &format!("{path}/pin"),
        json!({}),
    )
    .await;
    assert_eq!(pin["availability"], "Unsupported");
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "DELETE",
            "/api/v3/profiles/1",
            json!({})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(&app, "member-token-1", "DELETE", &path, json!({}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM engine_profile_presentation WHERE profile_id=?1",
                [id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}
#[tokio::test]
async fn restricted_profile_management_uses_existing_account_parent_grant() {
    let app = fixture();
    let (_, other) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v3/profiles",
        json!({"name":"Other profile"}),
    )
    .await;
    let other_id = other["id"].as_str().unwrap();
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/v3/parent/pin",
            json!({"pin":"2468"})
        )
        .await
        .0,
        StatusCode::OK
    );
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO kids_profiles(profile_id,enabled,max_age,revision) VALUES(1,1,12,1)",
            [],
        )
        .unwrap();
    let (status, list) =
        request(&app, "member-token-1", "GET", "/api/v3/profiles", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(
        list["capabilities"]["create"]["availability"],
        "NeedsAuthorization"
    );
    let target = list["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == other_id)
        .unwrap();
    assert_eq!(target["locked"], true);
    assert_eq!(target["lock_kind"], "parent");
    assert_eq!(target["selection_requires_parent"], true);
    let (status, bootstrap) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v3/bootstrap",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{bootstrap}");
    assert_eq!(bootstrap["profile_management"], list);
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/v3/profiles",
            json!({"name":"Blocked"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, unlock) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v3/parent/unlock",
        json!({"pin":"2468"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{unlock}");
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/v3/profiles",
            json!({"name":"Allowed","avatar_id":"initial-blue"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/v3/profiles",
            json!({"name":"Unknown avatar","avatar_id":"invalid"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}
