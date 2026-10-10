use super::*;
use crate::test_support::request;
use axum::body::to_bytes;
use std::sync::atomic::{AtomicBool, Ordering};

type Log = Arc<Mutex<Vec<(String, Value, String)>>>;
async fn fixture() -> (App, Log, Arc<AtomicBool>, tokio::task::JoinHandle<()>) {
    let mut app = crate::auth_integration_tests::fixture();
    let log: Log = Default::default();
    let fail = Arc::new(AtomicBool::new(false));
    let records = log.clone();
    let failure = fail.clone();
    let upstream=Router::new().fallback(move |req:Request|{let records=records.clone();let failure=failure.clone();async move {
        let path=req.uri().path().to_owned();let token=req.headers().get(header::AUTHORIZATION).and_then(|v|v.to_str().ok()).unwrap_or("").to_owned();
        let body=to_bytes(req.into_body(),65536).await.unwrap();let value=serde_json::from_slice(&body).unwrap_or(Value::Null);
        records.lock().unwrap().push((path.clone(),value,token));
        if path=="/sync/all-items/shows"&&failure.load(Ordering::Relaxed){return (StatusCode::BAD_REQUEST,axum::Json(json!({"error":"fixture_failed"})));}
        let movie=json!({"title":"Remote movie","year":2020,"ids":{"simkl":42,"imdb":"tt42"},"runtime":100});
        let show=json!({"title":"Remote show","ids":{"simkl":7,"imdb":"tt7"},"runtime":30});
        let response=match path.as_str(){
            "/sync/activities"=>json!({"all":"2026-10-10T12:00:00Z","shows":{"completed":"2026-10-10T12:00:00Z"},"settings":{"all":"2026-10-10T12:00:00Z"},"custom_lists":{"lists":{"all":"2026-10-10T12:00:00Z"}}}),
            "/users/settings"=>json!({"user":{"name":"Fixture user"},"account":{"id":11,"timezone":"America/Detroit","type":"free"}}),
            "/sync/all-items/movies"=>json!({"movies":[{"movie":movie,"status":"completed","last_watched_at":"2026-10-09T12:00:00Z","user_rating":8}]}),
            "/sync/all-items/shows"=>json!({"shows":[{"show":show,"status":"completed","seasons":[{"number":1,"episodes":[{"number":1,"watched_at":"2026-10-09T12:00:00Z"}]}]}]}),
            "/sync/all-items/anime"=>json!({"anime":[]}),
            "/sync/playback"=>json!([]),
            "/sync/history"|"/sync/add-to-list"=>json!({"added":{"movies":1},"not_found":{"movies":[],"shows":[],"episodes":[]}}),
            "/movies/42"=>movie,
            "/tv/7"=>show,
            "/tv/episodes/7"=>json!([{"season":1,"episode":1,"title":"Pilot"}]),
            "/discover/trending/movies/today_500.json"=>json!([movie]),
            "/lists/user/11"|"/lists/1"=>json!({"error":"premium_only","message":"PRO/VIP required"}),
            "/oauth2/token"=>json!({"access_token":"fixture-refreshed","refresh_token":"fixture-refresh","expires_in":604800,"scope":"media:read media:write"}),
            "/oauth2/revoke"=>json!({}),
            "/scrobble/stop"=>json!({"action":"scrobble"}),
            "/scrobble/start"|"/scrobble/pause"=>json!({"action":"pause"}),
            _=>json!({})
        };(StatusCode::OK,axum::Json(response))
    }});
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });
    let service = Service {
        db: app.db.clone(),
        client: Some(
            Client::new("fixture".into(), "fixture-secret".into())
                .unwrap()
                .with_origins(origin.clone(), origin),
        ),
        vault: Some(crate::test_support::vault()),
        gate: Default::default(),
    };
    app.simkl = Arc::new(service);
    app.addons.simkl = Some(app.simkl.clone());
    (app, log, fail, task)
}
fn link(app: &App, p: i64, account: i64, user: &str) {
    let tokens =
        json!({"access_token":format!("fixture-user-{p}"),"refresh_token":"fixture-refresh"});
    let sealed = app
        .simkl
        .vault
        .as_ref()
        .unwrap()
        .seal(
            account,
            "simkl",
            &p.to_string(),
            tokens.to_string().as_bytes(),
        )
        .unwrap();
    app.db.lock().unwrap().execute("INSERT INTO simkl_connections(profile_id,account_id,user_id,user_name,tokens,expires,generation) VALUES(?1,?2,?3,'Fixture',?4,?5,?6)",params![p,account,user,sealed,util::now()+604800,Uuid::new_v4().to_string()]).unwrap();
}
#[tokio::test]
async fn public_discovery_and_metadata_never_contact_addon_catalogs() {
    let (app, log, _, task) = fixture().await;
    let (status, body) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/discover?type=movie",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["metas"][0]["id"], "simkl:movies:42");
    assert_eq!(body["full_search"], false);
    let (status, body) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/meta/movie/simkl:movies:42",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let count = log.lock().unwrap().len();
    request(
        &app,
        "member-token-1",
        "GET",
        "/api/meta/movie/simkl:movies:42",
        Value::Null,
    )
    .await;
    assert_eq!(
        log.lock().unwrap().len(),
        count,
        "persistent cache should prevent refetch"
    );
    assert!(log
        .lock()
        .unwrap()
        .iter()
        .all(|(path, _, _)| !path.contains("/catalog/")
            && !path.contains("/meta/")
            && !path.contains("stremio")));
    let (status, _) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/profiles/1/imports/stremio/preview",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    task.abort();
}

