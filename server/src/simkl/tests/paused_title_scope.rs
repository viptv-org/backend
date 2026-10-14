use super::*;

#[tokio::test]
async fn paused_import_scopes_equal_numeric_ids_to_the_explicit_title_category() {
    for (wrapper, expected) in [
        ("show", "simkl:tv:30:1:2"),
        ("anime", "simkl:anime:30:2:1"),
        ("movie", "simkl:movie:30"),
        ("anime-absolute", "simkl:anime:30:2:1"),
        ("anime-conflict", ""),
        ("anime-id-alias", "simkl:anime:30:2:1"),
        ("anime-ambiguous", ""),
        ("multiple-wrappers", ""),
    ] {
        let (mut app, original, _) = server().await;
        link(&app).await;
        original.abort();
        app.db.lock().unwrap().execute_batch("INSERT INTO profiles(id,name) VALUES(2,'Other');INSERT INTO progress(profile_id,id,type,name,position,duration,updated_at,context,title_id) VALUES(1,'local-unrelated','movie','Local',123,400,2000000000,'{\"local\":true}','local-unrelated'),(2,'simkl:anime:30:2:1','series','Other profile',999,1440,2000000000,'{\"other\":true}','simkl:anime:30')").unwrap();
        let writes = Arc::new(AtomicUsize::new(0));
        let count = writes.clone();
        let mut pause = json!({"progress":50,"paused_at":"2020-01-02T00:00:00Z"});
        pause[if wrapper.starts_with("anime") {
            "anime"
        } else {
            wrapper
        }] = json!({"ids":{"simkl":30}});
        if wrapper != "movie" {
            pause["episode"] = if wrapper == "anime-absolute" {
                json!({"number":52,"season":1})
            } else if wrapper.starts_with("anime") {
                json!({"number":52,"season":1,"ids":{"simkl":303},"tvdb_season":2,"tvdb_number":1})
            } else {
                json!({"number":2,"season":1,"ids":{"simkl":302}})
            };
        }
        if wrapper == "anime-conflict" {
            pause["episode"]["number"] = json!(53);
        }
        if wrapper == "anime-id-alias" {
            pause["anime"]["ids"] = json!({"simkl_id":30});
            pause["episode"]["ids"] = json!({"simkl_id":303});
        }
        if wrapper == "multiple-wrappers" {
            pause = json!({"progress":50,"paused_at":"2020-01-02T00:00:00Z","movie":{"ids":{"simkl":30}},"show":{"ids":{"simkl":30}}});
        }
        let mut episodes = json!([{"type":"episode","episode":52,"aired":true,"ids":{"simkl":303},"tvdb":{"season":2,"episode":1}}]);
        if wrapper == "anime-ambiguous" {
            episodes.as_array_mut().unwrap().push(json!({"type":"episode","episode":52,"aired":true,"ids":{"simkl":304},"tvdb":{"season":2,"episode":2}}));
        }
        let routes = Router::new()
            .route("/sync/activities",get(||async{axum::Json(json!({"all":"2020-01-02T00:00:00Z"}))}))
            .route("/sync/all-items",get(||async{axum::Json(json!({
                "movies":[{"status":"plantowatch","movie":{"title":"Different movie","runtime":120,"genres":["Drama"],"ids":{"simkl":30}}}],
                "shows":[{"status":"watching","show":{"title":"Different TV show","runtime":24,"genres":["Drama"],"ids":{"simkl":30}}}],
                "anime":[{"status":"watching","show":{"title":"Different anime","runtime":24,"genres":["Drama"],"ids":{"simkl":30}}}]
            }))}))
            .route("/sync/playback",get(move||{let pause=pause.clone();async move{axum::Json(json!([pause]))}}))
            .route("/movies/30",get(||async{axum::Json(json!({"title":"Different movie","runtime":120,"ids":{"simkl":30}}))}))
            .route("/tv/30",get(||async{axum::Json(json!({"title":"Different TV show","runtime":24,"ids":{"simkl":30}}))}))
            .route("/anime/30",get(||async{axum::Json(json!({"title":"Different anime","runtime":24,"ids":{"simkl":30}}))}))
            .route("/tv/episodes/30",get(||async{axum::Json(json!([{"episode":2,"season":1,"aired":true,"ids":{"simkl":302}}]))}))
            .route("/anime/episodes/30",get(move||{let episodes=episodes.clone();async move{axum::Json(episodes)}}))
            .fallback(move|method:axum::http::Method|{let count=count.clone();async move{if method!=axum::http::Method::GET{count.fetch_add(1,Ordering::SeqCst);}StatusCode::NOT_FOUND}});
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
        app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
        assert_eq!(
            request(
                &app,
                "member-token-1",
                "GET",
                "/api/v3/profiles/1/library",
                Value::Null
            )
            .await
            .0,
            StatusCode::OK
        );
        let status = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let (code, value) = request(
                    &app,
                    "member-token-1",
                    "GET",
                    "/api/v3/profiles/1/imports/simkl",
                    Value::Null,
                )
                .await;
                assert_eq!(code, StatusCode::OK);
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
            status["imported_history"],
            if expected.is_empty() { 0 } else { 1 },
            "{wrapper}: {status}"
        );
        let db = app.db.lock().unwrap();
        let rows=db.prepare("SELECT id,position,duration FROM progress WHERE profile_id=1 AND id!='local-unrelated' ORDER BY id").unwrap().query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,f64>(1)?,row.get::<_,f64>(2)?))).unwrap().collect::<Result<Vec<_>,_>>().unwrap();
        let duration = if wrapper == "movie" { 7200.0 } else { 1440.0 };
        assert_eq!(
            rows,
            if expected.is_empty() {
                vec![]
            } else {
                vec![(expected.to_owned(), duration / 2.0, duration)]
            },
            "{wrapper} must never select another category's equal numeric ID"
        );
        for (profile, id, position, context) in [
            (1, "local-unrelated", 123.0, "{\"local\":true}"),
            (2, "simkl:anime:30:2:1", 999.0, "{\"other\":true}"),
        ] {
            assert_eq!(
                db.query_row(
                    "SELECT position,context FROM progress WHERE profile_id=?1 AND id=?2",
                    params![profile, id],
                    |r| Ok((r.get::<_, f64>(0)?, r.get::<_, String>(1)?))
                )
                .unwrap(),
                (position, context.to_owned())
            );
        }
        assert_eq!(
            db.query_row("SELECT count(*) FROM simkl_tracking_receipts", [], |r| r
                .get::<_, u64>(0))
                .unwrap(),
            0
        );
        assert_eq!(writes.load(Ordering::SeqCst), 0);
        task.abort();
    }
}
