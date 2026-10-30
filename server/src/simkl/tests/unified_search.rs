use super::*;

async fn stack(
    slow: u8,
) -> (
    App,
    tokio::task::JoinHandle<()>,
    Arc<tokio::sync::Notify>,
    Arc<tokio::sync::Semaphore>,
) {
    let started = Arc::new(tokio::sync::Notify::new());
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let routes=Router::new()
        .route("/search/:category",get(|Path(category):Path<String>|async move{axum::Json(json!([{"title":format!("Fixture {category}"),"ids":{"simkl_id":10,"imdb":"tt123"}}]))}))
        .route("/movies/19",get({let started=started.clone();let gate=gate.clone();move||{let started=started.clone();let gate=gate.clone();async move{started.notify_one();let _permit=gate.acquire().await.unwrap();axum::Json(json!({"title":"Blocked parent fixture","ids":{"simkl":19}}))}}}))
        .route("/movies/18",get(||async{axum::Json(json!({"title":"Adult fixture","ids":{"simkl":18},"certification":"TV-MA"}))}))
        .route("/movies/10",get(||async{axum::Json(json!({"title":"Fixture Simkl movie","ids":{"simkl":10,"imdb":"tt123"}}))}))
        .route("/tv/10",get(||async{axum::Json(json!({"title":"Fixture Simkl show","ids":{"simkl":10,"imdb":"tt1234567"}}))}))
        .route("/tv/episodes/10",get(||async{axum::Json(json!([{"title":"Pilot","season":1,"episode":1,"date":"2020-01-01T00:00:00Z","ids":{"simkl_id":1011}},{"title":"Second","season":1,"episode":2,"date":"2020-01-02T00:00:00Z","ids":{"simkl_id":1012}}]))}))
        .route("/addon/:owner/stream/:kind/:id",get(||async{axum::Json(json!({"streams":[{"name":"Fixture episode source","url":"http://fixture.invalid/episode.mp4"}]}))}))
        .route("/childsubs/subtitles/:kind/:id",get(|headers:axum::http::HeaderMap|async move{let host=headers[header::HOST].to_str().unwrap();axum::Json(json!({"subtitles":[{"lang":"eng","url":format!("http://{host}/child-caption.srt")}]}))}))
        .route("/child-caption.srt",get(||async{([(header::CONTENT_TYPE,"application/x-subrip")],"1\n00:00:01,000 --> 00:00:02,000\nFixture caption\n")}))
        .route("/subs/subtitles/:kind/:id",get(||async{axum::Json(json!({"subtitles":[]}))}))
        .route("/addon/:owner/catalog/:kind/:catalog/:extras",get({let started=started.clone();let gate=gate.clone();move|Path((_,kind,catalog,_)):Path<(String,String,String,String)>|{let started=started.clone();let gate=gate.clone();async move{
            if catalog=="broken"{return (StatusCode::BAD_GATEWAY,axum::Json(json!({"error":"private provider reflection"})));}
            if slow > 0 && catalog=="slow" && (slow!=2 || kind=="series"){started.notify_one();let _permit=gate.acquire().await.unwrap();}
            let metas=if slow==9 && catalog=="good" {vec![json!({"id":"simkl:movie:10","type":"movie","name":"Spoofed private title"})]}else if slow==10 && catalog=="good" {vec![json!({"id":"tt1375666","type":kind,"name":"Inception","releaseInfo":"2010"}),json!({"id":"tt5581256","type":kind,"name":"Inception"})]}else if slow==7 && catalog=="good" {vec![json!({"id":"opaque-parent","type":kind,"name":"Opaque fixture"}),json!({"id":"tt18","type":kind,"name":"Adult fixture","certification":"TV-MA"}),json!({"id":"tt123","type":kind,"name":"Canonical fixture"})]}else if catalog=="empty" {vec![]}else if slow==6 && catalog=="good" {vec![json!(null),json!(17),json!("bad")]}else if slow==4&&catalog=="good" {(0..250).map(|index|json!({"id":format!("tt{index}"),"type":kind,"name":"Fixture title"})).collect()}else{vec![json!({"id":"tt123","type":kind,"name":"Fixture title"})]};
            (StatusCode::OK,axum::Json(json!({"metas":metas})))
        }}}))
        .route("/addon/:owner/meta/:kind/:id",get(|Path((owner,kind,id)):Path<(String,String,String)>|async move{let id=id.trim_end_matches(".json");axum::Json(json!({"meta":{"id":id,"type":kind,"name":format!("Delegated fixture {owner}"),"recommendations":[{"id":"tt456","type":"movie","name":"Related"}],"videos":[{"id":format!("{id}:1:1"),"season":1,"episode":1,"title":"Pilot","released":"2020-01-01T00:00:00Z"},{"id":format!("{id}:1:2"),"season":1,"episode":2,"title":"Second","released":"2020-01-02T00:00:00Z"}]}}))}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    let mut app = fixture();
    app.addons.allow_test_loopback = true;
    app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
    for (id, account, enabled, catalog, metadata) in [
        (101, 1, 1, "good", true),
        (102, 1, 1, "search-only", false),
        (
            103,
            1,
            1,
            if slow == 3 {
                "empty"
            } else if matches!(slow, 1 | 2) {
                "slow"
            } else {
                "broken"
            },
            false,
        ),
        (104, 2, 1, "foreign", true),
        (105, 1, 0, "disabled", true),
    ] {
        let metadata = metadata || (slow == 5 && id == 102);
        let manifest = json!({"id":format!("fixture{id}"),"name":format!("Fixture {id}"),"resources":if metadata{json!(["catalog","meta","stream"])}else{json!(["catalog"])},"types":["movie","series"],"catalogs":[{"id":catalog,"type":"movie","extra":[{"name":"search","isRequired":true}]},{"id":catalog,"type":"series","extra":[{"name":"search","isRequired":true}]}]});
        app.db.lock().unwrap().execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id,enabled) VALUES(?1,?2,?3,?4,?5,?6)",params![id,format!("Fixture {id}"),format!("http://{address}/addon/{id}/manifest.json"),manifest.to_string(),account,enabled]).unwrap();
    }
    crate::test_support::encrypt_fixture_sources(&app);
    (app, task, started, gate)
}

async fn configuration(app: &App, browser: &str, remove: Option<&str>) {
    let (status, body) = request(
        app,
        "member-token-1",
        "GET",
        "/api/profiles/1/integrations",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mut value = body["configuration"].clone();
    value["browsing_plugin"] = json!(browser);
    value["scope"]["revision"] = json!(1);
    if let Some(remove) = remove {
        value["assignments"]
            .as_array_mut()
            .unwrap()
            .retain(|assignment| assignment["connection_id"] != remove);
    }
    app.db.lock().unwrap().execute("INSERT INTO engine_profile_configuration(profile_id,account_id,revision,value) VALUES(1,1,1,?1) ON CONFLICT(profile_id) DO UPDATE SET revision=1,value=excluded.value",[value.to_string()]).unwrap();
}

#[tokio::test]
async fn multi_catalog_addons_split_rows_per_catalog_with_catalog_names() {
    let (mut app, task, _, _) = stack(0).await;
    let (manifest_url, mut manifest): (String, Value) = {
        let db = app.db.lock().unwrap();
        let sealed: String = db
            .query_row(
                "SELECT secret FROM addon_credentials_v2 WHERE addon_id=101",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let payload = app
            .secret_vault
            .as_ref()
            .unwrap()
            .open_addon(1, "101", &sealed)
            .unwrap();
        let value: Value = serde_json::from_slice(payload.expose()).unwrap();
        (
            value["url"].as_str().unwrap().to_owned(),
            value["manifest"].clone(),
        )
    };
    manifest["catalogs"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"extra","name":"Extra Catalog","type":"movie","extra":[{"name":"search","isRequired":true}]}));
    {
        let db = app.db.lock().unwrap();
        let secret = app
            .secret_vault
            .as_ref()
            .unwrap()
            .seal_addon(
                1,
                "101",
                serde_json::json!({"url": manifest_url, "manifest": manifest})
                    .to_string()
                    .as_bytes(),
            )
            .unwrap();
        db.execute(
            "UPDATE addon_credentials_v2 SET secret=?1 WHERE addon_id=101",
            [secret],
        )
        .unwrap();
    }
    let (status, page) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/search?query=Fixture",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let rows = page["rows"].as_array().unwrap();
    let first = rows
        .iter()
        .find(|row| row["id"] == "addon:101:good")
        .expect("split catalog row");
    assert_eq!(
        first["title"],
        "Fixture 101 · good",
        "label uses the catalog name when the add-on name differs"
    );
    assert_eq!(first["state"], "Ready");
    assert!(!first["cards"].as_array().unwrap().is_empty());
    let second = rows
        .iter()
        .find(|row| row["id"] == "addon:101:extra")
        .expect("second split catalog row");
    assert_eq!(second["title"], "Fixture 101 · Extra Catalog");
    assert_eq!(second["state"], "Ready");
    assert!(!second["cards"].as_array().unwrap().is_empty());
    assert!(rows.iter().any(|row| row["id"] == "addon:102"));
    task.abort();
}

#[tokio::test]
async fn grouped_search_keeps_three_simkl_rows_and_per_addon_identity_with_partial_failures() {
    let (app, task, _, _) = stack(0).await;
    let (status, page) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/search?query=Fixture",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let rows = page["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 6);
    assert_eq!(page["complete"], false);
    for id in [
        "simkl:anime",
        "simkl:movie",
        "simkl:tv",
        "addon:101",
        "addon:102",
    ] {
        let row = rows.iter().find(|row| row["id"] == id).unwrap();
        assert_eq!(row["state"], "Ready", "{row}");
        assert!(!row["cards"].as_array().unwrap().is_empty());
    }
    let addon = rows.iter().find(|row| row["id"] == "addon:101").unwrap();
    assert_eq!(addon["cards"].as_array().unwrap().len(), 2);
    assert_eq!(addon["cards"][0]["provider_id"], "addon:101");
    assert_eq!(
        rows.iter().find(|row| row["id"] == "addon:103").unwrap()["state"],
        "Failed"
    );
    assert!(!page.to_string().contains("private provider reflection"));
    configuration(&app, "stremio", Some("addon:102")).await;
    let (status, page) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/search?query=Fixture",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["rows"].as_array().unwrap().len(), 5);
    assert!(page["rows"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["id"] == "simkl:movie"));
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/search?query=",
            json!({})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    task.abort();
}

