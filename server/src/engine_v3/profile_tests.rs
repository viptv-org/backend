use crate::{auth_integration_tests::fixture, test_support::request, *};
#[tokio::test]
async fn typed_profile_crud_preserves_ownership_avatar_and_primary_protection() {
    let app = fixture();
    let (status, list) =
        request(&app, "member-token-1", "GET", "/api/profiles", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(
        list["capabilities"]["profile_pin"]["availability"],
        "Available"
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
    assert_eq!(profile["avatar_style"], "critters");
    assert_eq!(profile["avatar_choice"], Value::Null);
    assert!(profile["avatar_url"].as_str().is_some_and(|url| !url.is_empty()));
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

    let (_, revisions) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/revisions",
        Value::Null,
    )
    .await;
    assert_eq!(revisions["preferences"], "1");

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

#[tokio::test]
async fn playback_controls_match_the_board_order_and_toggle_copy() {
    let app = fixture();
    let (status, snapshot) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/preferences",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let controls = snapshot["controls"].as_array().unwrap();
    let playback: Vec<_> = controls
        .iter()
        .filter(|control| control["group"] == "playback")
        .map(|control| control["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        playback,
        [
            "next_episode_button",
            "skip_intros",
            "source_selection",
            "max_quality_height",
            "hdr_preference",
            "match_frame_rate",
            "subtitle_mode"
        ]
    );
    let next = controls
        .iter()
        .find(|control| control["id"] == "next_episode_button")
        .unwrap();
    assert_eq!(next["label"], "Next episode button");
    assert_eq!(
        next["description"],
        "Shows during the credits. Nothing ever plays on its own."
    );
    assert_eq!(next["choices"][0]["value"], true);
    let start_subtitles = controls
        .iter()
        .find(|control| control["id"] == "subtitle_mode")
        .unwrap();
    assert_eq!(start_subtitles["label"], "Start with subtitles");
    assert_eq!(start_subtitles["choices"][0]["value"], "always");
    assert_eq!(start_subtitles["choices"][1]["value"], "never");
    assert!(
        !controls
            .iter()
            .any(|control| control["group"] == "subtitles" && control["id"] == "subtitle_mode")
    );
    let prefs = &snapshot["preferences"];
    assert_eq!(prefs["next_episode_button"], true);
    assert_eq!(prefs["skip_intros"], false);
    assert_eq!(prefs["match_frame_rate"], true);
}

fn profile_card(list: &Value, id: &str) -> Value {
    list["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == id)
        .cloned()
        .unwrap_or_else(|| panic!("profile {id} missing from {list}"))
}

#[tokio::test]
async fn engine_catalog_choice_round_trips_to_the_account_api() {
    let app = fixture();
    let (status, created) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/profiles",
        json!({"name":"Catalog","avatar_style":"disney","avatar_choice":3}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let id = created["id"].as_str().unwrap().to_owned();
    let disney = auth::character_avatars()["disney"][2]["url"].clone();
    assert_eq!(created["avatar_style"], "disney");
    assert_eq!(created["avatar_choice"], 3);
    assert_eq!(created["avatar_url"], disney);
    assert_eq!(created["avatar_id"], Value::Null);
    let (_, me) = request(&app, "member-token-1", "GET", "/api/auth/me", Value::Null).await;
    let account = profile_card(&me, &id);
    assert_eq!(account["avatar_style"], "disney");
    assert_eq!(account["avatar_choice"], 3);
    assert_eq!(account["avatar_url"], disney);

    let (status, updated) = request(
        &app,
        "member-token-1",
        "PATCH",
        &format!("/api/profiles/{id}"),
        json!({"avatar_style":"moods","avatar_choice":48}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["name"], "Catalog");
    assert_eq!(updated["avatar_style"], "moods");
    assert_eq!(updated["avatar_choice"], 48);
    let moods = "https://api.dicebear.com/10.x/moods/png?seed=viptv-moods-48&size=256";
    assert_eq!(updated["avatar_url"], moods);
    let (status, updated) = request(
        &app,
        "member-token-1",
        "PATCH",
        &format!("/api/profiles/{id}"),
        json!({"avatar_choice":7}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["avatar_style"], "moods");
    assert_eq!(updated["avatar_choice"], 7);
    let (_, list) = request(&app, "member-token-1", "GET", "/api/profiles", Value::Null).await;
    assert_eq!(profile_card(&list, &id), updated);
    let (_, bootstrap) =
        request(&app, "member-token-1", "GET", "/api/bootstrap", Value::Null).await;
    assert_eq!(profile_card(&bootstrap["profile_management"], &id), updated);
    assert_eq!(
        profile_card(&bootstrap["identity"], &id)["avatar_choice"],
        7
    );
}

#[tokio::test]
async fn account_catalog_choice_projects_on_the_engine_route_and_outranks_legacy_ids() {
    let app = fixture();
    let (status, legacy) = request(
        &app,
        "member-token-1",
        "PATCH",
        "/api/profiles/1",
        json!({"avatar_id":"vynxc"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{legacy}");
    assert_eq!(legacy["avatar_id"], "vynxc");
    assert_eq!(legacy["avatar_style"], "critters");
    assert_eq!(legacy["avatar_choice"], Value::Null);
    assert!(legacy["avatar_url"].as_str().is_some_and(|u| !u.is_empty()));

    auth::update_profile(
        &app.db.lock().unwrap(),
        1,
        1,
        &json!({"avatar_style":"princesses","avatar_choice":2}),
    )
    .unwrap();
    let princess = auth::character_avatars()["princesses"][1]["url"].clone();
    let (_, list) = request(&app, "member-token-1", "GET", "/api/profiles", Value::Null).await;
    let card = profile_card(&list, "1");
    assert_eq!(card["avatar_style"], "princesses");
    assert_eq!(card["avatar_choice"], 2);
    assert_eq!(card["avatar_url"], princess);
    assert_eq!(card["avatar_id"], Value::Null, "catalog choice wins");
    let (_, config) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/integrations",
        Value::Null,
    )
    .await;
    assert_eq!(config["configuration"]["avatar_id"], Value::Null);

    for body in [json!({"avatar_id":"zayne"}), json!({"name":"Renamed"})] {
        let (status, card) =
            request(&app, "member-token-1", "PATCH", "/api/profiles/1", body.clone()).await;
        assert_eq!(status, StatusCode::OK, "{body}: {card}");
        assert_eq!(card["avatar_id"], Value::Null, "{body}");
        assert_eq!(card["avatar_style"], "princesses", "{body}");
        assert_eq!(card["avatar_choice"], 2, "{body}");
    }

    let stored = || -> Option<String> {
        app.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT avatar_id FROM engine_profile_presentation WHERE profile_id=1",
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!(stored().as_deref(), Some("zayne"));

    let (status, catalog) = request(
        &app,
        "member-token-1",
        "PATCH",
        "/api/profiles/1",
        json!({"avatar_style":"villains","avatar_choice":12}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{catalog}");
    assert_eq!(catalog["avatar_id"], Value::Null);
    assert_eq!(catalog["avatar_choice"], 12);
    assert_eq!(stored(), None);
}

#[tokio::test]
async fn engine_profile_route_rejects_invalid_catalog_choices() {
    let app = fixture();
    let (_, before) = request(&app, "member-token-1", "GET", "/api/profiles", Value::Null).await;
    for body in [
        json!({"avatar_style":"unknown-world","avatar_choice":1}),
        json!({"avatar_style":42}),
        json!({"avatar_style":null}),
        json!({"avatar_choice":0}),
        json!({"avatar_choice":49}),
        json!({"avatar_choice":"3"}),
        json!({"avatar_choice":null}),
        json!({"avatar_style":"princesses","avatar_choice":11}),
        json!({"avatar_id":"vynxc","avatar_style":"moods","avatar_choice":1}),
        json!({"name":"Extra","avatar_style":"moods","avatar_choice":1,"setup_complete":true}),
    ] {
        let (status, error) =
            request(&app, "member-token-1", "PATCH", "/api/profiles/1", body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {error}");
        let mut create = body.clone();
        create["name"] = json!("Rejected");
        let (status, error) =
            request(&app, "member-token-1", "POST", "/api/profiles", create.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{create}: {error}");
    }
    let (_, after) = request(&app, "member-token-1", "GET", "/api/profiles", Value::Null).await;
    assert_eq!(before, after);
}

#[tokio::test]
async fn profile_configuration_projects_the_account_catalog_avatar() {
    let app = fixture();
    let configuration = || async {
        let (status, body) = request(
            &app,
            "member-token-1",
            "GET",
            "/api/profiles/1/integrations",
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["configuration"].clone()
    };
    let avatar = |config: &Value| {
        (
            config["avatar_id"].clone(),
            config["avatar_style"].clone(),
            config["avatar_choice"].clone(),
        )
    };
    assert_eq!(
        avatar(&configuration().await),
        (Value::Null, json!("critters"), Value::Null)
    );

    let (status, body) = request(
        &app,
        "member-token-1",
        "PATCH",
        "/api/profiles/1",
        json!({"avatar_id":"vynxc"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        avatar(&configuration().await),
        (json!("vynxc"), json!("critters"), Value::Null)
    );

    auth::update_profile(
        &app.db.lock().unwrap(),
        1,
        1,
        &json!({"avatar_style":"princesses","avatar_choice":2}),
    )
    .unwrap();
    assert_eq!(
        avatar(&configuration().await),
        (Value::Null, json!("princesses"), json!(2))
    );

    let (status, body) = request(
        &app,
        "member-token-1",
        "PUT",
        "/api/profiles/1/integrations",
        json!({"revision":0,"browsing_plugin":"stremio","assignments":[]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        avatar(&body["configuration"]),
        (Value::Null, json!("princesses"), json!(2))
    );
    auth::update_profile(
        &app.db.lock().unwrap(),
        1,
        1,
        &json!({"avatar_style":"moods","avatar_choice":48}),
    )
    .unwrap();
    assert_eq!(
        avatar(&configuration().await),
        (Value::Null, json!("moods"), json!(48))
    );
}

#[tokio::test]
async fn configurations_stored_before_the_avatar_catalog_keep_loading() {
    let app = fixture();
    auth::update_profile(
        &app.db.lock().unwrap(),
        1,
        1,
        &json!({"avatar_style":"villains","avatar_choice":12}),
    )
    .unwrap();
    let legacy = json!({
        "scope":{"connection_id":"server","account_id":"1","profile_id":"1","revision":1},
        "mode":"Server",
        "name":"Stale name",
        "avatar_id":null,
        "browsing_plugin":"stremio",
        "assignments":[]
    });
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO engine_profile_configuration VALUES(1,1,1,?1)",
            [legacy.to_string()],
        )
        .unwrap();
    let (status, body) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/integrations",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let configuration = &body["configuration"];
    assert_eq!(configuration["scope"]["revision"], 1);
    assert_eq!(configuration["avatar_style"], "villains");
    assert_eq!(configuration["avatar_choice"], 12);
    assert_ne!(configuration["name"], "Stale name");
}
