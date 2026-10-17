use super::*;

#[tokio::test]
async fn paused_episode_import_requires_episode_id_and_aired_mapping_to_agree() {
    for (category, mapped_episode, expected_history) in
        [("anime", 1, 0), ("anime", 2, 1), ("tv", 1, 0), ("tv", 2, 1)]
    {
        let anime = category == "anime";
        let (mut app, original, _) = server().await;
        link(&app).await;
        original.abort();
        let writes = Arc::new(AtomicUsize::new(0));
        let count = writes.clone();
        let mut items = json!({});
        items[if anime { "anime" } else { "shows" }] = json!([{
            "status":"watching","show":{"title":"Fixture series","genres":["Drama"],"runtime":24,"ids":{"simkl":30}}
        }]);
        let mut pause = json!({"progress":50,"paused_at":"2020-01-02T00:00:00Z",
            "episode":{"number":if anime{2}else{mapped_episode},"season":1,"ids":{"simkl":302},"tvdb_season":1,"tvdb_number":mapped_episode}});
        pause[if anime { "anime" } else { "show" }] = json!({"ids":{"simkl":30}});
        let routes=Router::new()
            .route("/sync/activities",get(||async{axum::Json(json!({"all":"2020-01-02T00:00:00Z"}))}))
            .route("/sync/all-items",get(move||{let items=items.clone();async move{axum::Json(items)}}))
            .route("/sync/playback",get(move||{let pause=pause.clone();async move{axum::Json(json!([pause]))}}))
            .route(&format!("/{category}/30"),get(||async{axum::Json(json!({"title":"Fixture series","runtime":24,"ids":{"simkl":30}}))}))
            .route(&format!("/{category}/episodes/30"),get(||async{axum::Json(json!([
                {"title":"First","type":"episode","episode":1,"season":1,"aired":true,"ids":{"simkl":301},"tvdb":{"season":1,"episode":1}},
                {"title":"Second","type":"episode","episode":2,"season":1,"aired":true,"ids":{"simkl":302},"tvdb":{"season":1,"episode":2}}
            ]))}))
            .fallback(move|method:axum::http::Method|{let count=count.clone();async move{
                if method!=axum::http::Method::GET{count.fetch_add(1,Ordering::SeqCst);}StatusCode::NOT_FOUND
            }});
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
        app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
        assert_eq!(
            request(
                &app,
                "member-token-1",
                "GET",
                "/api/profiles/1/library",
                Value::Null
            )
            .await
            .0,
            StatusCode::OK
        );
        let status = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let (status, value) = request(
                    &app,
                    "member-token-1",
                    "GET",
                    "/api/profiles/1/imports/simkl",
                    Value::Null,
                )
                .await;
                assert_eq!(status, StatusCode::OK);
                if value["running"] == false {
                    break value;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(status["availability"], "Available");
        assert_eq!(
            status["imported_history"], expected_history,
            "{category}: conflicting provider evidence must not choose the first episode"
        );
        assert_eq!(status["unresolved"], 1 - expected_history);
        let db = app.db.lock().unwrap();
        let rows = db
            .prepare("SELECT id,position FROM progress WHERE profile_id=1 ORDER BY id")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        if expected_history == 0 {
            assert!(rows.is_empty());
        } else {
            assert_eq!(rows, vec![(format!("simkl:{category}:30:1:2"), 720.0)]);
        }
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM progress WHERE profile_id!=1",
                [],
                |row| row.get::<_, u64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM simkl_tracking_receipts", [], |row| {
                row.get::<_, u64>(0)
            })
            .unwrap(),
            0
        );
        assert_eq!(writes.load(Ordering::SeqCst), 0);
        task.abort();
    }
}
