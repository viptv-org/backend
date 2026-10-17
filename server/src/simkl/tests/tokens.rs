use super::*;

pub(super) fn expired(app: &App) {
    let value = json!({"access_token":"expired-access","refresh_token":"refresh-private","scope":"media:read media:write","expires_at":1});
    let sealed = app
        .secret_vault
        .as_ref()
        .unwrap()
        .seal(1, "simkl", "1", value.to_string().as_bytes())
        .unwrap();
    app.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE simkl_links SET credential=?1 WHERE profile_id=1",
            [sealed],
        )
        .unwrap();
}

#[tokio::test]
async fn expired_profile_token_refresh_is_coalesced_sealed_and_preserves_link_revision() {
    let (mut app, initial, _) = server().await;
    link(&app).await;
    initial.abort();
    expired(&app);
    let count = Arc::new(AtomicUsize::new(0));
    let called = count.clone();
    let routes=axum::Router::new().route("/oauth2/token",post(move|axum::Form(body):axum::Form<HashMap<String,String>>|{let called=called.clone();async move{
        assert_eq!(body["grant_type"],"refresh_token");assert_eq!(body["refresh_token"],"refresh-private");called.fetch_add(1,Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(20)).await;
        axum::Json(json!({"access_token":"fresh-access","refresh_token":"fresh-refresh","token_type":"Bearer","expires_in":604800,"scope":"media:read media:write"}))
    }})).route("/search/movie",get(|headers:axum::http::HeaderMap|async move{
        if headers.get(header::AUTHORIZATION).and_then(|v|v.to_str().ok())!=Some("Bearer fresh-access"){return StatusCode::UNAUTHORIZED.into_response();}
        axum::Json(json!([{"title":"Movie","ids":{"simkl":10}}])).into_response()
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
    let revision: i64 = app
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT revision FROM simkl_links WHERE profile_id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let requests = (0..6).map(|_| {
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/profiles/1/simkl/search?category=movie&query=Movie",
            json!({}),
        )
    });
    for (status, _) in futures::future::join_all(requests).await {
        assert_eq!(status, StatusCode::OK);
    }
    assert_eq!(count.load(Ordering::SeqCst), 1);
    let (current, sealed): (i64, String) = app
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT revision,credential FROM simkl_links WHERE profile_id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(revision, current);
    assert!(!sealed.contains("fresh-access"));
    assert!(!sealed.contains("fresh-refresh"));
    let clear = app
        .secret_vault
        .as_ref()
        .unwrap()
        .open(1, "simkl", "1", &sealed)
        .unwrap();
    let stored: Value = serde_json::from_slice(clear.expose()).unwrap();
    assert_eq!(stored["refresh_token"], "fresh-refresh");
    assert!(stored["expires_at"].as_i64().unwrap() > util::now());
    task.abort();
}

#[tokio::test]
async fn wrong_oauth_issuer_does_not_consume_valid_state_or_exchange_code() {
    let (app, task, _) = server().await;
    let state = start(&app).await;
    assert!(callback(
        State(app.clone()),
        Query(crate::simkl::oauth::Callback {
            state: state.clone(),
            code: Some("fixture-code".into()),
            error: None,
            iss: Some("https://simkl.com.attacker.invalid".into())
        })
    )
    .await
    .is_err());
    assert!(callback(
        State(app),
        Query(crate::simkl::oauth::Callback {
            state,
            code: Some("fixture-code".into()),
            error: None,
            iss: Some("https://simkl.com".into())
        })
    )
    .await
    .is_ok());
    task.abort();
}

#[tokio::test]
async fn cancelled_refresh_caller_still_seals_the_replacement_and_never_replays() {
    let (mut app, initial, _) = server().await;
    link(&app).await;
    initial.abort();
    expired(&app);
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let reads = Arc::new(AtomicUsize::new(0));
    let routes=Router::new().route("/oauth2/token",post({let started=started.clone();let release=release.clone();let reads=reads.clone();move|axum::Form(body):axum::Form<HashMap<String,String>>|{let started=started.clone();let release=release.clone();let reads=reads.clone();async move{
        assert_eq!(body["refresh_token"],"refresh-private");assert_eq!(reads.fetch_add(1,Ordering::SeqCst),0,"A consumed token must never be replayed");started.notify_one();let _permit=release.acquire().await.unwrap();
        axum::Json(json!({"access_token":"fresh-access","refresh_token":"fresh-refresh","token_type":"Bearer","expires_in":604800,"scope":"media:read media:write"}))
    }}})).route("/search/movie",get(||async{axum::Json(json!([{"title":"Movie","ids":{"simkl":10}}]))}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
    let pending = tokio::spawn({
        let app = app.clone();
        async move {
            request(
                &app,
                "member-token-1",
                "GET",
                "/api/profiles/1/simkl/search?category=movie&query=Movie",
                json!({}),
            )
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .unwrap();
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let sealed: String = app
                .db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT credential FROM simkl_links WHERE profile_id=1",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            let clear = app
                .secret_vault
                .as_ref()
                .unwrap()
                .open(1, "simkl", "1", &sealed)
                .unwrap();
            let token: Value = serde_json::from_slice(clear.expose()).unwrap();
            if token["refresh_token"] == "fresh-refresh" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let (status, _) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/simkl/search?category=movie&query=Movie",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    task.abort();
}

#[tokio::test]
async fn parent_grant_expiry_during_one_use_refresh_seals_replacement_without_returning_content() {
    let (mut app, initial, _) = server().await;
    link(&app).await;
    initial.abort();
    expired(&app);
    {
        let db = app.db.lock().unwrap();
        db.execute(
            "INSERT INTO kids_profiles(profile_id,enabled,max_age)VALUES(1,1,7)",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO parent_grants(session_id,expires)VALUES('s1',?1)",
            [util::now() + 120],
        )
        .unwrap();
    }
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let count = Arc::new(AtomicUsize::new(0));
    let routes=Router::new().route("/oauth2/token",post({let started=started.clone();let release=release.clone();let count=count.clone();move|axum::Form(body):axum::Form<HashMap<String,String>>|{let started=started.clone();let release=release.clone();let count=count.clone();async move{
        assert_eq!(body["refresh_token"],"refresh-private");assert_eq!(count.fetch_add(1,Ordering::SeqCst),0);started.notify_one();let _permit=release.acquire().await.unwrap();
        axum::Json(json!({"access_token":"fresh-access","refresh_token":"fresh-refresh","token_type":"Bearer","expires_in":604800,"scope":"media:read media:write"}))
    }}})).route("/search/:category",get(||async{axum::Json(json!([{"title":"Movie","ids":{"simkl":10}}]))}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
    let pending = tokio::spawn({
        let app = app.clone();
        async move {
            request(
                &app,
                "member-token-1",
                "GET",
                "/api/parent/search?query=Movie&profile_id=1",
                Value::Null,
            )
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .unwrap();
    app.db
        .lock()
        .unwrap()
        .execute("DELETE FROM parent_grants WHERE session_id='s1'", [])
        .unwrap();
    release.add_permits(1);
    assert_eq!(pending.await.unwrap().0, StatusCode::FORBIDDEN);
    let sealed: String = app
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT credential FROM simkl_links WHERE profile_id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let clear = app
        .secret_vault
        .as_ref()
        .unwrap()
        .open(1, "simkl", "1", &sealed)
        .unwrap();
    let stored: Value = serde_json::from_slice(clear.expose()).unwrap();
    assert_eq!(stored["refresh_token"], "fresh-refresh");
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO parent_grants(session_id,expires)VALUES('s1',?1)",
            [util::now() + 120],
        )
        .unwrap();
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/parent/search?query=Movie&profile_id=1",
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
    task.abort();
}

#[tokio::test]
async fn consumed_rotation_survives_temporary_viewing_changes_but_never_link_replacement() {
    for change in [
        "policy",
        "configuration",
        "logout",
        "unlink",
        "delete",
        "relink",
    ] {
        let (mut app, initial, _) = server().await;
        link(&app).await;
        initial.abort();
        expired(&app);
        let (_, configuration) = request(
            &app,
            "member-token-1",
            "GET",
            "/api/profiles/1/integrations",
            Value::Null,
        )
        .await;
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let exchanges = Arc::new(AtomicUsize::new(0));
        let reads = Arc::new(AtomicUsize::new(0));
        let routes=Router::new().route("/oauth2/token",post({let started=started.clone();let release=release.clone();let exchanges=exchanges.clone();move|axum::Form(body):axum::Form<HashMap<String,String>>|{let started=started.clone();let release=release.clone();let exchanges=exchanges.clone();async move{
            assert_eq!(body["refresh_token"],"refresh-private");assert_eq!(exchanges.fetch_add(1,Ordering::SeqCst),0,"A consumed token must never be replayed");started.notify_one();let _permit=release.acquire().await.unwrap();
            axum::Json(json!({"access_token":"fresh-access","refresh_token":"fresh-refresh","token_type":"Bearer","expires_in":604800,"scope":"media:read media:write"}))
        }}})).route("/search/movie",get({let reads=reads.clone();move||{let reads=reads.clone();async move{reads.fetch_add(1,Ordering::SeqCst);axum::Json(json!([{"title":"Movie","ids":{"simkl":10}}]))}}}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
        app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
        let pending = tokio::spawn({
            let app = app.clone();
            async move {
                request(
                    &app,
                    "member-token-1",
                    "GET",
                    "/api/profiles/1/simkl/search?category=movie&query=Movie",
                    Value::Null,
                )
                .await
            }
        });
        tokio::time::timeout(Duration::from_secs(3), started.notified())
            .await
            .unwrap();
        {
            let db = app.db.lock().unwrap();
            match change {
                "policy" => {
                    db.execute("INSERT INTO kids_profiles(profile_id,enabled,max_age,revision)VALUES(1,1,7,1)",[]).unwrap();
                }
                "configuration" => {
                    let mut config = configuration["configuration"].clone();
                    config["scope"]["revision"] = json!(1);
                    for assignment in config["assignments"].as_array_mut().unwrap() {
                        assignment["enabled"]
                            .as_array_mut()
                            .unwrap()
                            .retain(|cap| cap != "Search");
                    }
                    db.execute("INSERT INTO engine_profile_configuration(profile_id,account_id,revision,value)VALUES(1,1,1,?1)",[config.to_string()]).unwrap();
                }
                "logout" => {
                    db.execute("DELETE FROM auth_sessions WHERE id='s1'", [])
                        .unwrap();
                }
                "unlink" => {
                    db.execute("UPDATE simkl_links SET credential=NULL,revision=revision+1 WHERE profile_id=1",[]).unwrap();
                }
                "delete" => {
                    db.execute("DELETE FROM profiles WHERE id=1", []).unwrap();
                }
                "relink" => {
                    let value = json!({"access_token":"other-link-access","refresh_token":"other-link-refresh","scope":"media:read media:write","expires_at":util::now()+3600});
                    let sealed = app
                        .secret_vault
                        .as_ref()
                        .unwrap()
                        .seal(1, "simkl", "1", value.to_string().as_bytes())
                        .unwrap();
                    db.execute(
                        "UPDATE simkl_links SET credential=?1 WHERE profile_id=1",
                        [sealed],
                    )
                    .unwrap();
                }
                _ => unreachable!(),
            }
        }
        release.add_permits(1);
        let (status, _) = tokio::time::timeout(Duration::from_secs(3), pending)
            .await
            .unwrap()
            .unwrap();
        assert!(!status.is_success(), "{change} must block content delivery");
        assert_eq!(reads.load(Ordering::SeqCst), 0, "{change}");
        let sealed: Option<Option<String>> = app
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT credential FROM simkl_links WHERE profile_id=1",
                [],
                |r| r.get(0),
            )
            .optional()
            .unwrap();
        let stored = sealed.flatten().map(|sealed| {
            let clear = app
                .secret_vault
                .as_ref()
                .unwrap()
                .open(1, "simkl", "1", &sealed)
                .unwrap();
            serde_json::from_slice::<Value>(clear.expose()).unwrap()
        });
        if matches!(change, "policy" | "configuration" | "logout") {
            assert_eq!(
                stored.unwrap()["refresh_token"],
                "fresh-refresh",
                "{change}"
            );
            let retry_token = if change == "logout" {
                "replacement-browser"
            } else {
                "member-token-1"
            };
            {
                let db = app.db.lock().unwrap();
                db.execute(
                    "UPDATE kids_profiles SET enabled=0,revision=revision+1 WHERE profile_id=1",
                    [],
                )
                .unwrap();
                db.execute(
                    "DELETE FROM engine_profile_configuration WHERE profile_id=1",
                    [],
                )
                .unwrap();
                if change == "logout" {
                    db.execute("INSERT INTO auth_sessions(id,account_id,profile_id,access_hash,refresh_hash,csrf_hash,kind,device_name,access_expires,refresh_expires,created_at)VALUES('replacement',1,1,?1,'replacement-refresh','unused','browser','synthetic',?2,?2,0)",params![auth::hash(retry_token),util::now()+3600]).unwrap();
                }
            }
            let (status, result) = request(
                &app,
                retry_token,
                "GET",
                "/api/profiles/1/simkl/search?category=movie&query=Movie",
                Value::Null,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{change}: {result}");
            assert_eq!(exchanges.load(Ordering::SeqCst), 1, "{change}");
        } else if change == "relink" {
            assert_eq!(stored.unwrap()["refresh_token"], "other-link-refresh");
        } else {
            assert!(stored.is_none(), "{change}");
        }
        task.abort();
    }
}