#[tokio::test]
async fn unlinked_title_search_excludes_cached_episodes() {
    let (app, _, _, task) = fixture().await;
    let parent = viptv_simkl::normalize(
        &json!({"title":"Unique show","ids":{"simkl":7}}),
        Category::Tv,
    )
    .unwrap();
    let ep =
        viptv_simkl::episode(&parent, &json!({"season":1,"episode":1,"title":"Pilot"})).unwrap();
    app.simkl.remember(&[parent, ep]).unwrap();
    let result = app
        .simkl
        .discover(
            addon::DiscoverOptions {
                kind: "series".into(),
                catalog: Some("today".into()),
                addon: None,
                skip: 0,
                search: Some("Unique show".into()),
                genre: None,
                extras: HashMap::new(),
            },
            Some(1),
        )
        .await
        .unwrap();
    assert_eq!(result["metas"].as_array().unwrap().len(), 1);
    assert_eq!(result["metas"][0]["id"], "simkl:tv:7");
    task.abort();
}

#[tokio::test]
async fn calendar_date_outside_rolling_window_uses_month_archive() {
    let (app, log, _, task) = fixture().await;
    let result = app
        .simkl
        .discover(
            addon::DiscoverOptions {
                kind: "series".into(),
                catalog: Some("calendar".into()),
                addon: None,
                skip: 0,
                search: None,
                genre: None,
                extras: HashMap::from([("date".into(), "2025-01-12".into())]),
            },
            Some(1),
        )
        .await
        .unwrap();
    assert!(result["metas"].as_array().unwrap().is_empty());
    assert_eq!(log.lock().unwrap()[0].0, "/calendar/v2/2025/1/tv.json");
    task.abort();
}

