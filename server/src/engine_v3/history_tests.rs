use crate::{auth_integration_tests::fixture, test_support::request, *};

fn identity(id: &str, episode: bool) -> Value {
    json!({"id":id,"kind":"series","ids":[{"namespace":"addon","value":id}],"edition":null,
        "episode":if episode {json!({"order":"aired","season":2,"episode":4})} else {Value::Null}})
}
fn body(position: u64) -> Value {
    json!({"record":{"identity":identity("opaque-episode",true),"parent_identity":identity("opaque-parent",false),"provider_id":"stremio","title":"Actual episode title","poster":"https://images.example/poster.jpg","landscape":"https://images.example/frame.jpg","position_millis":position,"duration_millis":100_000,"updated_at_millis":1,"completed_at_millis":2}})
}
async fn get(app: &App, path: &str) -> Value {
    let (status, value) = request(app, "member-token-1", "GET", path, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value
}

#[tokio::test]
async fn typed_history_and_home_round_trip_opaque_parent_and_real_progress() {
    let app = fixture();
    let before = get(&app, "/api/bootstrap").await;
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/profiles/1/progress",
            body(30_000)
        )
        .await
        .0,
        StatusCode::OK
    );
    let history = get(&app, "/api/profiles/1/history").await;
    let entry = &history[0];
    assert_eq!(entry["card"]["identity"], identity("opaque-episode", true));
    assert_eq!(
        entry["card"]["parent_identity"],
        identity("opaque-parent", false)
    );
    assert_eq!(
        entry["card"]["landscape"],
        "https://images.example/frame.jpg"
    );
    assert_eq!(entry["card"]["provider_id"], "stremio");
    assert_eq!(entry["card"]["progress"], 0.3);
    assert!(entry["completed_at_millis"].is_null());
    assert!(entry["updated_at_millis"].as_u64().unwrap() > 1000);
    let home = get(&app, "/api/bootstrap").await;
    assert_eq!(home["home"]["rows"][0]["cards"][0], entry["card"]);
    assert!(
        home["home"]["revision"].as_u64().unwrap() > before["home"]["revision"].as_u64().unwrap()
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/profiles/1/progress",
            body(95_000)
        )
        .await
        .0,
        StatusCode::OK
    );
    let completed = get(&app, "/api/profiles/1/history").await;
    assert!(completed[0]["completed_at_millis"].as_u64().is_some());
    assert_eq!(completed[0]["card"]["progress"], 0.95);
    assert_eq!(
        request(
            &app,
            "member-token-2",
            "GET",
            "/api/profiles/1/history",
            json!({})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn conflicting_parent_is_rejected_and_legacy_context_is_projected_without_id_guessing() {
    let app = fixture();
    let mut input = body(10_000);
    input["parent_identity"] = identity("different-parent", false);
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/profiles/1/progress",
            input
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert!(get(&app, "/api/profiles/1/history")
        .await
        .as_array()
        .unwrap()
        .is_empty());
    // Legacy rows (written by imports) project into typed history cards.
    app.db.lock().unwrap().execute("INSERT INTO progress(profile_id,id,type,name,position,duration,updated_at,context,title_id) VALUES(1,'legacy-opaque','series','Legacy episode',20,100,0,'{\"series_id\":\"legacy-parent\",\"season\":1,\"episode\":3}','legacy-parent')",[]).unwrap();
    let history = get(&app, "/api/profiles/1/history?limit=1").await;
    assert_eq!(history[0]["card"]["identity"]["id"], "legacy-opaque");
    assert_eq!(history[0]["card"]["parent_identity"]["id"], "legacy-parent");
    assert_eq!(history[0]["card"]["identity"]["episode"]["episode"], 3);
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO kids_profiles(profile_id,enabled,max_age,revision) VALUES(1,1,12,1)",
            [],
        )
        .unwrap();
    assert!(get(&app, "/api/profiles/1/history")
        .await
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/profiles/1/progress",
            body(30_000)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}
