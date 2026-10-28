use crate::{auth_integration_tests::fixture, test_support::request, *};
async fn fact(app: &App, token: &str) -> Value {
    let (status, body) = request(app, token, "GET", "/api/bootstrap", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["subtitle_availability"].clone()
}
/// One request over a raw loopback socket, so a test can play traffic the host
/// sends to any new listener without involving the app's own HTTP client.
async fn probe(address: std::net::SocketAddr, method: &str, path: &str, host: &str) -> StatusCode {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
    let request = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    socket.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    socket.read_to_end(&mut response).await.unwrap();
    let text = String::from_utf8_lossy(&response);
    let status = text.split_whitespace().nth(1).and_then(|v| v.parse().ok());
    StatusCode::from_u16(status.unwrap()).unwrap()
}
#[tokio::test]
async fn subtitle_bootstrap_fact_requires_enabled_scoped_assignment_and_performs_no_provider_io() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let app = fixture();
    assert_eq!(
        fact(&app, "member-token-1").await,
        json!({"available":false,"provider_count":0})
    );
    let reads = Arc::new(AtomicUsize::new(0));
    let count = reads.clone();
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let log = seen.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().fallback(
                move |method: axum::http::Method,
                      uri: axum::http::Uri,
                      headers: axum::http::HeaderMap| {
                    let count = count.clone();
                    let log = log.clone();
                    async move {
                        if method != axum::http::Method::HEAD {
                            count.fetch_add(1, Ordering::SeqCst);
                            log.lock().unwrap().push(format!(
                                "{method} {} host={:?}",
                                uri.path(),
                                headers.get(axum::http::header::HOST)
                            ));
                        }
                        StatusCode::BAD_GATEWAY
                    }
                },
            ),
        )
        .await
        .unwrap()
    });
    {
        let db = app.db.lock().unwrap();
        for (id, resources) in [
            (1, json!(["meta"])),
            (2, json!([{"name":"subtitles","types":["movie"]}])),
        ] {
            db.execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id) VALUES(?1,'Cached fixture',?2,?3,1)",params![id,format!("http://{address}/{id}/manifest.json"),json!({"id":format!("fixture-{id}"),"name":"Cached fixture","version":"1","resources":resources,"types":["movie"]}).to_string()]).unwrap();
        }
        db.execute("INSERT INTO profiles(id,name,avatar_seed,presentation_complete) VALUES(2,'Foreign','foreign',1)",[]).unwrap();
        db.execute(
            "INSERT INTO profile_owners(profile_id,account_id,created_at) VALUES(2,2,0)",
            [],
        )
        .unwrap();
        db.execute("UPDATE auth_sessions SET profile_id=2 WHERE id='s2'", [])
            .unwrap();
    }
    crate::test_support::encrypt_fixture_sources(&app);
    assert_eq!(
        fact(&app, "member-token-1").await,
        json!({"available":true,"provider_count":1})
    );
    assert_eq!(
        fact(&app, "member-token-2").await,
        json!({"available":false,"provider_count":0})
    );
    for (revision, enabled) in [(0, false), (1, true)] {
        let(status,body)=request(&app,"member-token-1","PUT","/api/profiles/1/integrations",json!({"revision":revision,"browsing_plugin":"stremio","assignments":[{"connection_id":"addon:1","enabled":["Metadata"]},{"connection_id":"addon:2","enabled":if enabled{json!(["Subtitles"])}else{json!([])}}]})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            fact(&app, "member-token-1").await,
            json!({"available":enabled,"provider_count":usize::from(enabled)})
        );
    }
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE addons SET enabled=0 WHERE id=2", [])
        .unwrap();
    assert_eq!(
        fact(&app, "member-token-1").await,
        json!({"available":false,"provider_count":0})
    );
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE auth_sessions SET profile_id=NULL WHERE id='s1'", [])
        .unwrap();
    assert!(fact(&app, "member-token-1").await.is_null());
    // The host reaches this listener too: `zeron headless` probes every new
    // loopback listener with `HEAD /` and catalogues an HTTP answer as a dev
    // server, which a preview can then load with a real `GET /` (backend#48,
    // backend#58). Neither is provider egress, so only the host probe and the
    // backend are compared here.
    for method in ["HEAD", "GET"] {
        assert_eq!(
            probe(address, method, "/", &format!("localhost:{}", address.port())).await,
            StatusCode::BAD_GATEWAY
        );
    }
    let provider_requests = seen.lock().unwrap().clone();
    assert_eq!(
        reads.load(Ordering::SeqCst),
        0,
        "provider requests: {provider_requests:?}"
    );
    // A read of a fixture add-on is still counted, so the assertion above is
    // not vacuous.
    assert_eq!(
        probe(address, "GET", "/2/manifest.json", &address.to_string()).await,
        StatusCode::BAD_GATEWAY
    );
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn bootstrap_negotiates_required_audio_api_without_an_extra_request_or_selected_profile() {
    let app = fixture();
    for profile in [Some(1_i64), None] {
        app.db.lock().unwrap().execute("UPDATE auth_sessions SET profile_id=?1 WHERE id='s1'", [profile]).unwrap();
        let(status,body)=request(&app,"member-token-1","GET","/api/bootstrap",Value::Null).await;
        assert_eq!(status,StatusCode::OK);
        assert_eq!(body["playback_capabilities"],json!({"required_audio_index":true}));
    }
    let(status,body)=request(&app,"invalid-token","GET","/api/bootstrap",Value::Null).await;
    assert_eq!(status,StatusCode::UNAUTHORIZED);
    assert!(body.get("playback_capabilities").is_none());
}

