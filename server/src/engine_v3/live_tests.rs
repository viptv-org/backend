use crate::{auth_integration_tests::fixture, test_support::request, *};
#[tokio::test]
async fn live_channels_and_real_guide_timestamps_remain_profile_assigned_and_sources_opaque() {
    let app = fixture();
    {
        let db = app.db.lock().unwrap();
        db.execute_batch("INSERT INTO providers(id,name,url,username,password,enabled,enable_live,enable_movies,enable_series)VALUES(10,'Owned live','https://provider.example','private-user','private-password',1,1,0,0);INSERT INTO provider_ownership VALUES(10,1);INSERT INTO provider_live(id,provider_id,stream_id,name,category_id,category,ordinal)VALUES('opaque-live',10,'7','News channel','news','News',1);").unwrap();
        let now = util::now();
        db.execute("INSERT INTO provider_cache VALUES(10,'get_short_epg:7',?1,?2)",params![now+60,json!({"epg_listings":[{"id":"upstream-program","title":"TmV3cw==","start_timestamp":now,"stop_timestamp":now+3600}]}).to_string()]).unwrap();
    }
    crate::test_support::encrypt_fixture_sources(&app);
    let (status, page) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/live/channels",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["channels"][0]["card"]["identity"]["kind"], "live");
    assert_eq!(page["availability"], "Available");
    assert!(!page.to_string().contains("private-password"));
    let (status, guide) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/live/opaque-live/guide",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{guide}");
    assert_eq!(guide["programs"][0]["title"], "News");
    assert_eq!(
        guide["programs"][0]["end_millis"].as_u64().unwrap()
            - guide["programs"][0]["start_millis"].as_u64().unwrap(),
        3600000
    );
    let (status, sources) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/profiles/1/live/opaque-live/sources",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{sources}");
    assert_eq!(sources["live"], true);
    assert_eq!(sources["action"]["card"]["identity"]["kind"], "live");
    assert_eq!(sources["action"]["card"]["identity"]["id"], "opaque-live");
    assert_eq!(sources["action"]["card"]["title"], "News channel");
    assert_eq!(sources["action"]["resume"], false);
    assert_eq!(sources["action"]["source_selection"], "ask");
    let (_, settings) = request(&app,"member-token-1","GET","/api/profiles/1/preferences",Value::Null).await;
    let mut preferences = settings["preferences"].clone();
    preferences["source_selection"] = json!("best");
    assert_eq!(request(&app,"member-token-1","PUT","/api/profiles/1/preferences",json!({"revision":0,"preferences":preferences})).await.0,StatusCode::OK);
    let (_, selected) = request(&app,"member-token-1","POST","/api/profiles/1/live/opaque-live/sources",Value::Null).await;
    assert_eq!(selected["action"]["source_selection"], "best");
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/profiles/1/source-ranking",
            json!({"ids":[sources["streams"][0]["id"]]})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(!sources.to_string().contains("private-password"));
    assert!(!sources.to_string().contains("https://provider.example"));
    assert_eq!(
        request(
            &app,
            "member-token-2",
            "GET",
            "/api/profiles/1/live/channels",
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, _) = request(
        &app,
        "member-token-1",
        "PUT",
        "/api/profiles/1/integrations",
        json!({"revision":0,"browsing_plugin":"stremio","assignments":[]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, page) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/live/channels",
        Value::Null,
    )
    .await;
    assert_eq!(page["availability"], "NeedsConfiguration");
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/profiles/1/live/opaque-live/sources",
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}
#[cfg(feature = "live-runtime")]
#[tokio::test]
async fn m3u_configuration_and_live_grants_keep_credentials_private_and_missing_guide_honest() {
    let app = fixture();
    let(status,_)=request(&app,"member-token-1","PUT","/api/profiles/1/source-integrations",json!({"revision":0,"plugin_id":"m3u","configuration":{"playlist_url":"https://provider.example/list.m3u?token=private"}})).await;
    assert_eq!(status, StatusCode::OK);
    let (_, schemas) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/source-integrations",
        Value::Null,
    )
    .await;
    let connection = schemas["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["plugin_id"] == "m3u")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let channels=viptv_engine::live::parse_m3u("#EXTM3U\n#EXTINF:-1 tvg-id=\"news\" tvg-logo=\"https://images.example/news.png\",News TV\n#EXTVLCOPT:http-referrer=https://headers.example/source-context\nhttps://media.example/live.m3u8?token=private\n",&url::Url::parse("https://provider.example/list.m3u").unwrap(),connection).unwrap();
    let id = channels[0].channel.id.clone();
    app.live_cache.lock().await.insert(
        connection.into(),
        engine_v3::live::Cache {
            revision: 1,
            expires: Instant::now() + Duration::from_secs(300),
            channels,
            groups: vec![],
            guides: Default::default(),
            xmltv: None,
        },
    );
    let (status, page) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/live/channels",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!page.to_string().contains("token=private"));
    let (status, guide) = request(
        &app,
        "member-token-1",
        "GET",
        &format!("/api/profiles/1/live/{id}/guide"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(guide["availability"], "Unsupported");
    assert!(guide["programs"].as_array().unwrap().is_empty());
    let (status, source) = request(
        &app,
        "member-token-1",
        "POST",
        &format!("/api/profiles/1/live/{id}/sources"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(source["live"], true);
    assert_eq!(source["action"]["card"]["identity"], page["channels"][0]["card"]["identity"]);
    assert_eq!(source["action"]["source_selection"], "ask");
    assert!(source["action"]["card"]["landscape"].is_null());
    assert!(!source.to_string().contains("https://"));
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/profiles/1/source-ranking",
            json!({"ids":[source["streams"][0]["id"]]})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(!source.to_string().contains("token=private"));
    let grant=source["streams"][0]["id"].as_str().unwrap();
    assert_eq!(app.streams.lock().unwrap()[grant].headers["referer"],"https://headers.example/source-context");
    assert!(!source.to_string().contains("source-context"));
}
#[cfg(feature = "live-runtime")]
#[tokio::test]
#[ignore = "reads the real published public playlist using an isolated owned API fixture"]
async fn real_public_m3u_api_directory_guide_status_and_source_grants() {
    let app = fixture();
    let(status,_)=request(&app,"member-token-1","PUT","/api/profiles/1/source-integrations",json!({"revision":0,"plugin_id":"m3u","configuration":{"playlist_url":"https://iptv-org.github.io/iptv/countries/us.m3u"}})).await;
    assert_eq!(status, StatusCode::OK);
    let start = Instant::now();
    let (status, page) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/live/channels?limit=20",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let channel = page["channels"][0]["id"].as_str().unwrap();
    assert_eq!(page["availability"], "Available");
    let (status, source) = request(
        &app,
        "member-token-1",
        "POST",
        &format!("/api/profiles/1/live/{channel}/sources"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(source["live"], true);
    assert!(!source.to_string().contains("https://"));
    let (status, ranked) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/profiles/1/source-ranking",
        json!({"ids":[source["streams"][0]["id"]]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ranked["streams"].as_array().unwrap().len(), 1);
    let (status, guide) = request(
        &app,
        "member-token-1",
        "GET",
        &format!("/api/profiles/1/live/{channel}/guide"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(guide["availability"], "Unsupported");
    println!(
        "Public M3U API qualification: channels={}, source_grants={}, elapsed_seconds={}",
        page["channels"].as_array().unwrap().len(),
        source["streams"].as_array().unwrap().len(),
        start.elapsed().as_secs()
    );
}

#[tokio::test]
async fn live_page_snapshot_changes_for_configuration_catalog_and_query_scope() {
    let app = fixture();
    {
        let db=app.db.lock().unwrap();
        db.execute_batch("INSERT INTO providers(id,name,url,username,password,enabled,enable_live,enable_movies,enable_series)VALUES(10,'Snapshot live','https://provider.example','private-user','private-password',1,1,0,0);INSERT INTO provider_ownership VALUES(10,1);INSERT INTO provider_live(id,provider_id,stream_id,name,category_id,category,ordinal)VALUES('snap-one',10,'1','One','news','News',1),('snap-two',10,'2','Two','news','News',2);INSERT INTO provider_live_categories_v2 VALUES(10,'news','News',1);").unwrap();
    }
    crate::test_support::encrypt_fixture_sources(&app);
    async fn read(app:&App,path:&str)->(StatusCode,Value){request(app,"member-token-1","GET",path,Value::Null).await}
    let (status,first)=read(&app,"/api/profiles/1/live/channels?limit=1").await;
    assert_eq!(status,StatusCode::OK,"{first}");
    assert!(first["snapshot_id"].as_str().unwrap().starts_with("live-v1:"));
    assert_eq!(first["next_cursor"],"1");
    let (_,second)=read(&app,"/api/profiles/1/live/channels?offset=1&limit=2").await;
    assert_eq!(first["snapshot_id"],second["snapshot_id"]);
    let (_,filtered)=read(&app,"/api/profiles/1/live/channels?group_id=news").await;
    assert_ne!(first["snapshot_id"],filtered["snapshot_id"]);
    {
        let db=app.db.lock().unwrap();
        db.execute("UPDATE provider_live SET name='Changed during paging' WHERE id='snap-two'",[]).unwrap();
        db.execute("INSERT INTO provider_live_generations VALUES(10,1) ON CONFLICT(provider_id) DO UPDATE SET generation=generation+1",[]).unwrap();
    }
    let (_,changed)=read(&app,"/api/profiles/1/live/channels?offset=1&limit=1").await;
    assert_ne!(first["snapshot_id"],changed["snapshot_id"]);
    assert_eq!(changed["channels"][0]["card"]["title"],"Changed during paging");
    {
        let db=app.db.lock().unwrap();
        db.execute("UPDATE provider_live_categories_v2 SET name='Updated group' WHERE provider_id=10",[]).unwrap();
        db.execute("UPDATE provider_live_generations SET generation=generation+1 WHERE provider_id=10",[]).unwrap();
    }
    let (_,groups)=read(&app,"/api/profiles/1/live/channels?offset=1&limit=1").await;
    assert_ne!(changed["snapshot_id"],groups["snapshot_id"]);
    let (_,config)=read(&app,"/api/profiles/1/integrations").await;
    let (status,_)=request(&app,"member-token-1","PUT","/api/profiles/1/integrations",json!({"revision":0,"browsing_plugin":config["configuration"]["browsing_plugin"],"assignments":config["configuration"]["assignments"]})).await;
    assert_eq!(status,StatusCode::OK);
    let (_,configured)=read(&app,"/api/profiles/1/live/channels?offset=1&limit=1").await;
    assert_ne!(groups["snapshot_id"],configured["snapshot_id"]);
    assert_eq!(groups["channels"],configured["channels"]);
    assert!(!configured.to_string().contains("private-password"));
    assert_eq!(request(&app,"member-token-2","GET","/api/profiles/1/live/channels?offset=1",Value::Null).await.0,StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn live_browsing_and_search_follow_cursors_past_ten_thousand() {
    let app=fixture();
    {
        let db=app.db.lock().unwrap();
        db.execute_batch("INSERT INTO providers(id,name,url,username,password,enabled,enable_live,enable_movies,enable_series)VALUES(10,'Large catalog','https://provider.example','fixture-user','fixture-password',1,1,0,0);INSERT INTO provider_ownership VALUES(10,1);WITH RECURSIVE rows(n) AS (VALUES(0) UNION ALL SELECT n+1 FROM rows WHERE n<10040) INSERT INTO provider_live(id,provider_id,stream_id,name,category_id,category,ordinal) SELECT 'large-'||n,10,CAST(n AS TEXT),'Large channel '||n,'news','News',n FROM rows;").unwrap();
    }
    crate::test_support::encrypt_fixture_sources(&app);
    for filter in ["", "&search=Large", "&group_id=news"] {
        let path=format!("/api/profiles/1/live/channels?offset=10000&limit=40{filter}");
        let (status,first)=request(&app,"member-token-1","GET",&path,Value::Null).await;
        assert_eq!(status,StatusCode::OK,"{first}");
        assert_eq!(first["channels"].as_array().unwrap().len(),40);
        assert_eq!(first["channels"][0]["id"],"large-10000");
        assert_eq!(first["next_cursor"],"10040");
        let path=format!("/api/profiles/1/live/channels?offset={}&limit=40{filter}",first["next_cursor"].as_str().unwrap());
        let (status,last)=request(&app,"member-token-1","GET",&path,Value::Null).await;
        assert_eq!(status,StatusCode::OK,"{last}");
        assert_eq!(last["channels"].as_array().unwrap().len(),1);
        assert_eq!(last["channels"][0]["id"],"large-10040");
        assert_eq!(last["snapshot_id"],first["snapshot_id"]);
        assert_eq!(last["complete"],true);
        assert!(last["next_cursor"].is_null());
    }
    let(status,end)=request(&app,"member-token-1","GET","/api/profiles/1/live/channels?offset=4294967295&limit=200",Value::Null).await;
    assert_eq!(status,StatusCode::OK,"{end}");
    assert_eq!(end["channels"],json!([]));assert_eq!(end["complete"],true);assert!(end["next_cursor"].is_null());
}