#[tokio::test]
async fn explicit_metadata_origin_overrides_default_and_search_only_addons_delegate_safely() {
    let (app, task, _, _) = stack(0).await;
    for addon in [101, 102] {
        let path = format!("/api/meta/series/tt123?provider_id=stremio&addon_id={addon}");
        let (status, detail) = request(&app, "member-token-1", "GET", &path, json!({})).await;
        assert_eq!(status, StatusCode::OK, "{detail}");
        assert_eq!(detail["item"]["provider_id"], format!("addon:{addon}"));
        assert_eq!(
            detail["episodes"][0]["provider_id"],
            format!("addon:{addon}")
        );
    }
    for addon in [104, 105, 999] {
        let path = format!("/api/meta/movie/tt123?provider_id=stremio&addon_id={addon}");
        assert_eq!(
            request(&app, "member-token-1", "GET", &path, json!({}))
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    configuration(&app, "stremio", Some("addon:102")).await;
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/meta/movie/tt123?provider_id=stremio&addon_id=102",
            json!({})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, detail) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/meta/movie/simkl:movie:10?provider_id=simkl",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["item"]["provider_id"], "simkl");
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/meta/movie/tt123?provider_id=other",
            json!({})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    task.abort();
}

#[tokio::test]
async fn a_search_row_timeout_retains_completed_rows() {
    let (app, task, _, _) = stack(1).await;
    let start = Instant::now();
    let (status, page) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/search?query=Fixture",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(start.elapsed() < Duration::from_secs(6));
    let rows = page["rows"].as_array().unwrap();
    assert_eq!(
        rows.iter().find(|row| row["id"] == "addon:103").unwrap()["state"],
        "Failed"
    );
    assert_eq!(
        rows.iter().find(|row| row["id"] == "addon:101").unwrap()["state"],
        "Ready"
    );
    task.abort();
}

#[tokio::test]
async fn profile_and_policy_changes_suppress_all_inflight_search_rows() {
    for mutation in ["UPDATE auth_sessions SET profile_id=NULL WHERE id='s1'","INSERT INTO kids_profiles(profile_id,enabled,max_age,revision) VALUES(1,1,12,1)","INSERT INTO engine_profile_configuration(profile_id,account_id,revision,value) VALUES(1,1,1,'{}')"] {
        let(app,task,started,gate)=stack(1).await;let pending=tokio::spawn({let app=app.clone();async move{request(&app,"member-token-1","GET","/api/search?query=Fixture",json!({})).await}});
        tokio::time::timeout(Duration::from_secs(2),started.notified()).await.unwrap();app.db.lock().unwrap().execute(mutation,[]).unwrap();gate.add_permits(4);
        let(status,page)=pending.await.unwrap();assert!(matches!(status,StatusCode::UNAUTHORIZED|StatusCode::CONFLICT|StatusCode::FORBIDDEN),"{status} {page}");assert!(page["rows"].is_null());task.abort();
    }
}

#[tokio::test]
async fn fast_sibling_catalog_cards_survive_the_same_addon_timeout() {
    let (app, task, _, _) = stack(2).await;
    let (status, page) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/search?query=Fixture",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let row = page["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "addon:103")
        .unwrap();
    assert_eq!(row["state"], "Ready");
    assert_eq!(row["cards"].as_array().unwrap().len(), 1);
    assert_eq!(row["cards"][0]["identity"]["kind"], "movie");
    assert!(row["message"].is_string());
    assert_eq!(page["complete"], false);
    task.abort();
}

