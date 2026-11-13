use super::*;
use axum::{routing::get, Json, Router};

fn member_app() -> App {
    let mut app = fixture();
    app.addons.allow_test_loopback = true;
    app.providers.allow_test_loopback = true;
    let db = app.db.lock().unwrap();
    db.execute_batch("INSERT INTO profiles(id,name,avatar_seed,presentation_complete) VALUES(2,'Member','fixture-two',1); INSERT INTO profile_owners(profile_id,account_id,created_at) VALUES(2,2,0); INSERT INTO auth_profiles VALUES(2,2); UPDATE auth_sessions SET profile_id=2 WHERE account_id=2;").unwrap();
    drop(db);
    app
}

async fn source() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, Router::new()
            .route("/manifest.json", get(|| async { Json(json!({"id":"fixture","name":"Member source","resources":["stream"],"types":["movie"],"behaviorHints":{"configurable":true}})) }))
            .route("/player_api.php", get(|| async { Json(json!({"user_info":{"auth":1,"status":"Active"}})) })))
            .await.unwrap();
    });
    (base, task)
}

fn parent(app: &App, unlocked: bool) {
    let db = app.db.lock().unwrap();
    db.execute("INSERT INTO kids_profiles(profile_id,enabled) VALUES(2,1) ON CONFLICT(profile_id) DO UPDATE SET enabled=1", []).unwrap();
    db.execute("DELETE FROM parent_grants WHERE session_id='s2'", []).unwrap();
    if unlocked {
        db.execute("INSERT INTO parent_grants(session_id,expires) VALUES('s2',?1) ON CONFLICT(session_id) DO UPDATE SET expires=excluded.expires", [util::now()+120]).unwrap();
    }
}

