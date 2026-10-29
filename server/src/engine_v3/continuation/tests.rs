use super::*;
use crate::{auth_integration_tests::fixture, test_support::request};
fn identity(id: &str, episode: bool) -> Value {
    json!({"id":id,"kind":"series","ids":[],"episode":if episode{json!({"order":"aired","season":1,"episode":1238})}else{Value::Null},"edition":null})
}
async fn progress(app: &App) {
    let(status,value)=request(app,"member-token-1","PUT","/api/profiles/1/progress",json!({"record":{"identity":identity("opaque-child-1238",true),"parent_identity":identity("opaque-parent",false),"provider_id":"stremio","title":"Owned series","poster":null,"landscape":null,"position_millis":30000,"duration_millis":100000,"updated_at_millis":1,"completed_at_millis":null}})).await;
    assert_eq!(status, StatusCode::OK, "{value}");
}
#[tokio::test]
async fn typed_queue_and_bootstrap_retain_exact_resume_clock_and_visibility_is_title_scoped() {
    let app = fixture();
    progress(&app).await;
    let path = "/api/profiles/1/continue";
    let (status, page) = request(&app, "member-token-1", "GET", path, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    let entry = &page["entries"][0];
    assert_eq!(entry["status"], "Resume");
    assert_eq!(entry["record"]["position_millis"], 30000);
    assert_eq!(entry["record"]["duration_millis"], 100000);
    assert_eq!(entry["card"]["parent_identity"]["id"], "opaque-parent");
    assert_eq!(entry["card"]["progress"], 0.3);
    let (status, boot) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/bootstrap",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        boot["continuation"]["entries"][0]["record"],
        entry["record"]
    );
    let body = json!({"identity":identity("opaque-child-1238",true),"parent_identity":identity("opaque-parent",false),"hidden":true});
    let (status, receipt) = request(
        &app,
        "member-token-1",
        "PUT",
        "/api/profiles/1/continue/visibility",
        body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    assert_eq!(receipt["identity"]["id"], "opaque-parent");
    assert!(request(&app, "member-token-1", "GET", path, Value::Null)
        .await
        .1["entries"]
        .as_array()
        .unwrap()
        .is_empty());
    let (status, undo) = request(
        &app,
        "member-token-1",
        "PUT",
        "/api/profiles/1/continue/visibility",
        json!({"identity":receipt["identity"],"hidden":false}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{undo}");
    assert_eq!(
        request(&app, "member-token-1", "GET", path, Value::Null)
            .await
            .1["entries"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let mut forged = body;
    forged["parent_identity"]["id"] = json!("wrong-parent");
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/profiles/1/continue/visibility",
            forged
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(&app, "member-token-2", "GET", path, Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/profiles/1/continue?limit=101",
            Value::Null
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/profiles/1/history",
            Value::Null
        )
        .await
        .1
        .as_array()
        .unwrap()
        .len(),
        1
    );
}
#[tokio::test]
async fn generic_next_uses_one_large_provider_document_opaque_ids_and_never_adds_history() {
    let mut app = fixture();
    app.addons.allow_test_loopback = true;
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = calls.clone();
    let videos=(1..=1242).map(|n|json!({"id":format!("opaque-child-{n}"),"title":format!("Episode {n}"),"season":1,"episode":n,"released":"2020-01-01T00:00:00Z"})).collect::<Vec<_>>();
    let routes=Router::new().route("/meta/series/opaque-parent.json",axum::routing::get(move||{let count=count.clone();let videos=videos.clone();async move{count.fetch_add(1,std::sync::atomic::Ordering::SeqCst);axum::Json(json!({"meta":{"id":"opaque-parent","type":"series","name":"Actual provider series","videos":videos}}))}}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    app.db.lock().unwrap().execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id) VALUES(1,'Metadata',?1,?2,1)",params![format!("http://{address}/manifest.json"),json!({"id":"metadata","name":"Metadata","version":"1","resources":["meta"],"types":["series"]}).to_string()]).unwrap();
    crate::test_support::encrypt_fixture_sources(&app);
    let body = json!({"identity":identity("opaque-child-1238",true),"parent_identity":identity("opaque-parent",false)});
    let (status, next) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/profiles/1/continue/next",
        body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{next}");
    assert_eq!(next["status"], "Next");
    assert_eq!(next["item"]["identity"]["id"], "opaque-child-1239");
    assert_eq!(next["item"]["parent_identity"]["id"], "opaque-parent");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let db = app.db.lock().unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM progress", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM simkl_tracking_receipts", [], |r| r
            .get::<_, u64>(0))
            .unwrap(),
        0
    );
    drop(db);
    let mut wrong = body;
    wrong["identity"]["episode"]["episode"] = json!(4);
    let (status, result) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/profiles/1/continue/next",
        wrong,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["status"], "Unavailable");
    let(status,_)=request(&app,"member-token-1","PUT","/api/profiles/1/progress",json!({"ended":true,"record":{"identity":identity("opaque-child-1238",true),"parent_identity":identity("opaque-parent",false),"provider_id":"stremio","title":"Actual series","poster":null,"landscape":null,"position_millis":100000,"duration_millis":100000,"updated_at_millis":1,"completed_at_millis":null}})).await;
    assert_eq!(status, StatusCode::OK);
    let (status, page) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/continue",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let entry = &page["entries"][0];
    assert_eq!(entry["status"], "Next");
    assert_eq!(entry["card"]["identity"]["id"], "opaque-child-1239");
    assert!(entry["card"]["progress"].is_null());
    assert_eq!(entry["record"]["identity"]["id"], "opaque-child-1238");
    assert_eq!(entry["record"]["position_millis"], 100000);
    assert_eq!(entry["record"]["duration_millis"], 100000);
    assert_eq!(entry["previous_identity"]["id"], "opaque-child-1238");
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM progress WHERE id='opaque-child-1239'",
                [],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        0
    );
    task.abort();
}
#[tokio::test]
async fn queue_visibility_rejects_unknown_and_restricted_identities_without_writes() {
    let app = fixture();
    let body = json!({"identity":identity("not-owned",true),"parent_identity":identity("opaque-parent",false),"hidden":true});
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/profiles/1/continue/visibility",
            body.clone()
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    progress(&app).await;
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO kids_profiles(profile_id,enabled,max_age,revision) VALUES(1,1,12,1)",
            [],
        )
        .unwrap();
    let (status, restricted) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/continue",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{restricted}");
    assert!(restricted["entries"].as_array().unwrap().is_empty());
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/profiles/1/continue/visibility",
            body
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn continue_collapses_duplicate_episode_progress_to_the_most_recent() {
    let app = fixture();
    // Two progress rows for the same series+episode (different positions and
    // update times) must collapse to one card keeping the most recent record.
    for (position, updated) in [(30000, 100), (50000, 200)] {
        let (status, value) = request(
            &app,
            "member-token-1",
            "PUT",
            "/api/profiles/1/progress",
            json!({"record":{"identity":identity("opaque-child-1238",true),"parent_identity":identity("opaque-parent",false),"provider_id":"stremio","title":"Owned series","poster":null,"landscape":null,"position_millis":position,"duration_millis":100000,"updated_at_millis":updated,"completed_at_millis":null}}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{value}");
    }
    let (status, page) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/continue",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entries = page["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "{page}");
    assert_eq!(entries[0]["record"]["position_millis"], 50000);
}

#[tokio::test]
async fn continue_collapses_duplicate_movie_progress_to_one_card() {
    let app = fixture();
    let movie = json!({"id":"movie-1","kind":"movie","ids":[],"episode":null,"edition":null});
    for (position, updated) in [(10000, 50), (40000, 90)] {
        let (status, value) = request(
            &app,
            "member-token-1",
            "PUT",
            "/api/profiles/1/progress",
            json!({"record":{"identity":movie,"parent_identity":null,"provider_id":"stremio","title":"Owned movie","poster":null,"landscape":null,"position_millis":position,"duration_millis":120000,"updated_at_millis":updated,"completed_at_millis":null}}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{value}");
    }
    let (status, page) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/continue",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entries = page["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "{page}");
    assert_eq!(entries[0]["record"]["position_millis"], 40000);
}

#[tokio::test]
async fn continue_and_history_emit_one_durable_simkl_movie_key() {
    let app = fixture();
    // The reported queue carried one anime film under the movie catalog, the
    // anime catalog and the old store prefix, with two different progress
    // values (backend#29).
    {
        let db = app.db.lock().unwrap();
        for (id, provider, position, updated) in [
            ("simkl:movies:250822", "simkl", 51.0, 100),
            ("simkl:anime:250822", "library", 50.0, 200),
            ("simkl:movie:250822", "simkl", 49.0, 300),
        ] {
            db.execute(
                "INSERT INTO progress(profile_id,id,type,name,position,duration,updated_at,context,title_id) VALUES(1,?1,'movie','Spider-Man: Brand New Day',?2,100.0,?3,?4,?1)",
                params![id, position, updated, json!({"engine_provider_id":provider}).to_string()],
            )
            .unwrap();
        }
    }
    let (status, page) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/continue",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let entries = page["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "{page}");
    assert_eq!(entries[0]["card"]["identity"]["id"], "simkl:movie:250822");
    assert_eq!(
        entries[0]["record"]["identity"]["id"],
        "simkl:movie:250822"
    );
    // The most recently updated row is the one the card shows.
    assert_eq!(entries[0]["record"]["position_millis"], 49000);
    let body = page.to_string();
    assert!(!body.contains("simkl:movies:"), "{page}");
    assert!(!body.contains("simkl:anime:250822"), "{page}");
    let (status, history) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/history",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{history}");
    let items = history.as_array().unwrap();
    assert_eq!(items.len(), 1, "{history}");
    assert_eq!(items[0]["card"]["identity"]["id"], "simkl:movie:250822");
    assert_eq!(items[0]["updated_at_millis"], 300000);
    let body = history.to_string();
    assert!(!body.contains("simkl:movies:"), "{history}");
    assert!(!body.contains("simkl:anime:250822"), "{history}");
}
