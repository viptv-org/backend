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
async fn home_preview_does_not_share_episode_frames_when_parent_artwork_is_missing() {
    let app=fixture();
    assert_eq!(request(&app,"member-token-1","PUT","/api/profiles/1/progress",body(30_000)).await.0,StatusCode::OK);
    let mut sibling=get(&app,"/api/profiles/1/history").await[0]["card"].clone();
    sibling["identity"]=identity("other-episode",true);
    sibling["identity"]["episode"]["episode"]=json!(5);
    sibling["landscape"]=json!("https://images.example/other-frame.jpg");
    assert_eq!(request(&app,"member-token-1","PUT","/api/profiles/1/library",json!({"card":sibling})).await.0,StatusCode::OK);
    let home=get(&app,"/api/profiles/1/home").await;
    assert_eq!(home["rows"][0]["cards"][0]["view"]["preview"]["background"],"https://images.example/frame.jpg");
    assert_eq!(home["rows"][1]["cards"][0]["view"]["preview"]["background"],"https://images.example/other-frame.jpg");
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
    let mut expected=entry["card"].clone();expected["view"]=super::viewing_presentation::card_view(&serde_json::from_value(expected.clone()).unwrap(),false,true);
    expected["view"]["preview"]=json!({"background":"https://images.example/frame.jpg","caption":"Resume S2 E4 · 2m left","progress":0.3,"action":"play"});
    assert_eq!(home["home"]["rows"][0]["cards"][0], expected);
    let explicit=get(&app,"/api/profiles/1/home").await;
    assert_eq!(explicit["rows"][0]["cards"][0]["view"]["preview"],expected["view"]["preview"]);
    let mut parent=entry["card"].clone();
    parent["identity"]=identity("opaque-parent",false);parent["parent_identity"]=Value::Null;
    parent["landscape"]=json!("https://images.example/series-background.jpg");
    let mut lease=ResourceLease{policy_revision:0,principal:auth::Principal::Account{account_id:1,profile_id:Some(1),role:"member".into(),session_id:Some("other-family".into())},session_id:Some("other-family".into())};
    super::source_integrations::cache_card(&app,&lease,0,serde_json::from_value(parent.clone()).unwrap());
    assert_eq!(get(&app,"/api/profiles/1/home").await["rows"][0]["cards"][0]["view"]["preview"]["background"],"https://images.example/frame.jpg");
    let auth::Principal::Account{session_id,..}=&mut lease.principal;*session_id=Some("s1".into());lease.session_id=Some("s1".into());
    super::source_integrations::cache_card(&app,&lease,0,serde_json::from_value(parent.clone()).unwrap());
    let rich=get(&app,"/api/profiles/1/home").await;
    assert_eq!(rich["rows"][0]["cards"][0]["landscape"],"https://images.example/frame.jpg");
    assert_eq!(rich["rows"][0]["cards"][0]["view"]["preview"],json!({"background":"https://images.example/series-background.jpg","caption":"Resume S2 E4 · 2m left","progress":0.3,"action":"play"}));
    assert_eq!(request(&app,"member-token-1","PUT","/api/profiles/1/library",json!({"card":parent})).await.0,StatusCode::OK);
    let saved_home=get(&app,"/api/profiles/1/home").await;
    assert_eq!(saved_home["rows"][1]["cards"][0]["view"]["preview"]["action"],"details");
    assert_eq!(saved_home["rows"][1]["cards"][0]["view"]["preview"]["caption"],"Resume S2 E4 · 2m left");
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

#[tokio::test]
async fn history_reports_a_legacy_completion_with_an_unknown_watch_date() {
    let app = fixture();
    app.db.lock().unwrap().execute("INSERT INTO progress(profile_id,id,type,name,position,duration,updated_at,context,title_id) VALUES(1,'imported-movie','movie','Imported movie',0,0,0,'{\"watched_override\":true,\"stremio_import_watched\":true,\"stremio_completion_only\":true,\"stremio_watch_date_unknown\":true}','imported-movie')",[]).unwrap();
    let entry = get(&app, "/api/profiles/1/history").await[0].clone();
    assert_eq!(entry["card"]["identity"]["id"], "imported-movie");
    assert_eq!(entry["watched"], json!(true));
    assert_eq!(entry["completion_only"], json!(true));
    assert_eq!(entry["watch_date_known"], json!(false));
    assert_eq!(entry["resume_active"], json!(false));
    assert_eq!(entry["position"], json!(0.0));
    assert_eq!(entry["duration"], json!(0.0));
    assert!(entry["completed_at_millis"].is_null());
}

#[tokio::test]
async fn history_reports_position_and_watch_state_from_an_engine_record() {
    let app = fixture();
    let payload = |position: u64, completed: Option<u64>| {
        json!({"identity":identity("engine-episode",true),"parent_identity":identity("engine-parent",false),"provider_id":"stremio","title":"Engine episode","poster":null,"landscape":null,"position_millis":position,"duration_millis":100_000,"updated_at_millis":5_000,"completed_at_millis":completed})
    };
    app.db.lock().unwrap().execute("INSERT INTO engine_scoped_records VALUES(1,'playback','legacy','series','engine-episode','shared','engine-parent','',?1,NULL,5000,0)",[payload(30_000,None).to_string()]).unwrap();
    let entry = get(&app, "/api/profiles/1/history").await[0].clone();
    assert_eq!(entry["card"]["identity"], identity("engine-episode", true));
    assert_eq!(entry["position"], json!(30.0));
    assert_eq!(entry["duration"], json!(100.0));
    assert_eq!(entry["watched"], json!(false));
    assert_eq!(entry["resume_active"], json!(true));
    assert_eq!(entry["completion_only"], json!(false));
    assert_eq!(entry["watch_date_known"], json!(true));
    assert!(entry["completed_at_millis"].is_null());
    app.db.lock().unwrap().execute("UPDATE engine_scoped_records SET value=?1 WHERE profile_id=1 AND record_kind='playback'",[payload(95_000,Some(5_000)).to_string()]).unwrap();
    let completed = get(&app, "/api/profiles/1/history").await[0].clone();
    assert_eq!(completed["position"], json!(95.0));
    assert_eq!(completed["watched"], json!(true));
    assert_eq!(completed["resume_active"], json!(false));
    assert_eq!(completed["completion_only"], json!(false));
    assert_eq!(completed["completed_at_millis"], json!(5_000));
}