#[tokio::test]
async fn empty_rows_are_distinct_from_failure_and_large_rows_keep_the_bounded_200_cards() {
    for mode in [3, 4, 6] {
        let (app, task, _, _) = stack(mode).await;
        let (status, page) = request(
            &app,
            "member-token-1",
            "GET",
            "/api/search?query=Fixture",
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{page}");
        let id = if mode == 3 { "addon:103" } else { "addon:101" };
        let row = page["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == id)
            .unwrap();
        if mode == 3 {
            assert_eq!(row["state"], "Empty");
            assert!(row["message"].is_null());
        } else if mode == 4 {
            assert_eq!(row["cards"].as_array().unwrap().len(), 200);
        } else {
            assert_eq!(row["state"], "Failed");
            assert!(row["message"].is_string());
        }
        task.abort();
    }
}

#[tokio::test]
async fn duplicate_display_names_without_a_year_are_dropped_from_search() {
    let (app, task, _, _) = stack(10).await;
    let (status, page) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/search?query=Inception",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let row = page["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "addon:101")
        .unwrap();
    assert_eq!(row["state"], "Ready", "{page}");
    let cards = row["cards"].as_array().unwrap();
    assert!(
        cards
            .iter()
            .any(|card| card["identity"]["id"] == "tt1375666"),
        "{page}"
    );
    assert!(
        cards
            .iter()
            .all(|card| card["identity"]["id"] != "tt5581256"),
        "{page}"
    );
    task.abort();
}

#[cfg(feature = "vsources-runtime")]
#[tokio::test]
async fn source_metadata_cache_is_scoped_to_the_validated_origin_even_for_equal_opaque_ids() {
    let (app, task, _, _) = stack(5).await;
    for id in [101, 102] {
        let (status, _) = request(
            &app,
            "member-token-1",
            "GET",
            &format!("/api/meta/movie/tt123?provider_id=stremio&addon_id={id}"),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    let lease = ResourceLease {
        policy_revision: 0,
        principal: auth::Principal::Account {
            account_id: 1,
            profile_id: Some(1),
            role: "owner".into(),
            session_id: Some("s1".into()),
        },
        session_id: Some("s1".into()),
    };
    let scoped = app.clone().with_lease(lease.clone());
    for id in [101, 102] {
        let value = json!({"type":"movie","id":"tt123","metadata_provider_id":"stremio","metadata_addon_id":id});
        let card =
            crate::engine_v3::source_integrations::canonical_card(&scoped, &value, &lease, 0)
                .await
                .unwrap();
        assert_eq!(card.provider_id, format!("addon:{id}"));
        assert_eq!(card.title, format!("Delegated fixture {id}"));
    }
    for id in [104, 105, 999] {
        let(status,_)=request(&app,"member-token-1","POST","/api/streams",json!({"type":"movie","id":"tt123","metadata_provider_id":"stremio","metadata_addon_id":id})).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    task.abort();
}

fn next_request(addon: i64) -> Value {
    json!({"identity":{"id":"opaque-parent:1:1","kind":"series","ids":[],"episode":{"order":"aired","season":1,"episode":1}},"parent_identity":{"id":"opaque-parent","kind":"series","ids":[]},"metadata_provider_id":"stremio","metadata_addon_id":addon})
}

#[tokio::test]
async fn typed_next_preserves_explicit_origin_and_never_reuses_a_different_provider_queue_cache() {
    let (app, task, _, _) = stack(0).await;
    let request_a = next_request(101);
    let (status, next) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/profiles/1/continue/next",
        request_a.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{next}");
    assert_eq!(next["status"], "Next");
    assert_eq!(next["item"]["provider_id"], "addon:101");
    let record = json!({"ended":true,"record":{"identity":request_a["identity"],"parent_identity":request_a["parent_identity"],"provider_id":"addon:102","title":"Same opaque ID from another addon","poster":null,"landscape":null,"position_millis":100000,"duration_millis":100000,"updated_at_millis":1,"completed_at_millis":null}});
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/profiles/1/progress",
            record
        )
        .await
        .0,
        StatusCode::OK
    );
    let (status, queue) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/continue",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{queue}");
    assert_eq!(queue["entries"][0]["status"], "Pending");
    let (status, next) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/profiles/1/continue/next",
        next_request(102),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{next}");
    assert_eq!(next["item"]["provider_id"], "addon:102");
    assert_eq!(next["item"]["identity"]["id"], "opaque-parent:1:2");
    let (_, queue) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/continue",
        json!({}),
    )
    .await;
    assert_eq!(queue["entries"][0]["status"], "Next");
    assert_eq!(queue["entries"][0]["card"]["provider_id"], "addon:102");
    for id in [104, 105, 999] {
        assert_eq!(
            request(
                &app,
                "member-token-1",
                "POST",
                "/api/profiles/1/continue/next",
                next_request(id)
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    configuration(&app, "stremio", None).await;
    let (_, queue) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/continue",
        json!({}),
    )
    .await;
    assert_eq!(queue["entries"][0]["status"], "Pending");
    task.abort();
}

#[tokio::test]
async fn typed_simkl_next_works_when_stremio_is_the_browse_default() {
    let (app, task, _, _) = stack(0).await;
    configuration(&app, "stremio", None).await;
    let (status, detail) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/meta/series/simkl:tv:10?provider_id=simkl",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    let input = json!({"identity":detail["episodes"][0]["identity"],"parent_identity":detail["item"]["identity"],"metadata_provider_id":"simkl"});
    let (status, next) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/profiles/1/continue/next",
        input,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{next}");
    assert_eq!(next["status"], "Next");
    assert_eq!(next["item"]["provider_id"], "simkl");
    assert_eq!(next["item"]["identity"]["episode"]["episode"], 2);
    task.abort();
}

#[tokio::test]
async fn subtitle_origin_keeps_stremio_opaque_simkl_prefix_ids_out_of_simkl_mapping() {
    let (app, task, _, _) = stack(0).await;
    let manifest = json!({"id":"subtitles","name":"Subtitles","resources":["subtitles"],"types":["movie","series"]});
    app.db.lock().unwrap().execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id)VALUES(110,'Subtitles',?1,?2,1)",params![format!("{}/subs/manifest.json",app.simkl.data_base),manifest.to_string()]).unwrap();
    crate::test_support::encrypt_fixture_sources(&app);
    let path="/api/subtitles/movie/simkl:not-a-real-simkl-id?metadata_provider_id=stremio&metadata_addon_id=101";
    let (status, subtitles) = request(&app, "member-token-1", "GET", path, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{subtitles}");
    assert_eq!(subtitles["providers"].as_array().unwrap().len(), 1);
    assert_eq!(subtitles["providers"][0]["state"], "Empty");
    for id in [104, 105, 999] {
        assert_eq!(request(&app,"member-token-1","GET",&format!("/api/subtitles/movie/tt123?metadata_provider_id=stremio&metadata_addon_id={id}"),json!({})).await.0,StatusCode::FORBIDDEN);
    }
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/subtitles/movie/tt123?metadata_addon_id=101",
            json!({})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    task.abort();
}

#[tokio::test]
async fn typed_parent_search_and_approval_are_selected_profile_scoped_and_observe_real_simkl_episodes(
) {
    let (app, task, _, _) = stack(0).await;
    configuration(&app, "stremio", None).await;
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/parent/pin",
            json!({"pin":"7248"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/profiles/1/kids",
            json!({"enabled":true,"max_age":7})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/parent/search?query=Fixture",
            json!({})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/parent/unlock",
            json!({"pin":"7248"})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (status, page) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/parent/search?query=Fixture",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let rows = page["rows"].as_array().unwrap();
    for row in rows.iter().filter(|row| row["state"] == "Ready") {
        assert!(row.get("cards").is_none());
        assert!(row["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["approval_supported"] == true));
    }
    assert!(rows
        .iter()
        .any(|row| row["id"] == "simkl:tv" && !row["items"].as_array().unwrap().is_empty()));
    let body =
        json!({"type":"series","id":"simkl:tv:10","approved":true,"metadata_provider_id":"simkl"});
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/profiles/2/approvals",
            body.clone()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, reply) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/profiles/1/approvals",
        body,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    {
        let db = app.db.lock().unwrap();
        let count: i64 = db
            .query_row(
                "SELECT count(*) FROM kids_media WHERE account_id=1 AND parent_id='simkl:tv:10'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 3);
        db.execute("DELETE FROM parent_grants", []).unwrap();
    }
    let (status, detail) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/meta/series/simkl:tv:10?provider_id=simkl",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["episodes"].as_array().unwrap().len(), 2);
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/parent/search?query=Fixture",
            json!({})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    task.abort();
}

#[tokio::test]
async fn typed_parent_approval_rejects_opaque_namespace_confusion_and_keeps_revoke_available() {
    let (app, task, _, _) = stack(0).await;
    for (provider, id, addon, expected) in [
        (
            "stremio",
            "opaque-parent",
            Some(101),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "stremio",
            "simkl:movie:10",
            Some(101),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        ("simkl", "tt123", None, StatusCode::UNPROCESSABLE_ENTITY),
        ("stremio", "tt123", Some(104), StatusCode::FORBIDDEN),
        ("stremio", "tt123", Some(105), StatusCode::FORBIDDEN),
    ] {
        let mut body =
            json!({"type":"movie","id":id,"approved":true,"metadata_provider_id":provider});
        if let Some(addon) = addon {
            body["metadata_addon_id"] = json!(addon);
        }
        assert_eq!(
            request(
                &app,
                "member-token-1",
                "POST",
                "/api/profiles/1/approvals",
                body
            )
            .await
            .0,
            expected
        );
    }
    assert_eq!(request(&app,"member-token-1","POST","/api/profiles/1/approvals",json!({"type":"movie","id":"tt123","approved":true,"metadata_provider_id":"stremio","metadata_addon_id":102})).await.0,StatusCode::OK);
    {
        let db = app.db.lock().unwrap();
        db.execute(
            "INSERT INTO kids_approvals(profile_id,kind,id)VALUES(1,'movie','opaque-legacy')",
            [],
        )
        .unwrap();
    }
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/profiles/1/approvals",
            json!({"type":"movie","id":"opaque-legacy","approved":false})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (status, list) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/approvals",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    configuration(&app, "stremio", Some("addon:101")).await;
    let (status, page) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/parent/search?query=Fixture",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let row = page["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "addon:102")
        .unwrap();
    assert!(row["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["approval_supported"] == false
            && item["message"] == "Metadata is not configured for this result."));
    task.abort();
}

#[tokio::test]
async fn typed_parent_search_marks_scoped_private_ids_supported_and_adult_refetch_cannot_be_approved() {
    let (app, task, _, _) = stack(7).await;
    let (status, page) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/parent/search?query=Fixture",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let row = page["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "addon:101")
        .unwrap();
    let items = row["items"].as_array().unwrap();
    assert!(items
        .iter()
        .any(|item| item["card"]["identity"]["id"] == "opaque-parent"
            && item["approval_supported"] == true));
    assert!(!items
        .iter()
        .any(|item| item["card"]["identity"]["id"] == "tt18"));
    let (status,error)=request(&app,"member-token-1","POST","/api/profiles/1/approvals",json!({"type":"movie","id":"simkl:movie:18","approved":true,"metadata_provider_id":"simkl"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM kids_approvals", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    task.abort();
}

#[tokio::test]
async fn typed_parent_search_discards_inflight_previews_after_parent_grant_is_revoked() {
    let (app, task, started, gate) = stack(1).await;
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO kids_profiles(profile_id,enabled,max_age)VALUES(1,1,7)",
            [],
        )
        .unwrap();
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO parent_grants(session_id,expires)VALUES('s1',?1)",
            [util::now() + 120],
        )
        .unwrap();
    let request_app = app.clone();
    let pending = tokio::spawn(async move {
        request(
            &request_app,
            "member-token-1",
            "GET",
            "/api/parent/search?query=Fixture",
            json!({}),
        )
        .await
    });
    started.notified().await;
    app.db
        .lock()
        .unwrap()
        .execute("DELETE FROM parent_grants WHERE session_id='s1'", [])
        .unwrap();
    gate.add_permits(2);
    assert_eq!(pending.await.unwrap().0, StatusCode::FORBIDDEN);
    task.abort();
}

#[tokio::test]
async fn typed_parent_approval_refuses_late_metadata_after_parent_authority_is_removed() {
    let (app, task, started, gate) = stack(0).await;
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO kids_profiles(profile_id,enabled,max_age)VALUES(1,1,7)",
            [],
        )
        .unwrap();
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO parent_grants(session_id,expires)VALUES('s1',?1)",
            [util::now() + 120],
        )
        .unwrap();
    let request_app = app.clone();
    let pending = tokio::spawn(async move {
        request(&request_app,"member-token-1","POST","/api/profiles/1/approvals",json!({"type":"movie","id":"simkl:movie:19","approved":true,"metadata_provider_id":"simkl"})).await
    });
    started.notified().await;
    app.db
        .lock()
        .unwrap()
        .execute("DELETE FROM parent_grants WHERE session_id='s1'", [])
        .unwrap();
    gate.add_permits(1);
    assert_eq!(pending.await.unwrap().0, StatusCode::FORBIDDEN);
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM kids_approvals", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    task.abort();
}

#[tokio::test]
async fn restricted_canonical_episode_consumers_use_verified_policy_and_revoke_real_playback_and_subtitles(
) {
    let (app, task, _, _) = stack(0).await;
    let manifest = json!({"id":"child-subs","name":"Child fixture subtitles","resources":["subtitles"],"types":["movie","series"]});
    app.db.lock().unwrap().execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id)VALUES(111,'Child subtitles',?1,?2,1)",params![format!("{}/childsubs/manifest.json",app.simkl.data_base),manifest.to_string()]).unwrap();
    crate::test_support::encrypt_fixture_sources(&app);
    configuration(&app, "stremio", None).await;
    assert_eq!(request(&app,"member-token-1","POST","/api/profiles/1/approvals",json!({"type":"series","id":"simkl:tv:10","approved":true,"metadata_provider_id":"simkl"})).await.0,StatusCode::OK);
    app.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE kids_profiles SET enabled=1,max_age=7,revision=revision+1 WHERE profile_id=1",
            [],
        )
        .unwrap();
    let meta_path = "/api/meta/series/simkl:tv:10?provider_id=simkl";
    let (status, detail) = request(&app, "member-token-1", "GET", meta_path, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["episodes"].as_array().unwrap().len(), 2);
    let episode = &detail["episodes"][0];
    let next_body = json!({"identity":episode["identity"],"parent_identity":detail["item"]["identity"],"metadata_provider_id":"simkl"});
    let (status, next) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/profiles/1/continue/next",
        next_body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{next}");
    assert_eq!(next["status"], "Next");
    let episode_id = episode["identity"]["id"].as_str().unwrap();
    let subpath = format!("/api/subtitles/series/{episode_id}?metadata_provider_id=simkl");
    let (status, subs) = request(&app, "member-token-1", "GET", &subpath, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{subs}");
    assert_eq!(subs["candidates"].as_array().unwrap().len(), 1);
    let caption = format!(
        "/api/subtitle-content/{}",
        subs["candidates"][0]["id"].as_str().unwrap()
    );
    {
        use tower::ServiceExt;
        let response = crate::router(app.clone())
            .oneshot(
                axum::http::Request::builder()
                    .uri(&caption)
                    .header(header::AUTHORIZATION, "Bearer member-token-1")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert!(String::from_utf8(bytes.to_vec())
            .unwrap()
            .starts_with("WEBVTT"));
    }
    let source_body = json!({"type":"series","id":episode_id,"series_id":"simkl:tv:10","season":1,"episode":1,"metadata_provider_id":"simkl"});
    let (status, job) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/streams",
        source_body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{job}");
    let poll = format!("/api/streams/{}", job["id"].as_str().unwrap());
    let found = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let (status, found) = request(&app, "member-token-1", "GET", &poll, json!({})).await;
            assert_eq!(status, StatusCode::OK, "{found}");
            if found["done"] == true {
                break found;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let source = found["events"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|event| event["streams"].as_array().into_iter().flatten())
        .next()
        .expect("real mock-addon source emitted")["id"]
        .as_str()
        .unwrap();
    let (status,playback)=request(&app,"member-token-1","POST","/api/playback",json!({"request_id":"canonical-child-episode","stream_id":source,"client":{"platform":"android","can_play_direct":true,"max_width":1920,"max_height":1080,"video_codecs":["h264"],"audio_codecs":["aac"]},"position":0})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{playback}");
    let playback_path = format!("/api/playback/{}", playback["id"].as_str().unwrap());
    let ready = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let (status, ready) =
                request(&app, "member-token-1", "GET", &playback_path, Value::Null).await;
            assert_eq!(status, StatusCode::OK, "{ready}");
            if ready["status"] != "starting" {
                break ready;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(ready["status"], "ready", "{ready}");
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            &format!("{playback_path}/heartbeat"),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/meta/series/simkl:tv:10?provider_id=stremio&addon_id=101",
            json!({})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let spoof_sub = format!(
        "/api/subtitles/series/{episode_id}?metadata_provider_id=stremio&metadata_addon_id=101"
    );
    assert_eq!(
        request(&app, "member-token-1", "GET", &spoof_sub, json!({}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let mut spoof_next = next_body;
    spoof_next["metadata_provider_id"] = json!("stremio");
    spoof_next["metadata_addon_id"] = json!(101);
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/profiles/1/continue/next",
            spoof_next
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let mut spoof_source = source_body;
    spoof_source["metadata_provider_id"] = json!("stremio");
    spoof_source["metadata_addon_id"] = json!(101);
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/streams",
            spoof_source
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/meta/movie/simkl:movie:18?provider_id=simkl",
            json!({})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    app.db
        .lock()
        .unwrap()
        .execute("DELETE FROM kids_approvals WHERE profile_id=1", [])
        .unwrap();
    app.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE kids_profiles SET revision=revision+1 WHERE profile_id=1",
            [],
        )
        .unwrap();
    for path in [meta_path, &caption, &poll, &playback_path] {
        assert_eq!(
            request(&app, "member-token-1", "GET", path, Value::Null)
                .await
                .0,
            StatusCode::FORBIDDEN,
            "{path}"
        );
    }
    task.abort();
}

#[tokio::test]
async fn restricted_opaque_explicit_consumers_remain_unsupported_without_widening_legacy_approvals()
{
    let (app, task, _, _) = stack(0).await;
    configuration(&app, "stremio", None).await;
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/profiles/1/approvals",
            json!({"type":"movie","id":"opaque-legacy","approved":true})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    app.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE kids_profiles SET enabled=1,max_age=7,revision=revision+1 WHERE profile_id=1",
            [],
        )
        .unwrap();
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO kids_approvals(profile_id,kind,id) VALUES(1,'movie','opaque-legacy')",
            [],
        )
        .unwrap();
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/meta/movie/opaque-legacy",
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/meta/movie/opaque-legacy?provider_id=stremio&addon_id=101",
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/subtitles/movie/opaque-legacy?metadata_provider_id=stremio&metadata_addon_id=101",
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/streams",
            json!({"type":"movie","id":"opaque-legacy","metadata_provider_id":"stremio","metadata_addon_id":101})
        )
        .await
        .0,
        StatusCode::OK
    );
    task.abort();
}

#[tokio::test]
async fn restricted_imdb_parent_and_exact_stremio_episode_work_with_a_different_browse_default() {
    let (app, task, _, _) = stack(0).await;
    configuration(&app, "simkl", None).await;
    assert_eq!(request(&app,"member-token-1","POST","/api/profiles/1/approvals",json!({"type":"series","id":"tt123","approved":true,"metadata_provider_id":"stremio","metadata_addon_id":101})).await.0,StatusCode::OK);
    app.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE kids_profiles SET enabled=1,max_age=7,revision=revision+1 WHERE profile_id=1",
            [],
        )
        .unwrap();
    let (status, detail) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/meta/series/tt123?provider_id=stremio&addon_id=101",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["episodes"].as_array().unwrap().len(), 2);
    let body = json!({"identity":detail["episodes"][0]["identity"],"parent_identity":detail["item"]["identity"],"metadata_provider_id":"stremio","metadata_addon_id":101});
    let (status, next) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/profiles/1/continue/next",
        body,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{next}");
    assert_eq!(next["item"]["identity"]["id"], "tt123:1:2");
    assert_eq!(next["item"]["provider_id"], "addon:101");
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/subtitles/series/tt123:1:1?metadata_provider_id=stremio&metadata_addon_id=101",
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(request(&app,"member-token-1","POST","/api/streams",json!({"type":"series","id":"tt123:1:1","series_id":"tt123","metadata_provider_id":"stremio","metadata_addon_id":101})).await.0,StatusCode::OK);
    for path in [
        "/api/meta/movie/tt456?provider_id=stremio&addon_id=101",
        "/api/subtitles/movie/tt456?metadata_provider_id=stremio&metadata_addon_id=101",
    ] {
        assert_eq!(
            request(&app, "member-token-1", "GET", path, json!({}))
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    task.abort();
}

#[tokio::test]
async fn restricted_search_and_discovery_project_only_approved_titles_and_hide_reserved_namespace_spoofs(
) {
    let (app, task, _, _) = stack(9).await;
    configuration(&app, "stremio", None).await;
    assert_eq!(request(&app,"member-token-1","POST","/api/profiles/1/approvals",json!({"type":"movie","id":"simkl:movie:10","approved":true,"metadata_provider_id":"simkl"})).await.0,StatusCode::OK);
    app.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE kids_profiles SET enabled=1,max_age=7,revision=revision+1 WHERE profile_id=1",
            [],
        )
        .unwrap();
    let (status, search) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/search?query=Fixture",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{search}");
    let rows = search["rows"].as_array().unwrap();
    let simkl = rows.iter().find(|row| row["id"] == "simkl:movie").unwrap();
    assert_eq!(simkl["cards"].as_array().unwrap().len(), 1);
    for row in rows.iter().filter(|row| row["provider_id"] == "stremio") {
        assert!(row["cards"].as_array().unwrap().is_empty());
    }
    assert!(!search.to_string().contains("Spoofed private title"));
    let (status, page) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/discover?type=movie&search=Fixture&addon_id=101",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page["cards"].as_array().unwrap().is_empty());
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/parent/search?query=Fixture",
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    task.abort();
}

fn add_parent_target(app: &App) {
    let db = app.db.lock().unwrap();
    db.execute("INSERT INTO profiles(id,name,avatar_seed,presentation_complete) VALUES(3,'Managed child','target',1)",[]).unwrap();
    db.execute(
        "INSERT INTO profile_owners(profile_id,account_id,created_at) VALUES(3,1,0)",
        [],
    )
    .unwrap();
    db.execute("INSERT INTO auth_profiles VALUES(1,3)", [])
        .unwrap();
    db.execute(
        "INSERT INTO kids_profiles(profile_id,enabled,max_age,revision)VALUES(3,1,7,1)",
        [],
    )
    .unwrap();
}

#[tokio::test]
async fn parent_target_search_and_approval_use_owned_target_without_switching_media_profile() {
    let (app, task, _, _) = stack(0).await;
    add_parent_target(&app);
    let (status, value) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/integrations",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let mut config = value["configuration"].clone();
    config["scope"]["profile_id"] = json!("3");
    config["browsing_plugin"] = json!("stremio");
    config["scope"]["revision"] = json!(1);
    config["assignments"]
        .as_array_mut()
        .unwrap()
        .retain(|entry| entry["connection_id"] == "addon:101");
    app.db.lock().unwrap().execute("INSERT INTO engine_profile_configuration(profile_id,account_id,revision,value)VALUES(3,1,1,?1)",[config.to_string()]).unwrap();
    let (status, page) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/parent/search?query=Fixture&profile_id=3",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let rows = page["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], "addon:101");
    assert_eq!(rows[0]["items"][0]["approval_supported"], true);
    let body = json!({"type":"movie","id":"tt123","approved":true,"metadata_provider_id":"stremio","metadata_addon_id":101});
    let (status, result) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/profiles/3/approvals",
        body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/profiles/3/approvals",
            Value::Null
        )
        .await
        .1["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        request(
            &app,
            "member-token-2",
            "GET",
            "/api/parent/search?query=Fixture&profile_id=3",
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &app,
            "member-token-2",
            "POST",
            "/api/profiles/3/approvals",
            body
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/search?query=Fixture&profile_id=3",
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
            "/api/profiles/3/library",
            Value::Null
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
                "SELECT profile_id FROM auth_sessions WHERE id='s1'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    task.abort();
}

#[tokio::test]
async fn parent_target_requires_current_grant_and_discards_late_target_policy_changes() {
    let (app, task, started, gate) = stack(0).await;
    add_parent_target(&app);
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO kids_profiles(profile_id,enabled,max_age)VALUES(1,1,7)",
            [],
        )
        .unwrap();
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/parent/search?query=Fixture&profile_id=3",
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO parent_grants(session_id,expires)VALUES('s1',?1)",
            [util::now() + 120],
        )
        .unwrap();
    let pending_app = app.clone();
    let pending = tokio::spawn(async move {
        request(&pending_app,"member-token-1","POST","/api/profiles/3/approvals",json!({"type":"movie","id":"simkl:movie:19","approved":true,"metadata_provider_id":"simkl"})).await
    });
    started.notified().await;
    app.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE kids_profiles SET revision=revision+1,max_age=8 WHERE profile_id=3",
            [],
        )
        .unwrap();
    gate.add_permits(1);
    assert_eq!(pending.await.unwrap().0, StatusCode::CONFLICT);
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM kids_approvals WHERE profile_id=3",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    task.abort();
}