#[tokio::test]
async fn offline_watchlist_status_backfills_once_when_linked_later() {
    let (app, log, _, task) = fixture().await;
    let item = json!({"id":"simkl:tv:555","type":"series","name":"Local show","simkl_ids":{"simkl":555},"simkl_category":"tv"});
    let (status, body) = request(
        &app,
        "member-token-1",
        "PUT",
        "/api/profiles/1/integrations/simkl/watchlist",
        json!({"item":item,"status":"hold"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, list) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/integrations/simkl/watchlist",
        Value::Null,
    )
    .await;
    assert_eq!(list["metas"][0]["watchlist_status"], "hold");
    assert!(log.lock().unwrap().is_empty());
    link(&app, 1, 1, "11");
    app.simkl.sync(1, true).await.unwrap();
    let writes: Vec<_> = log
        .lock()
        .unwrap()
        .iter()
        .filter(|(p, _, _)| p == "/sync/history")
        .map(|(_, body, _)| body.clone())
        .collect();
    assert!(writes.iter().any(|v| v["shows"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|v| v["ids"]["simkl"] == 555 && v["status"] == "hold")));
    let count = writes.len();
    app.simkl.sync(1, true).await.unwrap();
    assert_eq!(
        log.lock()
            .unwrap()
            .iter()
            .filter(|(p, _, _)| p == "/sync/history")
            .count(),
        count
    );
    task.abort();
}

#[tokio::test]
async fn two_profiles_use_distinct_grants_and_duplicate_identity_is_rejected() {
    let (app, log, _, task) = fixture().await;
    link(&app, 1, 1, "11");
    {
        let db = app.db.lock().unwrap();
        db.execute("INSERT INTO profiles(id,name,avatar_seed,presentation_complete) VALUES(2,'Second','fixture-two',1)",[]).unwrap();
        db.execute("INSERT INTO profile_owners VALUES(2,1,0)", [])
            .unwrap();
    }
    link(&app, 2, 1, "22");
    app.simkl.user_get(1, "/users/settings").await.unwrap();
    app.simkl.user_get(2, "/users/settings").await.unwrap();
    let tokens: Vec<_> = log
        .lock()
        .unwrap()
        .iter()
        .filter(|(p, _, _)| p == "/users/settings")
        .map(|(_, _, t)| t.clone())
        .collect();
    assert_eq!(
        tokens,
        vec!["Bearer fixture-user-1", "Bearer fixture-user-2"]
    );
    let db = app.db.lock().unwrap();
    assert!(db
        .execute(
            "UPDATE simkl_connections SET user_id='11' WHERE profile_id=2",
            []
        )
        .is_err());
    drop(db);
    task.abort();
}

#[tokio::test]
async fn expired_tokens_refresh_once_and_disconnect_preserves_local_history() {
    let (app, log, _, task) = fixture().await;
    link(&app, 1, 1, "11");
    app.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE simkl_connections SET expires=0 WHERE profile_id=1",
            [],
        )
        .unwrap();
    app.simkl.user_get(1, "/users/settings").await.unwrap();
    app.simkl.user_get(1, "/users/settings").await.unwrap();
    assert_eq!(
        log.lock()
            .unwrap()
            .iter()
            .filter(|(p, _, _)| p == "/oauth2/token")
            .count(),
        1
    );
    assert!(log
        .lock()
        .unwrap()
        .iter()
        .filter(|(p, _, _)| p == "/users/settings")
        .all(|(_, _, t)| t == "Bearer fixture-refreshed"));
    app.db.lock().unwrap().execute("INSERT INTO progress(profile_id,id,type,name,position,duration,updated_at,context,title_id) VALUES(1,'tt999','movie','Local',90,100,1000,'{}','tt999')",[]).unwrap();
    let (status, body) = request(
        &app,
        "member-token-1",
        "DELETE",
        "/api/profiles/1/integrations/simkl",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!app.simkl.connected(1));
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM progress WHERE profile_id=1",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    task.abort();
}

#[tokio::test]
async fn remote_removal_does_not_get_uploaded_again_by_backfill() {
    let (app, log, _, task) = fixture().await;
    link(&app, 1, 1, "11");
    app.simkl.sync(1, true).await.unwrap();
    let mut snapshot: Value = serde_json::from_str(
        &app.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT snapshot FROM simkl_connections WHERE profile_id=1",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap(),
    )
    .unwrap();
    snapshot["all"] = json!("2026-10-09T00:00:00Z");
    snapshot["movies"]["removed_from_list"] = json!("2026-10-09T00:00:00Z");
    app.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE simkl_connections SET snapshot=?1 WHERE profile_id=1",
            [snapshot.to_string()],
        )
        .unwrap();
    let before = log
        .lock()
        .unwrap()
        .iter()
        .filter(|(p, _, _)| p == "/sync/history")
        .count();
    app.simkl.sync(1, true).await.unwrap();
    assert_eq!(
        log.lock()
            .unwrap()
            .iter()
            .filter(|(p, _, _)| p == "/sync/history")
            .count(),
        before
    );
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM progress WHERE profile_id=1 AND id='simkl:movies:42'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    task.abort();
}