#[tokio::test]
async fn capability_is_explicit_account_scoped_and_parent_effective() {
    let app = member_app();
    let tv = secondary_session(&app, "member-tv", 2, "device");
    let native = native_session(&app, "member-phone", 2);
    app.db.lock().unwrap().execute("UPDATE auth_sessions SET profile_id=2 WHERE account_id=2", []).unwrap();
    for (token, integrations, admin) in [("member-token-1",true,true),("member-token-2",true,false),(native.as_str(),true,false),(tv.as_str(),false,false)] {
        let (status, me) = request(&app, token, "GET", "/api/auth/me", Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{me}");
        assert_eq!(me["capabilities"]["manage_integrations"], integrations, "{token} {me}");
        assert_eq!(me["capabilities"]["manage_account"], admin);
        let (status, bootstrap) = request(&app, token, "GET", "/api/bootstrap", Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{bootstrap}");
        assert_eq!(bootstrap["capabilities"], me["capabilities"]);
    }
    parent(&app, false);
    let (_, me) = request(&app,"member-token-2","GET","/api/auth/me",Value::Null).await;
    assert_eq!(me["capabilities"]["manage_integrations"], false);
    parent(&app, true);
    let (_, me) = request(&app,"member-token-2","GET","/api/auth/me",Value::Null).await;
    assert_eq!(me["capabilities"]["manage_integrations"], true);
}

#[tokio::test]
async fn member_addon_crud_rechecks_ownership_device_and_parent_authority() {
    let app = member_app();
    let (base, upstream) = source().await;
    let (status, addon) = request(&app,"member-token-2","POST","/api/addons",json!({"manifest_url":format!("{base}/manifest.json")})).await;
    assert_eq!(status, StatusCode::OK, "{addon}");
    let id = addon["id"].as_i64().unwrap();
    let path = format!("/api/addons/{id}");
    let link = format!("{path}/configure-link");
    assert_eq!(request(&app,"member-token-2","POST",&link,Value::Null).await.0, StatusCode::OK);
    for (method, url) in [("PATCH",path.as_str()),("DELETE",path.as_str()),("POST",link.as_str())] {
        assert_eq!(request(&app,"member-token-1",method,url,json!({"enabled":false})).await.0, StatusCode::NOT_FOUND, "cross-account {method}");
    }
    let tv = secondary_session(&app,"addon-tv",2,"device");
    for (method, url, body) in [("POST","/api/addons",json!({"manifest_url":format!("{base}/manifest.json")})),("PATCH",path.as_str(),json!({"enabled":false})),("DELETE",path.as_str(),Value::Null),("POST",link.as_str(),Value::Null)] {
        let (status,error) = request(&app,&tv,method,url,body).await;
        assert_eq!(status,StatusCode::FORBIDDEN,"{error}");
        assert_eq!(error["error_code"],"account_session_required");
    }
    parent(&app,false);
    assert_eq!(request(&app,"member-token-2","PATCH",&path,json!({"enabled":false})).await.0,StatusCode::FORBIDDEN);
    assert!(app.db.lock().unwrap().query_row("SELECT enabled FROM addons WHERE id=?1",[id],|r|r.get::<_,bool>(0)).unwrap());
    parent(&app,true);
    assert_eq!(request(&app,"member-token-2","PATCH",&path,json!({"enabled":false})).await.0,StatusCode::OK);
    let native = native_session(&app,"addon-native",2);
    assert_eq!(request(&app,&native,"PATCH",&path,json!({"enabled":true})).await.0,StatusCode::OK);
    assert_eq!(request(&app,"member-token-2","DELETE",&path,Value::Null).await.0,StatusCode::OK);
    upstream.abort();
}

#[tokio::test]
async fn member_sdk_configuration_preserves_profile_and_device_fences() {
    let app = member_app();
    let input = json!({"revision":0,"plugin_id":"vsources","configuration":{"tmdb_api_key":"fixture-member-key"}});
    let tv = secondary_session(&app,"sdk-tv",2,"device");
    for token in ["member-token-1",tv.as_str()] {
        assert_eq!(request(&app,token,"PUT","/api/profiles/2/source-integrations",input.clone()).await.0,StatusCode::FORBIDDEN);
    }
    parent(&app,false);
    assert_eq!(request(&app,"member-token-2","PUT","/api/profiles/2/source-integrations",input.clone()).await.0,StatusCode::FORBIDDEN);
    parent(&app,true);
    let (status,result) = request(&app,"member-token-2","PUT","/api/profiles/2/source-integrations",input).await;
    if !cfg!(feature="vsources-runtime") { assert_eq!(status,StatusCode::SERVICE_UNAVAILABLE,"{result}"); return; }
    assert_eq!(status,StatusCode::OK,"{result}");
    assert!(!result.to_string().contains("fixture-member-key"));
    let (_, schemas) = request(&app,"member-token-2","GET","/api/profiles/2/source-integrations",Value::Null).await;
    let id = schemas["schemas"][0]["connection_id"].as_str().unwrap();
    let path = format!("/api/profiles/2/source-integrations/{id}?revision=1");
    assert_eq!(request(&app,&tv,"DELETE",&path,Value::Null).await.0,StatusCode::FORBIDDEN);
    assert_eq!(request(&app,"member-token-1","DELETE",&path,Value::Null).await.0,StatusCode::FORBIDDEN);
    assert_eq!(request(&app,"member-token-2","DELETE",&path,Value::Null).await.0,StatusCode::OK);
}

#[tokio::test]
async fn member_iptv_configuration_refresh_and_defaults_stay_account_scoped() {
    let app = member_app();
    let (base, upstream) = source().await;
    let (status,result) = request(&app,"member-token-2","POST","/api/iptv/connections",json!({"name":"Member IPTV","url":base,"username":"fixture","password":"fixture-password"})).await;
    assert_eq!(status,StatusCode::OK,"{result}");
    let id = result["id"].as_i64().unwrap();
    let path = format!("/api/iptv/connections/{id}");
    let tv = secondary_session(&app,"iptv-tv",2,"device");
    for (method,url,body,member_status) in [
        ("PATCH",path.clone(),json!({"enabled":true}),StatusCode::OK),
        ("PUT",format!("{path}/credentials"),json!({"password":"replacement"}),StatusCode::OK),
        ("POST",format!("{path}/refresh"),Value::Null,StatusCode::ACCEPTED),
        ("DELETE",format!("{path}/refresh"),Value::Null,StatusCode::OK),
        ("PUT","/api/iptv/live-default".into(),json!({"catalog_id":id}),StatusCode::OK),
        ("PUT","/api/iptv/matches".into(),json!({"vod_id":"missing","metadata_id":"tmdb:1","type":"movie"}),StatusCode::NOT_FOUND),
    ] {
        assert_eq!(request(&app,"member-token-1",method,&url,body.clone()).await.0,StatusCode::NOT_FOUND,"cross-account {method} {url}");
        let (status,error) = request(&app,&tv,method,&url,body.clone()).await;
        assert_eq!(status,StatusCode::FORBIDDEN,"{error}");
        assert_eq!(error["error_code"],"account_session_required");
        parent(&app,false);
        assert_eq!(request(&app,"member-token-2",method,&url,body.clone()).await.0,StatusCode::FORBIDDEN,"locked {method} {url}");
        parent(&app,true);
        if method=="POST" { app.db.lock().unwrap().execute("UPDATE providers SET enabled=1 WHERE id=?1",[id]).unwrap(); }
        assert_eq!(request(&app,"member-token-2",method,&url,body).await.0,member_status,"member {method} {url}");
    }
    assert_eq!(request(&app,"member-token-1","DELETE",&path,Value::Null).await.0,StatusCode::NOT_FOUND);
    assert_eq!(request(&app,"member-token-2","DELETE",&path,Value::Null).await.0,StatusCode::OK);
    upstream.abort();
}
