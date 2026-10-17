use crate::{auth_integration_tests::fixture, test_support::request, *};
#[tokio::test]
async fn cached_m3u_multi_membership_and_xmltv_are_projected_without_new_egress() {
    let app = fixture();
    let(status,_)=request(&app,"member-token-1","PUT","/api/profiles/1/source-integrations",json!({"revision":0,"plugin_id":"m3u","configuration":{"playlist_url":"https://fixture.example/list.m3u","epg_url":"https://fixture.example/guide.xml.gz"}})).await;
    assert_eq!(status, StatusCode::OK);
    let (_, snapshot) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/source-integrations",
        Value::Null,
    )
    .await;
    let connection = snapshot["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["plugin_id"] == "m3u")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let channels=viptv_engine::live::parse_m3u("#EXTM3U\n#EXTINF:-1 tvg-id=\"one\" group-title=\"Animation;Kids;Religious\",One\nhttps://media.example/one.m3u8\n#EXTINF:-1 tvg-id=\"two\" group-title=\"Kids\",Two\nhttps://media.example/two.m3u8\n",&url::Url::parse("https://fixture.example/list.m3u").unwrap(),connection).unwrap();
    let channel = channels[0].channel.id.clone();
    let second = channels[1].channel.id.clone();
    let kids = channels[0]
        .groups
        .iter()
        .find(|g| g.label == "Kids")
        .unwrap()
        .id
        .clone();
    let now = util::now();
    let stamp = |seconds| {
        chrono::DateTime::<chrono::Utc>::from_timestamp(seconds, 0)
            .unwrap()
            .format("%Y%m%d%H%M%S +0000")
            .to_string()
    };
    let xml:Arc<str>=format!("<tv><programme channel=\"one\" start=\"{}\" stop=\"{}\"><title>First program</title></programme><programme channel=\"two\" start=\"{}\" stop=\"{}\"><title>Second program</title></programme></tv>",stamp(now),stamp(now+3600),stamp(now),stamp(now+3600)).into();
    let expiry = Instant::now() + Duration::from_secs(60);
    let groups = channels[0].groups.clone();
    app.live_cache.lock().await.insert(
        connection.into(),
        crate::engine_v3::live::Cache {
            revision: 1,
            expires: Instant::now() + Duration::from_secs(300),
            channels,
            groups,
            guides: Default::default(),
            xmltv: Some((expiry, xml)),
        },
    );
    let (status, page) = request(
        &app,
        "member-token-1",
        "GET",
        &format!("/api/profiles/1/live/channels?group_id={kids}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["channels"].as_array().unwrap().len(), 2);
    assert_eq!(page["channels"][0]["group_name"], "Animation");
    assert_eq!(
        page["channels"][0]["group_ids"].as_array().unwrap().len(),
        3
    );
    for (id, title) in [(channel, "First program"), (second, "Second program")] {
        let (status, guide) = request(
            &app,
            "member-token-1",
            "GET",
            &format!("/api/profiles/1/live/{id}/guide"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{guide}");
        assert_eq!(guide["availability"], "Available");
        assert_eq!(guide["programs"][0]["title"], title);
    }
    let caches = app.live_cache.lock().await;
    let cache = caches.get(connection).unwrap();
    assert_eq!(cache.guides.len(), 2);
    assert_eq!(cache.xmltv.as_ref().unwrap().0, expiry);
}