#[tokio::test]
async fn oauth_state_cannot_be_retargeted_after_profile_switch() {
    let (app, log, _, task) = fixture().await;
    let state = "fixture-state";
    let sealed = app
        .simkl
        .vault
        .as_ref()
        .unwrap()
        .seal(1, "simkl-oauth", &auth::hash(state), b"fixture-verifier")
        .unwrap();
    {
        let db = app.db.lock().unwrap();
        db.execute("INSERT INTO profiles(id,name,avatar_seed,presentation_complete) VALUES(2,'Second','fixture-two',1)",[]).unwrap();
        db.execute("INSERT INTO profile_owners VALUES(2,1,0)", [])
            .unwrap();
        db.execute(
            "INSERT INTO simkl_oauth VALUES(?1,2,1,'s1',?2,?3)",
            params![auth::hash(state), sealed, util::now() + 600],
        )
        .unwrap();
    }
    let (status, _) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/integrations/simkl/callback?state=fixture-state&code=fixture-code",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(log.lock().unwrap().is_empty());
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM simkl_oauth", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    task.abort();
}

#[tokio::test]
async fn legacy_saved_identity_is_consolidated_during_import() {
    let (app, _, _, task) = fixture().await;
    link(&app, 1, 1, "11");
    {
        let db = app.db.lock().unwrap();
        db.execute("INSERT INTO favorites(profile_id,id,type,name,poster) VALUES(1,'tt42','movie','Old title',NULL)",[]).unwrap();
        db.execute("INSERT INTO progress(profile_id,id,type,name,position,duration,updated_at,context,title_id) VALUES(1,'tt42','movie','Old title',90,100,0,'{}','tt42')",[]).unwrap();
    }
    app.simkl.sync(1, true).await.unwrap();
    let db = app.db.lock().unwrap();
    assert_eq!(db.query_row("SELECT count(*) FROM progress WHERE profile_id=1 AND (id='tt42' OR id='simkl:movies:42')",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    assert_eq!(
        db.query_row("SELECT id FROM favorites WHERE profile_id=1", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
        "simkl:movies:42"
    );
    drop(db);
    task.abort();
}

#[tokio::test]
async fn simkl_identity_requests_only_the_mapped_addon_stream_resource() {
    let (mut app, log, _, task) = fixture().await;
    let movie = app
        .simkl
        .client
        .as_ref()
        .unwrap()
        .get("/movies/42", None)
        .await
        .unwrap();
    app.simkl
        .remember(&[viptv_simkl::normalize(&movie, Category::Movie).unwrap()])
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let paths: Arc<Mutex<Vec<String>>> = Default::default();
    let observed = paths.clone();
    let addon = Router::new().fallback(move |req: Request| {
        observed.lock().unwrap().push(req.uri().path().to_owned());
        async { axum::Json(json!({"streams":[]})) }
    });
    let addon_task = tokio::spawn(async move {
        axum::serve(listener, addon).await.unwrap();
    });
    {
        let db = app.db.lock().unwrap();
        db.execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id) VALUES(50,'Streams',?1,?2,1)",params![format!("http://{address}/manifest.json"),json!({"id":"fixture.streams","name":"Streams","resources":["stream","catalog","meta"],"types":["movie"]}).to_string()]).unwrap();
    }
    crate::test_support::encrypt_fixture_sources(&app);
    app.addons.allow_test_loopback = true;
    let (status, body) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/streams",
        json!({"id":"simkl:movies:42","type":"movie","name":"Remote movie","only_addons":true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let id = body["id"].as_str().unwrap();
    for _ in 0..20 {
        if !paths.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    request(
        &app,
        "member-token-1",
        "GET",
        &format!("/api/v2/streams/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(*paths.lock().unwrap(), vec!["/stream/movie/tt42.json"]);
    assert!(log
        .lock()
        .unwrap()
        .iter()
        .all(|(p, _, _)| !p.contains("/catalog/")));
    addon_task.abort();
    task.abort();
}

#[tokio::test]
async fn missing_addon_mapping_does_not_block_iptv_and_proof_keeps_simkl_identity() {
    let (app, _, _, task) = fixture().await;
    let item = viptv_simkl::normalize(
        &json!({"title":"IPTV title","year":2020,"ids":{"simkl":999}}),
        Category::Movie,
    )
    .unwrap();
    app.simkl.remember(&[item]).unwrap();
    let (status, body) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/streams",
        json!({"id":"simkl:movies:999","type":"movie","name":"IPTV title","year":2020}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let job = app
        .jobs
        .lock()
        .unwrap()
        .get(body["id"].as_str().unwrap())
        .unwrap()
        .clone();
    assert_eq!(job.exact_vod.as_ref().unwrap().title, "simkl:movies:999");
    let (status, _) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/streams",
        json!({"id":"simkl:movies:999","type":"movie","name":"IPTV title","only_addons":true}),
    )
    .await;
    assert_ne!(status, StatusCode::OK);
    task.abort();
}
#[tokio::test]
async fn initial_merge_uploads_only_missing_history_and_repeated_sync_is_stable() {
    let (app, log, _, task) = fixture().await;
    link(&app, 1, 1, "11");
    app.db.lock().unwrap().execute("INSERT INTO progress(profile_id,id,type,name,position,duration,updated_at,context,title_id) VALUES(1,'tt999','movie','Local movie',100,100,1000,'{}','tt999')",[]).unwrap();
    let result = app.simkl.sync(1, true).await.unwrap();
    assert_eq!(result["imported"], 2);
    assert_eq!(result["exported"], 1);
    let pulls: Vec<_> = log
        .lock()
        .unwrap()
        .iter()
        .filter(|(p, _, _)| p.starts_with("/sync/all-items/"))
        .map(|(p, _, _)| p.clone())
        .collect();
    assert_eq!(
        pulls,
        vec![
            "/sync/all-items/shows",
            "/sync/all-items/movies",
            "/sync/all-items/anime"
        ]
    );
    let rows: i64 = app
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM progress WHERE profile_id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let writes = log
        .lock()
        .unwrap()
        .iter()
        .filter(|(p, _, _)| p == "/sync/history")
        .count();
    app.simkl.sync(1, true).await.unwrap();
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM progress WHERE profile_id=1",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        rows
    );
    assert_eq!(
        log.lock()
            .unwrap()
            .iter()
            .filter(|(p, _, _)| p == "/sync/history")
            .count(),
        writes
    );
    task.abort();
}
#[tokio::test]
async fn incomplete_pull_does_not_advance_snapshot() {
    let (app, _, fail, task) = fixture().await;
    link(&app, 1, 1, "11");
    fail.store(true, Ordering::Relaxed);
    assert!(app.simkl.sync(1, true).await.is_err());
    assert!(app
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT snapshot FROM simkl_connections WHERE profile_id=1",
            [],
            |r| r.get::<_, Option<String>>(0)
        )
        .unwrap()
        .is_none());
    task.abort();
}
#[tokio::test]
async fn profiles_are_authorized_and_premium_200_is_preserved() {
    let (app, log, _, task) = fixture().await;
    link(&app, 1, 1, "11");
    let (status, _) = request(
        &app,
        "member-token-2",
        "GET",
        "/api/profiles/1/integrations/simkl",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/integrations/simkl/lists",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["error"], "premium_only");
    assert!(log
        .lock()
        .unwrap()
        .iter()
        .filter(|(p, _, _)| p.starts_with("/lists"))
        .all(|(_, _, t)| t == "Bearer fixture-user-1"));
    task.abort();
}
#[tokio::test]
async fn playback_events_are_deduplicated_and_progress_saves_do_not_scrobble() {
    let (app, log, _, task) = fixture().await;
    link(&app, 1, 1, "11");
    let item =
        json!({"id":"simkl:movies:42","type":"movie","name":"Movie","simkl_ids":{"simkl":42}});
    for (i, action, position) in [
        (1, "start", 0),
        (2, "pause", 30),
        (3, "start", 30),
        (4, "stop", 90),
    ] {
        let event = json!({"item":item,"event_id":format!("event-{i}"),"session_id":"session-1","action":action,"position":position,"duration":100});
        let (status, body) = request(
            &app,
            "member-token-1",
            "POST",
            "/api/profiles/1/integrations/simkl/playback",
            event.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/profiles/1/integrations/simkl/playback",
            event,
        )
        .await;
    }
    let calls: Vec<_> = log
        .lock()
        .unwrap()
        .iter()
        .filter(|(p, _, _)| p.starts_with("/scrobble/"))
        .map(|(p, _, _)| p.clone())
        .collect();
    assert_eq!(
        calls,
        vec![
            "/scrobble/start",
            "/scrobble/pause",
            "/scrobble/start",
            "/scrobble/stop"
        ]
    );
    task.abort();
}
