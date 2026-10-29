use super::*;
#[tokio::test]
async fn linked_library_uses_one_private_read_real_user_stats_and_never_writes_watchlist() {
    let (mut app, initial, writes) = server().await;
    link(&app).await;
    initial.abort();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let routes=Router::new().route("/sync/activities",get(||async {axum::Json(json!({"all":"2026-01-01T00:00:00Z"}))})).route("/sync/all-items",get(move |headers:axum::http::HeaderMap|{let count=count.clone();async move{assert_eq!(headers[header::AUTHORIZATION],"Bearer fixture-private-token");count.fetch_add(1,Ordering::SeqCst);axum::Json(json!({"movies":[{"status":"completed","user_rating":9,"added_to_watchlist_at":"2026-01-01T00:00:00Z","last_watched_at":"2026-01-02T00:00:00Z","memo":{"text":"private memo never returned"},"movie":{"title":"Movie","genres":["Drama"],"ids":{"simkl":10},"ratings":{"imdb":{"rating":4.0}}}}],"shows":[{"status":"watching","user_rating":null,"watched_episodes_count":2,"total_episodes_count":5,"total_aired_episodes_count":3,"next_to_watch_info":{"season":1,"episode":3,"date":"2020-01-01T00:00:00Z"},"show":{"title":"Show","genres":["Comedy"],"ids":{"simkl":20}}}],"anime":[]}))}}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
    let (status, page) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/library?source=simkl&rated=rated_8_plus",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["cards"].as_array().unwrap().len(), 1);
    assert_eq!(page["statistics"][0]["user_rating"].as_f64(), Some(9.0));
    assert_eq!(page["cards"][0]["metadata"]["rating"], 4.0);
    assert!(!page.to_string().contains("private memo"));
    let (status, page) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/library?source=simkl&show=has_new_episode&episodes_left_max=9",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["cards"][0]["identity"]["id"], "simkl:tv:20");
    assert_eq!(page["statistics"][0]["next_episode"]["episode"], 3);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(writes.load(Ordering::SeqCst), 0);
    assert_eq!(
        request(
            &app,
            "member-token-2",
            "GET",
            "/api/profiles/1/library?source=simkl",
            json!({})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "DELETE",
            "/api/profiles/1/simkl",
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (status, page) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/library?source=simkl",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["availability"], "NeedsAuthorization");
    assert!(page["cards"].as_array().unwrap().is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    task.abort();
}

async fn expire(app: &App) {
    for entry in app.simkl.watchlists.lock().await.values() {
        if let Some(snapshot) = entry.lock().await.as_mut() {
            snapshot.checked = Instant::now() - Duration::from_secs(61);
        }
    }
}
#[tokio::test]
async fn expired_private_snapshot_checks_activities_merges_deltas_ratings_and_removals() {
    let (mut app, initial, _) = server().await;
    link(&app).await;
    initial.abort();
    let phase = Arc::new(AtomicUsize::new(0));
    let pulls = Arc::new(AtomicUsize::new(0));
    let activities = Arc::new(AtomicUsize::new(0));
    let p = phase.clone();
    let a = activities.clone();
    let p2 = phase.clone();
    let reads = pulls.clone();
    let routes=Router::new().route("/sync/activities",get(move ||{let p=p.clone();let a=a.clone();async move{a.fetch_add(1,Ordering::SeqCst);let n=if p.load(Ordering::SeqCst)==0{"2026-01-01T00:00:00Z"}else{"2026-01-02T00:00:00Z"};axum::Json(json!({"all":n,"movies":{"removed_from_list":n,"rated_at":n},"tv_shows":{"all":n}}))}}))
 .route("/sync/all-items",get(move |axum::extract::Query(q):axum::extract::Query<HashMap<String,String>>|{let p=p2.clone();let reads=reads.clone();async move{reads.fetch_add(1,Ordering::SeqCst);let item=json!({"status":"watching","user_rating":null,"show":{"title":"Show","ids":{"simkl":20}}});axum::Json(if p.load(Ordering::SeqCst)==0 {assert!(!q.contains_key("date_from"));json!({"movies":[{"status":"completed","movie":{"title":"Movie","ids":{"simkl":10}}}],"shows":null,"anime":null})}else if q.get("extended").map(String::as_str)==Some("simkl_ids_only"){json!({"shows":[item]})}else{assert_eq!(q["date_from"],"2026-01-01T00:00:00Z");json!({"shows":[item]})})}}))
 .route("/sync/ratings",get(||async{axum::Json(json!({"shows":[{"show":{"ids":{"simkl":20}},"user_rating":9}]}))}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
    let path = "/api/profiles/1/library?source=simkl";
    assert_eq!(
        request(&app, "member-token-1", "GET", path, json!({}))
            .await
            .1["cards"][0]["identity"]["id"],
        "simkl:movie:10"
    );
    expire(&app).await;
    assert_eq!(
        request(&app, "member-token-1", "GET", path, json!({}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(pulls.load(Ordering::SeqCst), 1);
    assert_eq!(activities.load(Ordering::SeqCst), 2);
    phase.store(1, Ordering::SeqCst);
    expire(&app).await;
    let (status, page) = request(&app, "member-token-1", "GET", path, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["cards"].as_array().unwrap().len(), 2);
    let show=page["statistics"].as_array().unwrap().iter().find(|s|s["identity"]["id"]=="simkl:tv:20").unwrap();
    assert_eq!(show["user_rating"], 9.0);
    assert!(show["times_watched"].is_null());
    assert_eq!(pulls.load(Ordering::SeqCst), 3);
    task.abort();
}
#[tokio::test]
async fn profile_switch_during_private_read_discards_the_entire_response() {
    let (mut app, initial, _) = server().await;
    link(&app).await;
    initial.abort();
    let entered = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    let start = entered.clone();
    let finish = resume.clone();
    let routes=Router::new().route("/sync/activities",get(||async{axum::Json(json!({"all":"2026-01-01T00:00:00Z"}))})).route("/sync/all-items",get(move ||{let start=start.clone();let finish=finish.clone();async move{start.notify_one();finish.notified().await;axum::Json(json!({"movies":[{"status":"completed","movie":{"title":"Private","ids":{"simkl":10}}}]}))}}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
    let cloned = app.clone();
    let pending = tokio::spawn(async move {
        request(
            &cloned,
            "member-token-1",
            "GET",
            "/api/profiles/1/library?source=simkl",
            json!({}),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE auth_sessions SET profile_id=NULL WHERE id='s1'", [])
        .unwrap();
    resume.notify_one();
    let (status, page) = pending.await.unwrap();
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(page.get("cards").is_none());
    task.abort();
}
#[tokio::test]
async fn genre_enrichment_runs_in_background_and_reports_partial_matches_until_verified(){
 let (mut app,initial,_)=server().await;link(&app).await;initial.abort();let gate=Arc::new(tokio::sync::Semaphore::new(0));let reads=Arc::new(AtomicUsize::new(0));let g=gate.clone();let r=reads.clone();
 let routes=Router::new().route("/sync/activities",get(||async{axum::Json(json!({"all":null}))})).route("/sync/all-items",get(||async{axum::Json(json!({"shows":null,"anime":null,"movies":[{"status":"completed","movie":{"title":"Movie","ids":{"simkl":10}}}]}))})).route("/movies/10",get(move |headers:axum::http::HeaderMap|{let g=g.clone();let r=r.clone();async move{assert!(!headers.contains_key(header::AUTHORIZATION));r.fetch_add(1,Ordering::SeqCst);let _permit=g.acquire().await.unwrap();axum::Json(json!({"title":"Movie","ids":{"simkl":10},"genres":["Drama"]}))}}));
 let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();let task=tokio::spawn(async move{axum::serve(listener,routes).await.unwrap()});app.simkl=Arc::new(Service::fixture(&format!("http://{address}")));let path="/api/profiles/1/library?source=simkl";
 let (status,page)=request(&app,"member-token-1","GET",path,json!({})).await;assert_eq!(status,StatusCode::OK);assert_eq!(page["cards"].as_array().unwrap().len(),1);assert!(page["reason"].as_str().unwrap().contains("0 of 1"));assert_eq!(page["refresh_after_millis"],4000);
 let (status,partial)=request(&app,"member-token-1","GET",&format!("{path}&genre=Drama"),json!({})).await;assert_eq!(status,StatusCode::OK);assert_eq!(partial["availability"],"TemporarilyUnavailable");assert!(partial["cards"].as_array().unwrap().is_empty());gate.add_permits(1);
 tokio::time::timeout(Duration::from_secs(3),async{loop{let (_,page)=request(&app,"member-token-1","GET",&format!("{path}&genre=Drama"),json!({})).await;if page["availability"]=="Available"&&page["cards"].as_array().is_some_and(|cards|cards.len()==1){assert!(page["reason"].is_null());break;}tokio::time::sleep(Duration::from_millis(10)).await;}}).await.unwrap();assert_eq!(reads.load(Ordering::SeqCst),1);
 app.simkl.watchlists.lock().await.clear();let(status,warm)=request(&app,"member-token-1","GET",&format!("{path}&genre=Drama"),json!({})).await;assert_eq!(status,StatusCode::OK);assert_eq!(warm["availability"],"Available");assert!(warm["reason"].is_null());assert!(warm["refresh_after_millis"].is_null());assert_eq!(warm["cards"].as_array().unwrap().len(),1);assert_eq!(reads.load(Ordering::SeqCst),1);task.abort();
}
#[tokio::test]
async fn unavailable_metadata_stops_aggregate_refresh_when_the_worker_finishes(){
 let (mut app,initial,_)=server().await;link(&app).await;initial.abort();
 let routes=Router::new().route("/sync/activities",get(||async{axum::Json(json!({"all":null}))})).route("/sync/all-items",get(||async{axum::Json(json!({"movies":[{"status":"completed","movie":{"title":"Movie","ids":{"simkl":10}}}]}))})).route("/movies/10",get(||async{StatusCode::BAD_GATEWAY}));
 let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();let task=tokio::spawn(async move{axum::serve(listener,routes).await.unwrap()});app.simkl=Arc::new(Service::fixture(&format!("http://{address}")));let path="/api/profiles/1/library?source=simkl&genre=Drama";
 let(status,page)=request(&app,"member-token-1","GET",path,json!({})).await;assert_eq!(status,StatusCode::OK);assert_eq!(page["refresh_after_millis"],4000);
 tokio::time::timeout(Duration::from_secs(3),async{loop{let(status,page)=request(&app,"member-token-1","GET",path,json!({})).await;assert_eq!(status,StatusCode::OK);if page["refresh_after_millis"].is_null(){assert_eq!(page["availability"],"TemporarilyUnavailable");assert!(page["reason"].as_str().unwrap().contains("incomplete"));break;}tokio::time::sleep(Duration::from_millis(10)).await;}}).await.unwrap();task.abort();
}

#[tokio::test]
async fn unified_library_imports_membership_original_history_and_verified_pauses_without_export() {
    let (mut app,initial,_)=server().await;link(&app).await;initial.abort();
    let reads=Arc::new(AtomicUsize::new(0));let copies=reads.clone();
    let routes=Router::new()
      .route("/sync/activities",get(||async{axum::Json(json!({"all":"2026-01-02T00:00:00Z"}))}))
      .route("/sync/all-items",get(move |axum::extract::Query(query):axum::extract::Query<HashMap<String,String>>|{let reads=copies.clone();async move{assert_eq!(query["extended"],"full_anime_seasons");assert_eq!(query["include_all_episodes"],"original");assert_eq!(query["episode_watched_at"],"yes");reads.fetch_add(1,Ordering::SeqCst);axum::Json(json!({"movies":[{"status":"completed","last_watched_at":"2026-01-01T00:00:00Z","user_rating":9,"movie":{"title":"Movie","genres":["Drama"],"runtime":120,"ids":{"simkl":10,"imdb":"tt1234567"}}}],"shows":[{"status":"watching","show":{"title":"Show","genres":["Comedy"],"runtime":24,"ids":{"simkl":20}},"seasons":[{"number":1,"episodes":[{"number":1,"runtime":24,"watched_at":"2026-01-01T00:00:00Z"}]}]}]}))}}))
      .route("/sync/playback",get(||async{axum::Json(json!([{"progress":50,"paused_at":"2026-01-02T00:00:00Z","type":"episode","show":{"ids":{"simkl":20}},"episode":{"season":1,"number":2}}]))}))
      .route("/tv/20",get(||async{axum::Json(json!({"title":"Show","runtime":24,"ids":{"simkl":20,"imdb":"tt7654321"}}))}))
      .route("/tv/episodes/20",get(||async{axum::Json(json!([{"title":"Second","season":1,"episode":2,"ids":{"simkl":201}}]))}));
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();let task=tokio::spawn(async move{axum::serve(listener,routes).await.unwrap()});app.simkl=Arc::new(Service::fixture(&format!("http://{address}")));
    let (status,initial)=request(&app,"member-token-1","GET","/api/profiles/1/library",json!({})).await;assert_eq!(status,StatusCode::OK);assert_eq!(initial["refresh_after_millis"],4000);
    let mut terminal=Value::Null;
    for _ in 0..100 {let (status,state)=request(&app,"member-token-1","GET","/api/profiles/1/imports/simkl",json!({})).await;assert_eq!(status,StatusCode::OK);if state["running"]==false{terminal=state;break;}tokio::time::sleep(Duration::from_millis(20)).await;}
    assert_eq!(terminal["availability"],"Available","{terminal}");assert_eq!(terminal["added_titles"],2);assert_eq!(terminal["imported_history"],3);assert!(terminal["library_revision"].as_u64().unwrap()>0);
    let (status,library)=request(&app,"member-token-1","GET","/api/profiles/1/library",json!({})).await;assert_eq!(status,StatusCode::OK);assert_eq!(library["cards"].as_array().unwrap().len(),2);assert_eq!(library["sources"].as_array().unwrap().len(),1);assert_eq!(library["query"]["source"],"favorites");assert!(library["controls"].as_array().unwrap().iter().all(|c|c["id"]!="source"));assert_eq!(library["statistics"][0]["provenance"][0]["provider_id"],"simkl");
    let db=app.db.lock().unwrap();let row:(f64,f64)=db.query_row("SELECT position,duration FROM progress WHERE profile_id=1 AND id='simkl:tv:20:1:2'",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();assert_eq!(row,(720.0,1440.0));assert_eq!(db.query_row("SELECT COUNT(*) FROM simkl_tracking_receipts",[],|r|r.get::<_,u64>(0)).unwrap(),0);assert_eq!(reads.load(Ordering::SeqCst),1);drop(db);task.abort();
}
