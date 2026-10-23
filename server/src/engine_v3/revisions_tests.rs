use crate::{auth_integration_tests::fixture, test_support::request, *};
use tower::ServiceExt;

const KEYS: [&str; 6] = [
    "library",
    "configuration",
    "addons",
    "policy",
    "preferences",
    "profiles",
];

async fn revisions_as(app: &App, token: &str, profile: i64) -> Value {
    let (status, value) = request(
        app,
        token,
        "GET",
        &format!("/api/profiles/{profile}/revisions"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value
}
async fn revisions(app: &App) -> Value {
    revisions_as(app, "member-token-1", 1).await
}
async fn ok(app: &App, token: &str, method: &str, path: &str, body: Value) -> Value {
    let (status, value) = request(app, token, method, path, body).await;
    assert_eq!(status, StatusCode::OK, "{method} {path}: {value}");
    value
}
/// Names of the tokens that differ, in contract order.
fn changed(before: &Value, after: &Value) -> Vec<&'static str> {
    KEYS.into_iter()
        .filter(|key| before[*key] != after[*key])
        .collect()
}
fn progress(position: u64) -> Value {
    let identity = |id: &str, episode: bool| {
        json!({"id":id,"kind":"series","ids":[{"namespace":"addon","value":id}],"edition":null,
            "episode":if episode {json!({"order":"aired","season":1,"episode":2})} else {Value::Null}})
    };
    json!({"record":{"identity":identity("rev-episode",true),"parent_identity":identity("rev-parent",false),"provider_id":"stremio","title":"Episode","poster":null,"landscape":null,"position_millis":position,"duration_millis":100_000,"updated_at_millis":1,"completed_at_millis":null}})
}
fn favorite(id: &str) -> Value {
    json!({"card":{"identity":{"id":id,"kind":"movie","ids":[{"namespace":"imdb","value":"tt7654321"}],"edition":null,"episode":null},"provider_id":"stremio","title":"Saved","subtitle":null,"description":null,"poster":null,"landscape":null,"logo":null,"progress":null,"badges":[],"parent_identity":null}})
}
/// Gives account 2 its own selected profile 2.
fn attach_second_account_profile(app: &App) {
    app.db.lock().unwrap().execute_batch("INSERT INTO profiles(id,name,avatar_seed,presentation_complete) VALUES(2,'Other account','fixture-two',1);
        INSERT INTO profile_owners(profile_id,account_id,created_at) VALUES(2,2,0);
        INSERT INTO auth_profiles VALUES(2,2);
        UPDATE auth_sessions SET profile_id=2 WHERE account_id=2;").unwrap();
}

#[tokio::test]
async fn inherited_preferences_change_only_the_selected_profiles_preference_token() {
    let app = fixture();
    attach_second_account_profile(&app);
    let before = revisions(&app).await;
    let other = revisions_as(&app,"member-token-2",2).await;
    app.db.lock().unwrap().execute("INSERT INTO playback_preferences(profile_id,value) VALUES(1,?1)", [json!({"audio_language":"ja","subtitle_language":"fr","subtitles_enabled":false}).to_string()]).unwrap();
    let inherited = revisions(&app).await;
    assert_eq!(changed(&before,&inherited), vec!["preferences"]);
    assert_eq!(revisions_as(&app,"member-token-2",2).await,other);
    assert_eq!(revisions(&app).await,inherited);
    app.db.lock().unwrap().execute("UPDATE playback_preferences SET value=?1 WHERE profile_id=1", [json!({"audio_language":"ko","subtitle_language":"fr","subtitles_enabled":false}).to_string()]).unwrap();
    assert_eq!(changed(&inherited,&revisions(&app).await),vec!["preferences"]);
}

#[tokio::test]
async fn revisions_are_opaque_stable_strings_and_private() {
    let app = fixture();
    let first = revisions(&app).await;
    let object = first.as_object().unwrap();
    assert_eq!(object.len(), KEYS.len(), "{first}");
    for key in KEYS {
        assert!(first[key].is_string(), "{key}: {first}");
    }
    assert_eq!(first["library"], "0");
    assert_eq!(first["configuration"], "0");
    assert_eq!(first["policy"], "0");
    assert_eq!(first["preferences"], "0");
    for _ in 0..3 {
        assert_eq!(revisions(&app).await, first);
    }
    let catalogs = ok(
        &app,
        "member-token-1",
        "GET",
        "/api/catalogs/revision",
        Value::Null,
    )
    .await;
    assert_eq!(first["addons"], catalogs["revision"]);
    let response = router(app.clone(), None)
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/profiles/1/revisions")
                .header("authorization", "Bearer member-token-1")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
}

#[tokio::test]
async fn revisions_reject_missing_sessions_unselected_and_foreign_profiles() {
    let app = fixture();
    let get = |token: &'static str, profile: &'static str| {
        let app = app.clone();
        async move {
            request(
                &app,
                token,
                "GET",
                &format!("/api/profiles/{profile}/revisions"),
                Value::Null,
            )
            .await
        }
    };
    assert_eq!(get("invalid-token", "1").await.0, StatusCode::UNAUTHORIZED);
    // Account 2 has a session but no selected profile yet.
    let (status, body) = get("member-token-2", "1").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    attach_second_account_profile(&app);
    assert_eq!(get("member-token-2", "1").await.0, StatusCode::FORBIDDEN);
    assert_eq!(get("member-token-1", "2").await.0, StatusCode::FORBIDDEN);
    assert_eq!(get("member-token-1", "999").await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        get("member-token-1", "abc").await.0,
        StatusCode::BAD_REQUEST
    );
    // A second owned but unselected profile follows the selected-profile rule.
    let created = ok(
        &app,
        "member-token-1",
        "POST",
        "/api/profiles",
        json!({"name":"Second"}),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_owned();
    let (status, _) = request(
        &app,
        "member-token-1",
        "GET",
        &format!("/api/profiles/{id}/revisions"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(get("member-token-2", "2").await.0, StatusCode::OK);
}

#[tokio::test]
async fn library_writes_bump_only_the_library_token() {
    let app = fixture();
    let token = "member-token-1";
    let mut before = revisions(&app).await;
    for (method, path, body) in [
        ("PUT", "/api/profiles/1/progress", progress(30_000)),
        ("PUT", "/api/profiles/1/progress", progress(60_000)),
        ("PUT", "/api/profiles/1/library", favorite("rev-movie")),
        (
            "DELETE",
            "/api/profiles/1/library/movie/rev-movie",
            Value::Null,
        ),
        ("DELETE", "/api/profiles/1/history", Value::Null),
    ] {
        ok(&app, token, method, path, body).await;
        let after = revisions(&app).await;
        assert_eq!(changed(&before, &after), ["library"], "{method} {path}");
        before = after;
    }
}

#[tokio::test]
async fn configuration_preferences_and_addon_writes_bump_their_own_token() {
    let app = fixture();
    let token = "member-token-1";
    let before = revisions(&app).await;
    ok(
        &app,
        token,
        "PUT",
        "/api/profiles/1/integrations",
        json!({"revision":0,"browsing_plugin":"stremio","assignments":[]}),
    )
    .await;
    let after = revisions(&app).await;
    assert_eq!(changed(&before, &after), ["configuration"]);

    let before = after;
    let current = ok(
        &app,
        token,
        "GET",
        "/api/profiles/1/preferences",
        Value::Null,
    )
    .await;
    ok(
        &app,
        token,
        "PUT",
        "/api/profiles/1/preferences",
        json!({"revision":current["revision"],"preferences":current["preferences"]}),
    )
    .await;
    let after = revisions(&app).await;
    assert_eq!(changed(&before, &after), ["preferences"]);

    let before = after;
    let added = {
        let db = app.db.lock().unwrap();
        db.execute("INSERT INTO addons(name,manifest_url,manifest,account_id) VALUES('Rev','https://rev.invalid/manifest.json','{}',1)", []).unwrap();
        db.last_insert_rowid()
    };
    let after = revisions(&app).await;
    assert_eq!(changed(&before, &after), ["addons"]);
    let catalogs = ok(&app, token, "GET", "/api/catalogs/revision", Value::Null).await;
    assert_eq!(after["addons"], catalogs["revision"]);
    let before = after;
    app.db
        .lock()
        .unwrap()
        .execute("DELETE FROM addons WHERE id=?1", [added])
        .unwrap();
    let after = revisions(&app).await;
    assert_eq!(changed(&before, &after), ["addons"]);
}

#[tokio::test]
async fn kids_policy_and_parent_controls_bump_policy_and_profile_management() {
    let app = fixture();
    let token = "member-token-1";
    let before = revisions(&app).await;
    // The account PIN changes the advertised profile-management state only.
    ok(&app, token, "PUT", "/api/parent/pin", json!({"pin":"7248"})).await;
    let after = revisions(&app).await;
    assert_eq!(changed(&before, &after), ["profiles"]);

    // A kids setting changes this profile's policy and the profile cards
    // (kids/max_age), so both signals move.
    let before = after;
    ok(
        &app,
        token,
        "PUT",
        "/api/profiles/1/kids",
        json!({"enabled":false,"max_age":9}),
    )
    .await;
    let after = revisions(&app).await;
    assert_eq!(changed(&before, &after), ["policy", "profiles"]);

    // Title approvals change only what the kids profile sees.
    let before = after;
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO kids_profiles(profile_id,revision) VALUES(1,1) ON CONFLICT(profile_id) DO UPDATE SET revision=revision+1",
            [],
        )
        .unwrap();
    let after = revisions(&app).await;
    assert_eq!(changed(&before, &after), ["policy"]);

    // Restricting the selected profile: the restricted session can still poll
    // without a parent grant and sees the kids add-on revision.
    let before = after;
    ok(
        &app,
        token,
        "PUT",
        "/api/profiles/1/kids",
        json!({"enabled":true,"max_age":9}),
    )
    .await;
    let after = revisions(&app).await;
    assert!(changed(&before, &after).contains(&"policy"));
    assert!(changed(&before, &after).contains(&"profiles"));
    let catalogs = ok(&app, token, "GET", "/api/catalogs/revision", Value::Null).await;
    assert_eq!(after["addons"], catalogs["revision"]);

    // Unlocking the parent grant changes this session's profile locks.
    let before = after;
    ok(
        &app,
        token,
        "POST",
        "/api/parent/unlock",
        json!({"pin":"7248"}),
    )
    .await;
    let after = revisions(&app).await;
    assert_eq!(changed(&before, &after), ["profiles"]);
    assert_eq!(revisions(&app).await, after);
}

#[tokio::test]
async fn profile_management_writes_bump_only_the_profiles_token() {
    let app = fixture();
    let token = "member-token-1";
    let mut before = revisions(&app).await;
    let created = ok(
        &app,
        token,
        "POST",
        "/api/profiles",
        json!({"name":"Second","avatar_id":"vynxc"}),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_owned();
    let mut after = revisions(&app).await;
    assert_eq!(changed(&before, &after), ["profiles"], "create");
    for (method, path, body) in [
        (
            "PATCH",
            "/api/profiles/1".to_owned(),
            json!({"name":"Renamed"}),
        ),
        (
            "PATCH",
            "/api/profiles/1".to_owned(),
            json!({"avatar_id":"zayne"}),
        ),
        (
            "PATCH",
            format!("/api/profiles/{id}"),
            json!({"avatar_id":null}),
        ),
        ("DELETE", format!("/api/profiles/{id}"), Value::Null),
    ] {
        before = after;
        ok(&app, token, method, &path, body.clone()).await;
        after = revisions(&app).await;
        assert_eq!(
            changed(&before, &after),
            ["profiles"],
            "{method} {path} {body}"
        );
    }
}

#[tokio::test]
async fn another_accounts_writes_do_not_move_this_accounts_tokens() {
    let app = fixture();
    attach_second_account_profile(&app);
    let mine = revisions(&app).await;
    let theirs = revisions_as(&app, "member-token-2", 2).await;
    let other = "member-token-2";
    ok(
        &app,
        other,
        "PUT",
        "/api/profiles/2/progress",
        progress(30_000),
    )
    .await;
    ok(
        &app,
        other,
        "PUT",
        "/api/profiles/2/library",
        favorite("other-movie"),
    )
    .await;
    ok(
        &app,
        other,
        "PUT",
        "/api/profiles/2/integrations",
        json!({"revision":0,"browsing_plugin":"stremio","assignments":[]}),
    )
    .await;
    let current = ok(
        &app,
        other,
        "GET",
        "/api/profiles/2/preferences",
        Value::Null,
    )
    .await;
    ok(
        &app,
        other,
        "PUT",
        "/api/profiles/2/preferences",
        json!({"revision":current["revision"],"preferences":current["preferences"]}),
    )
    .await;
    ok(
        &app,
        other,
        "PATCH",
        "/api/profiles/2",
        json!({"name":"Theirs","avatar_id":"zayne"}),
    )
    .await;
    let extra = ok(
        &app,
        other,
        "POST",
        "/api/profiles",
        json!({"name":"Extra"}),
    )
    .await;
    ok(
        &app,
        other,
        "DELETE",
        &format!("/api/profiles/{}", extra["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    ok(&app, other, "PUT", "/api/parent/pin", json!({"pin":"1357"})).await;
    ok(
        &app,
        other,
        "PUT",
        "/api/profiles/2/kids",
        json!({"enabled":false,"max_age":5}),
    )
    .await;
    app.db.lock().unwrap().execute("INSERT INTO addons(name,manifest_url,manifest,account_id) VALUES('Theirs','https://theirs.invalid/manifest.json','{}',2)", []).unwrap();
    assert_eq!(revisions(&app).await, mine);
    let changed_theirs = changed(&theirs, &revisions_as(&app, other, 2).await);
    assert_eq!(changed_theirs, KEYS, "account 2 observes its own writes");
}

#[tokio::test]
async fn bootstrap_carries_the_selected_profile_revisions() {
    let app = fixture();
    ok(
        &app,
        "member-token-1",
        "PUT",
        "/api/profiles/1/progress",
        progress(30_000),
    )
    .await;
    let bootstrap = ok(&app, "member-token-1", "GET", "/api/bootstrap", Value::Null).await;
    assert!(bootstrap["revisions"].is_object(), "{bootstrap}");
    assert_eq!(bootstrap["revisions"], revisions(&app).await);
    assert_ne!(bootstrap["revisions"]["library"], "0");
    // Existing fields are unchanged.
    assert!(bootstrap["home"]["revision"].is_u64(), "{bootstrap}");
    // An unselected session has no profile revisions.
    let unselected = ok(&app, "member-token-2", "GET", "/api/bootstrap", Value::Null).await;
    assert!(unselected["revisions"].is_null(), "{unselected}");
}

#[tokio::test]
async fn profile_revision_triggers_are_idempotent_and_survive_cascading_deletes() {
    let app = fixture();
    let db = app.db.lock().unwrap();
    super::revisions::init(&db).unwrap();
    super::revisions::init(&db).unwrap();
    let read = |db: &rusqlite::Connection| -> i64 {
        db.query_row(
            "SELECT COALESCE((SELECT revision FROM engine_account_profile_revisions WHERE account_id=1),0)",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    let start = read(&db);
    db.execute_batch("INSERT INTO profiles(id,name) VALUES(7,'Cascade'); INSERT INTO profile_owners(profile_id,account_id,created_at) VALUES(7,1,0);").unwrap();
    let created = read(&db);
    assert!(created > start);
    db.execute("DELETE FROM profiles WHERE id=7", []).unwrap();
    assert!(read(&db) > created, "profile deletion cascades to a bump");
    // Deleting an account removes its revision row without failing the cascade.
    db.execute("DELETE FROM auth_accounts WHERE id=1", [])
        .unwrap();
    let rows: i64 = db
        .query_row(
            "SELECT count(*) FROM engine_account_profile_revisions WHERE account_id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rows, 0);
}
