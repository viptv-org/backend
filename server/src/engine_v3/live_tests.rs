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
        "/api/v3/profiles/1/live/channels",
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
        "/api/v3/profiles/1/live/opaque-live/guide",
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
        "/api/v3/profiles/1/live/opaque-live/sources",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{sources}");
    assert_eq!(sources["live"], true);
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/v3/profiles/1/source-ranking",
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
            "/api/v3/profiles/1/live/channels",
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
        "/api/v3/profiles/1/integrations",
        json!({"revision":0,"browsing_plugin":"stremio","assignments":[]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, page) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v3/profiles/1/live/channels",
        Value::Null,
    )
    .await;
    assert_eq!(page["availability"], "NeedsConfiguration");
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/v3/profiles/1/live/opaque-live/sources",
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
    let(status,_)=request(&app,"member-token-1","PUT","/api/v3/profiles/1/source-integrations",json!({"revision":0,"plugin_id":"m3u","configuration":{"playlist_url":"https://provider.example/list.m3u?token=private"}})).await;
    assert_eq!(status, StatusCode::OK);
    let (_, schemas) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v3/profiles/1/source-integrations",
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
    let channels=viptv_engine::live::parse_m3u("#EXTM3U\n#EXTINF:-1 tvg-id=\"news\",News TV\n#EXTVLCOPT:http-referrer=https://headers.example/source-context\nhttps://media.example/live.m3u8?token=private\n",&url::Url::parse("https://provider.example/list.m3u").unwrap(),connection).unwrap();
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
        "/api/v3/profiles/1/live/channels",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!page.to_string().contains("token=private"));
    let (status, guide) = request(
        &app,
        "member-token-1",
        "GET",
        &format!("/api/v3/profiles/1/live/{id}/guide"),
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
        &format!("/api/v3/profiles/1/live/{id}/sources"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(source["live"], true);
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/v3/profiles/1/source-ranking",
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
    let(status,_)=request(&app,"member-token-1","PUT","/api/v3/profiles/1/source-integrations",json!({"revision":0,"plugin_id":"m3u","configuration":{"playlist_url":"https://iptv-org.github.io/iptv/countries/us.m3u"}})).await;
    assert_eq!(status, StatusCode::OK);
    let start = Instant::now();
    let (status, page) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v3/profiles/1/live/channels?limit=20",
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
        &format!("/api/v3/profiles/1/live/{channel}/sources"),
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
        "/api/v3/profiles/1/source-ranking",
        json!({"ids":[source["streams"][0]["id"]]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ranked["streams"].as_array().unwrap().len(), 1);
    let (status, guide) = request(
        &app,
        "member-token-1",
        "GET",
        &format!("/api/v3/profiles/1/live/{channel}/guide"),
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
