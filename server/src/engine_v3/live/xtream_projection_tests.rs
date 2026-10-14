use super::xtream_profile_tests::setup;
use crate::{test_support::request, *};
pub(super) async fn cached() -> (App, String, String, u64) {
    use viptv_engine::live::*;
    let (app, connection) = setup().await;
    let credentials = XtreamConfiguration {
        server_url: "https://provider.example".into(),
        username: "fixture-user".into(),
        password: "fixture-password".into(),
    };
    let mut channels=parse_xtream(r#"[{"stream_id":7,"name":"News","category_id":"news","stream_icon":"https://images.example/news.png"}]"#,&connection,&credentials).unwrap();
    let groups = xtream_groups(r#"[{"category_id":"news","category_name":"News group"}]"#).unwrap();
    channels[0].channel.group_name = Some(groups[0].label.clone());
    let channel = channels[0].channel.id.clone();
    let now = util::now();
    let guide = LiveGuide {
        channel_id: channel.clone(),
        programs: parse_xtream_guide(
            &json!({"epg_listings":[{"title":"TmV3cw==","start_timestamp":now,"stop_timestamp":now+3600}]}),
            &channel,
        ),
        availability: viptv_engine::capabilities::Availability::Available,
        reason: None,
    };
    app.live_cache.lock().await.insert(
        connection.clone(),
        super::Cache {
            revision: 1,
            expires: Instant::now() + Duration::from_secs(60),
            channels,
            groups,
            guides: HashMap::from([(
                channel.clone(),
                (Instant::now() + Duration::from_secs(60), guide),
            )]),
            xmltv: None,
        },
    );
    (app, connection, channel, now as u64)
}
#[tokio::test]
async fn private_xtream_projects_cached_catalog_guide_and_revocable_opaque_live_sources() {
    let (app, connection, channel, now) = cached().await;
    let (status, page) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v3/profiles/1/live/channels?group_id=news",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["channels"].as_array().unwrap().len(), 1);
    assert_eq!(page["channels"][0]["connection_id"], connection);
    assert_eq!(page["groups"][0]["label"], "News group");
    let (status, guide) = request(
        &app,
        "member-token-1",
        "GET",
        &format!("/api/v3/profiles/1/live/{channel}/guide"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{guide}");
    assert_eq!(guide["programs"][0]["title"], "News");
    assert_eq!(guide["programs"][0]["start_millis"], now * 1000);
    let (status, sources) = request(
        &app,
        "member-token-1",
        "POST",
        &format!("/api/v3/profiles/1/live/{channel}/sources"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{sources}");
    assert_eq!(sources["live"], true);
    for public in [&page, &guide, &sources] {
        let text = public.to_string();
        assert!(
            !text.contains("fixture-user")
                && !text.contains("fixture-password")
                && !text.contains("https://provider.example")
        );
    }
    let id = sources["streams"][0]["id"].as_str().unwrap();
    let play = |name| json!({"request_id":name,"stream_id":id,"client":{"platform":"roku","max_width":1920,"max_height":1080,"video_codecs":["h264"],"audio_codecs":["aac"]}});
    let (status, missing) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/playback",
        play("before_remove"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(missing["error_code"], "gateway_required");
    assert_eq!(
        request(
            &app,
            "member-token-2",
            "POST",
            &format!("/api/v3/profiles/1/live/{channel}/sources"),
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, _) = request(
        &app,
        "member-token-1",
        "DELETE",
        &format!("/api/v3/profiles/1/source-integrations/{connection}?revision=1"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, expired) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/playback",
        play("after_remove"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(expired["error_code"], "source_configuration_changed");
}