#[tokio::test]
async fn navigation_uses_scoped_cached_catalogs_and_live_assignments() {
    let app = fixture();
    let contract: Value = serde_json::from_str(include_str!("../../../fixtures/remote-viewing-v3.json")).unwrap();
    {
        let db = app.db.lock().unwrap();
        for (id, account, kind) in [(21,1,"movie"),(22,2,"series")] {
            db.execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id) VALUES(?1,'Cached catalog',?2,?3,?4)",params![id,format!("http://127.0.0.1:9/{id}/manifest.json"),json!({"id":format!("navigation-{id}"),"name":"Cached catalog","version":"1","resources":["catalog","meta"],"types":[kind],"catalogs":[{"type":kind,"id":"fixture","name":"Fixture","extra":[{"name":"search","isRequired":false}]}]}).to_string(),account]).unwrap();
        }
        db.execute_batch("INSERT INTO providers(id,name,url,username,password,enabled,enable_live,enable_movies,enable_series) VALUES(21,'Owned Live','https://fixture.invalid','fixture','fixture',1,1,0,0); INSERT INTO provider_ownership VALUES(21,1);").unwrap();
    }
    crate::test_support::encrypt_fixture_sources(&app);
    let configure = |revision, enabled| json!({"revision":revision,"browsing_plugin":"stremio","assignments":[
        {"connection_id":"addon:21","enabled":if enabled{json!(["Discovery","Metadata","Search"])}else{json!([])}},
        {"connection_id":"iptv:21","enabled":if enabled{json!(["LiveTv"])}else{json!([])}}
    ]});
    let lease = ResourceLease { policy_revision:0, principal:auth::Principal::Account { account_id:1,role:"member".into(),profile_id:Some(1),session_id:Some("s1".into()) },session_id:Some("s1".into()) };
    let ids = |value:Value| value.as_array().unwrap().iter().map(|item|item["id"].as_str().unwrap().to_owned()).collect::<Vec<_>>();
    for (revision, enabled) in [(0,true),(1,false)] {
        let (status,value)=request(&app,"member-token-1","PUT","/api/profiles/1/integrations",configure(revision,enabled)).await;
        assert_eq!(status,StatusCode::OK,"{value}");
        let navigation=super::bootstrap_facts::navigation(&app,&lease,Some(1)).unwrap();
        assert_eq!(navigation, contract["navigation"][if enabled {"movie_and_live"} else {"unconfigured"}]);
        assert_eq!(ids(navigation), if enabled {vec!["search","home","movie","live","library","profiles"]}else{vec!["home","library","profiles"]});
    }
    assert_eq!(ids(super::bootstrap_facts::navigation(&app,&lease,None).unwrap()),vec!["profiles"]);
    assert_eq!(request(&app,"member-token-1","PUT","/api/profiles/1/integrations",configure(2,true)).await.0,StatusCode::OK);
    app.db.lock().unwrap().execute_batch("DELETE FROM addons WHERE id=21;").unwrap();
    let live_only = super::bootstrap_facts::navigation(&app,&lease,Some(1)).unwrap();
    assert_eq!(live_only,contract["navigation"]["live_only"]);
    app.db.lock().unwrap().execute("UPDATE providers SET enabled=0 WHERE id=21",[]).unwrap();
    assert_eq!(ids(super::bootstrap_facts::navigation(&app,&lease,Some(1)).unwrap()),vec!["home","library","profiles"]);
}

#[tokio::test]
async fn restricted_navigation_keeps_approved_library_browse_and_omits_live_without_parent_permission() {
    let app=fixture();
    app.db.lock().unwrap().execute_batch("INSERT INTO kids_profiles(profile_id,enabled,max_age,revision) VALUES(1,1,12,1); INSERT INTO providers(id,name,url,username,password,enabled,enable_live,enable_movies,enable_series) VALUES(23,'Owned Live','https://fixture.invalid','fixture','fixture',1,1,0,0); INSERT INTO provider_ownership VALUES(23,1);").unwrap();
    crate::test_support::encrypt_fixture_sources(&app);
    let lease=ResourceLease {policy_revision:1,principal:auth::Principal::Account{account_id:1,role:"member".into(),profile_id:Some(1),session_id:Some("s1".into())},session_id:Some("s1".into())};
    let nav=super::bootstrap_facts::navigation(&app,&lease,Some(1)).unwrap();
    let contract: Value = serde_json::from_str(include_str!("../../../fixtures/remote-viewing-v3.json")).unwrap();
    assert_eq!(nav, contract["navigation"]["restricted_without_parent"]);
    let ids:Vec<_>=nav.as_array().unwrap().iter().map(|item|item["id"].as_str().unwrap()).collect();
    assert_eq!(ids,vec!["search","home","movie","series","library","profiles"]);
    app.db.lock().unwrap().execute("INSERT INTO parent_grants(session_id,expires) VALUES('s1',?1)",[util::now()+60]).unwrap();
    let nav=super::bootstrap_facts::navigation(&app,&lease,Some(1)).unwrap();
    assert!(nav.as_array().unwrap().iter().any(|item|item["id"]=="live"));
}
