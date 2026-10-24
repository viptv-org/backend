use crate::{auth_integration_tests::fixture, test_support::request, *};
#[tokio::test]
async fn typed_profile_crud_preserves_ownership_avatar_and_primary_protection() {
    let app = fixture();
    let (status, list) =
        request(&app, "member-token-1", "GET", "/api/profiles", json!({})).await;
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
        "/api/profiles",
        json!({"name":"Typed profile","avatar_id":"zayne"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{profile}");
    let id = profile["id"].as_str().unwrap();
    assert_eq!(profile["avatar_id"], "zayne");
    assert_eq!(profile["can_delete"], true);
    let path = format!("/api/profiles/{id}");
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
            "/api/profiles/1",
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
        "/api/profiles",
        json!({"name":"Other profile"}),
    )
    .await;
    let other_id = other["id"].as_str().unwrap();
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/parent/pin",
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
        request(&app, "member-token-1", "GET", "/api/profiles", json!({})).await;
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
        "/api/bootstrap",
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
            "/api/profiles",
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
        "/api/parent/unlock",
        json!({"pin":"2468"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{unlock}");
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/profiles",
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
            "/api/profiles",
            json!({"name":"Unknown avatar","avatar_id":"invalid"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}
#[tokio::test]
async fn profile_preferences_are_private_independently_versioned_and_reject_stale_edits() {
    let app = fixture();
    let (status, defaults) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/preferences",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(defaults["revision"], 0);
    let mut prefs = defaults["preferences"].clone();
    prefs["preferred_audio_language"] = json!("en-US");
    prefs["max_quality_height"] = json!(1080);
    let (status, saved) = request(
        &app,
        "member-token-1",
        "PUT",
        "/api/profiles/1/preferences",
        json!({"revision":0,"preferences":prefs}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["revision"], 1);
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/profiles/1/preferences",
            json!({"revision":0,"preferences":prefs})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(
            &app,
            "member-token-2",
            "GET",
            "/api/profiles/1/preferences",
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn profile_management_exposes_scalar_kids_and_the_actual_age_ceiling() {
    let app = fixture();
    let (status, normal) =
        request(&app, "member-token-1", "GET", "/api/profiles", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(normal["profiles"][0]["kids"], false);
    assert_eq!(normal["profiles"][0]["max_age"], 12);
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO kids_profiles(profile_id,enabled,max_age,revision)VALUES(1,1,7,1)",
            [],
        )
        .unwrap();
    let (status, kids) =
        request(&app, "member-token-1", "GET", "/api/profiles", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{kids}");
    assert_eq!(kids["profiles"][0]["kids"], true);
    assert_eq!(kids["profiles"][0]["max_age"], 7);
    assert!(kids["profiles"][0]["kids"].is_boolean());
}

#[tokio::test]
async fn display_preferences_project_controls_persist_and_validate() {
    let app = fixture();
    let (status, defaults) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/preferences",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let display = &defaults["display"];
    assert_eq!(display["accent_color"], "#F5C542");
    assert_eq!(display["show_ratings"], true);
    assert_eq!(display["text_size"], "default");
    assert_eq!(display["reduce_motion"], "follow");
    assert_eq!(display["screen_saver_minutes"], 5);
    assert_eq!(display["screen_inset"], 0);
    let controls = defaults["controls"].as_array().unwrap();
    let display_controls: Vec<_> = controls
        .iter()
        .filter(|control| control["group"] == "display")
        .collect();
    assert_eq!(display_controls.len(), 6);
    let accent = display_controls
        .iter()
        .find(|control| control["id"] == "accent_color")
        .unwrap();
    assert_eq!(accent["label"], "Accent colour");
    assert_eq!(accent["description"], "Progress bars, marks and toggles");
    assert_eq!(accent["kind"], "swatches");
    assert_eq!(accent["choices"][0]["value"], "#F5C542");
    let inset = display_controls
        .iter()
        .find(|control| control["id"] == "screen_inset")
        .unwrap();
    assert_eq!(inset["label"], "Fit to screen");
    assert_eq!(inset["kind"], "calibration");
    assert_eq!(inset["max"], 20);

    // A display-only update persists, bumps the shared revision and keeps the
    // playback preferences untouched.
    let mut display = display.clone();
    display["accent_color"] = json!("#62D9BC");
    display["screen_saver_minutes"] = json!(10);
    display["screen_inset"] = json!(4);
    display["reduce_motion"] = json!("on");
    let (status, saved) = request(
        &app,
        "member-token-1",
        "PUT",
        "/api/profiles/1/preferences",
        json!({"revision":0,"display":display}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["revision"], 1);
    assert_eq!(saved["display"]["accent_color"], "#62D9BC");
    assert_eq!(saved["preferences"]["subtitle_mode"], "always");

    let (status, reread) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/preferences",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reread["revision"], 1);
    assert_eq!(reread["display"]["screen_saver_minutes"], 10);
    assert_eq!(reread["display"]["screen_inset"], 4);

    // The revisions channel moves on a display-only update so other devices
    // refresh their display settings without a playback-preference change.
    let (_, revisions) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/revisions",
        Value::Null,
    )
    .await;
    assert_eq!(revisions["preferences"], "1");

    // A playback-only update keeps the saved display values.
    let mut prefs = reread["preferences"].clone();
    prefs["subtitle_mode"] = json!("never");
    prefs["subtitles_enabled"] = json!(false);
    let (status, saved) = request(
        &app,
        "member-token-1",
        "PUT",
        "/api/profiles/1/preferences",
        json!({"revision":1,"preferences":prefs}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["display"]["accent_color"], "#62D9BC");

    // Stale revisions conflict on display updates too.
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/profiles/1/preferences",
            json!({"revision":0,"display":display})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn display_preferences_reject_invalid_values_and_empty_updates() {
    let app = fixture();
    let (_, defaults) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/preferences",
        Value::Null,
    )
    .await;
    let display = defaults["display"].clone();
    // Values outside the projected inventories are rejected.
    for change in [
        json!({"accent_color":"#123456"}),
        json!({"screen_saver_minutes":7}),
        json!({"screen_inset":21}),
    ] {
        let mut invalid = display.clone();
        for (key, value) in change.as_object().unwrap() {
            invalid[key] = value.clone();
        }
        assert_eq!(
            request(
                &app,
                "member-token-1",
                "PUT",
                "/api/profiles/1/preferences",
                json!({"revision":0,"display":invalid})
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    // Unknown display fields and empty updates are shape errors.
    let mut unknown = display.clone();
    unknown["theme"] = json!("dark");
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/profiles/1/preferences",
            json!({"revision":0,"display":unknown})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/profiles/1/preferences",
            json!({"revision":0})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    // Lowercase accent hex normalizes to the palette casing.
    let mut lower = display.clone();
    lower["accent_color"] = json!("#ff8b5c");
    let (status, saved) = request(
        &app,
        "member-token-1",
        "PUT",
        "/api/profiles/1/preferences",
        json!({"revision":0,"display":lower}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["display"]["accent_color"], "#FF8B5C");
}
