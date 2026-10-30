use crate::{auth_integration_tests::fixture, test_support::request, *};
fn input(id: &str, title: &str) -> Value {
    json!({"card":{"identity":{"id":id,"kind":"movie","ids":[{"namespace":"imdb","value":"tt1234567"},{"namespace":"simkl","value":"123"}],"edition":null,"episode":null},"provider_id":"simkl","title":title,"subtitle":null,"description":"Actual metadata","poster":"https://images.example/poster.jpg","landscape":"https://images.example/background.jpg","logo":null,"progress":0.8,"badges":[],"parent_identity":null}})
}
async fn get(app: &App, path: &str) -> Value {
    let (status, value) = request(app, "member-token-1", "GET", path, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value
}
#[tokio::test]
async fn typed_library_preserves_provider_identity_shared_rows_profile_fences_and_paging() {
    let app = fixture();
    let before = get(&app, "/api/bootstrap").await;
    for (id, name) in [("opaque-movie-a", "Alpha"), ("opaque-movie-b", "Beta")] {
        for _ in 0..2 {
            assert_eq!(
                request(
                    &app,
                    "member-token-1",
                    "PUT",
                    "/api/profiles/1/library",
                    input(id, name)
                )
                .await
                .0,
                StatusCode::OK
            );
        }
    }
    let page = get(&app, "/api/profiles/1/library?limit=1").await;
    assert_eq!(
        page["cards"][0]["identity"],
        input("opaque-movie-b", "Beta")["card"]["identity"]
    );
    assert_eq!(page["cards"][0]["provider_id"], "simkl");
    assert_eq!(
        page["cards"][0]["landscape"],
        "https://images.example/background.jpg"
    );
    assert!(page["cards"][0]["progress"].is_null());
    assert_eq!(page["next_cursor"], "1");
    assert_eq!(page["complete"], false);
    let next = get(&app, "/api/profiles/1/library?offset=1&limit=1").await;
    assert_eq!(next["cards"][0]["identity"]["id"], "opaque-movie-a");
    assert_eq!(next["complete"], true);
    let home = get(&app, "/api/bootstrap").await;
    assert_eq!(
        home["home"]["rows"][1]["cards"][0]["identity"],
        page["cards"][0]["identity"]
    );
    assert_eq!(home["home"]["rows"][1]["cards"][1]["provider_id"], "simkl");
    assert!(
        home["home"]["revision"].as_u64().unwrap() > before["home"]["revision"].as_u64().unwrap()
    );
    assert_eq!(
        get(&app, "/api/profiles/1/favorites")
            .await
            .as_array()
            .unwrap()
            .len(),
        2
    );
    for (method, path, body) in [
        ("GET", "/api/profiles/1/library", json!({})),
        ("PUT", "/api/profiles/1/library", input("x", "X")),
        (
            "DELETE",
            "/api/profiles/1/library/movie/opaque-movie-a",
            json!({}),
        ),
    ] {
        assert_eq!(
            request(&app, "member-token-2", method, path, body).await.0,
            StatusCode::FORBIDDEN
        );
    }
    for _ in 0..2 {
        assert_eq!(
            request(
                &app,
                "member-token-1",
                "DELETE",
                "/api/profiles/1/library/movie/opaque-movie-a",
                json!({})
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    assert_eq!(
        get(&app, "/api/profiles/1/favorites")
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
#[tokio::test]
async fn legacy_favorites_are_projected_without_guessing_and_restricted_changes_fail_closed() {
    let app = fixture();
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/profiles/1/favorites",
            json!({"id":"opaque:favorite","type":"movie","name":"Legacy title"})
        )
        .await
        .0,
        StatusCode::OK
    );
    let page = get(&app, "/api/profiles/1/library").await;
    assert_eq!(page["cards"][0]["identity"]["id"], "opaque:favorite");
    assert_eq!(page["cards"][0]["provider_id"], "library");
    let mut bad = input("", "Invalid");
    bad["card"]["identity"]["id"] = json!("");
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/profiles/1/library",
            bad
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO kids_profiles(profile_id,enabled,max_age,revision) VALUES(1,1,12,1)",
            [],
        )
        .unwrap();
    assert!(get(&app, "/api/profiles/1/library").await["cards"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/profiles/1/library",
            input("other", "Other")
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "DELETE",
            "/api/profiles/1/library/movie/opaque%3Afavorite",
            json!({})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM favorites WHERE profile_id=1",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
}
#[tokio::test]
async fn favorite_dates_preserve_first_add_and_do_not_invent_legacy_timestamps(){
 let app=fixture();app.db.lock().unwrap().execute("INSERT INTO favorites(profile_id,id,type,name,poster)VALUES(1,'legacy-date','movie','Legacy',NULL)",[]).unwrap();
 let card=json!({"identity":{"id":"new-date","kind":"movie","ids":[],"episode":null,"edition":null},"provider_id":"stremio","title":"New","subtitle":null,"description":null,"poster":null,"landscape":null,"logo":null,"progress":null,"badges":[],"parent_identity":null,"added_at_millis":1});
 let(status,_)=request(&app,"member-token-1","PUT","/api/profiles/1/library",json!({"card":card})).await;assert_eq!(status,StatusCode::OK);
 let(_,page)=request(&app,"member-token-1","GET","/api/profiles/1/library",Value::Null).await;let first=page["cards"].as_array().unwrap().iter().find(|c|c["identity"]["id"]=="new-date").unwrap()["added_at_millis"].as_u64().unwrap();assert!(first>1);assert_eq!(page["cards"][0]["identity"]["id"],"new-date");assert!(page["cards"].as_array().unwrap().iter().find(|c|c["identity"]["id"]=="legacy-date").unwrap()["added_at_millis"].is_null());
 let(status,_)=request(&app,"member-token-1","PUT","/api/profiles/1/library",json!({"card":card})).await;assert_eq!(status,StatusCode::OK);let(_,page)=request(&app,"member-token-1","GET","/api/profiles/1/library",Value::Null).await;assert_eq!(page["cards"][0]["added_at_millis"],first);
}

#[tokio::test]
async fn library_and_home_reuse_exact_current_family_metadata_without_changing_personal_state(){
 let app=fixture();let saved=input("opaque-title","Alpha");assert_eq!(request(&app,"member-token-1","PUT","/api/profiles/1/library",saved.clone()).await.0,StatusCode::OK);
 let before=get(&app,"/api/profiles/1/library").await;let added=before["cards"][0]["added_at_millis"].clone();
 let mut full=saved["card"].clone();full["description"]=json!("Host full metadata");full["logo"]=json!("https://images.example/transparent.png");full["metadata"]=json!({"year":2026,"runtime_minutes":120,"genres":["Drama"],"rating":8.4,"rating_votes":100,"certification":null,"alternate_titles":[]});full["added_at_millis"]=json!(1);
 let mut lease=ResourceLease{policy_revision:0,principal:auth::Principal::Account{account_id:1,profile_id:Some(1),role:"owner".into(),session_id:Some("different-family".into())},session_id:Some("different-family".into())};
 super::source_integrations::cache_card(&app,&lease,0,serde_json::from_value(full.clone()).unwrap());assert!(get(&app,"/api/profiles/1/library").await["cards"][0]["logo"].is_null());
 let auth::Principal::Account{session_id,..}=&mut lease.principal;*session_id=Some("s1".into());lease.session_id=Some("s1".into());
 super::source_integrations::cache_card(&app,&lease,99,serde_json::from_value(full.clone()).unwrap());assert!(get(&app,"/api/profiles/1/library").await["cards"][0]["logo"].is_null());
 super::source_integrations::cache_card(&app,&lease,0,serde_json::from_value(full).unwrap());
 let page=get(&app,"/api/profiles/1/library").await;assert_eq!(page["cards"][0]["logo"],"https://images.example/transparent.png");assert_eq!(page["cards"][0]["description"],"Host full metadata");assert_eq!(page["cards"][0]["added_at_millis"],added);assert!(page["cards"][0]["progress"].is_null());
 let bootstrap=get(&app,"/api/bootstrap").await;let mut home_card=bootstrap["home"]["rows"][1]["cards"][0].clone();let preview=home_card["view"].as_object_mut().unwrap().remove("preview").unwrap();assert_eq!(preview,json!({"background":"https://images.example/background.jpg","caption":null,"progress":null,"action":"details"}));assert_eq!(bootstrap["library"]["cards"][0],home_card);assert_eq!(bootstrap["library"]["cards"][0]["logo"],"https://images.example/transparent.png");
}
#[tokio::test]
async fn real_completion_counts_remain_unknown_for_legacy_rows_and_increment_only_on_transitions(){
 let app=fixture();let db=app.db.lock().unwrap();
 db.execute("INSERT INTO progress(profile_id,id,type,name,position,duration,updated_at) VALUES(1,'legacy','movie','Legacy',95,100,1)",[]).unwrap();
 super::library_stats::observe(&db,1,"movie","legacy",true).unwrap();assert_eq!(db.query_row("SELECT count FROM engine_watch_counts WHERE profile_id=1 AND id='legacy'",[],|r|r.get::<_,Option<u64>>(0)).unwrap(),None);
 for (position,completed) in [(10,false),(95,true),(99,true),(10,false),(96,true)]{
  super::library_stats::observe(&db,1,"movie","new",completed).unwrap();db.execute("INSERT INTO progress(profile_id,id,type,name,position,duration,updated_at) VALUES(1,'new','movie','New',?1,100,1) ON CONFLICT(profile_id,type,id) DO UPDATE SET position=excluded.position",[position]).unwrap();
 }
 assert_eq!(db.query_row("SELECT count FROM engine_watch_counts WHERE profile_id=1 AND id='new'",[],|r|r.get::<_,u64>(0)).unwrap(),2);
 db.execute("DELETE FROM progress WHERE profile_id=1 AND id='new'",[]).unwrap();assert_eq!(db.query_row("SELECT count(*) FROM engine_watch_counts WHERE profile_id=1 AND id='new'",[],|r|r.get::<_,u64>(0)).unwrap(),0);
}
