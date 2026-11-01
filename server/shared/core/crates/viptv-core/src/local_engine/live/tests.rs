use super::*;

struct Secrets(Mutex<std::collections::HashMap<String, String>>);
impl SecretStore for Secrets {
    fn get(&self, key: String) -> Result<Option<String>, CoreError> {
        Ok(self.0.lock().unwrap().get(&key).cloned())
    }
    fn put(&self, key: String, value: String) -> Result<(), CoreError> {
        self.0.lock().unwrap().insert(key, value);
        Ok(())
    }
    fn remove(&self, key: String) -> Result<(), CoreError> {
        self.0.lock().unwrap().remove(&key);
        Ok(())
    }
}
#[test]
fn live_directory_and_grants_preserve_scope_and_never_export_private_uris() {
    let dir = std::env::temp_dir().join(format!("viptv-native-live-{}", uuid::Uuid::new_v4()));
    let engine = LocalEngine::new(
        dir.join("live.sqlite").to_string_lossy().into(),
        Arc::new(Secrets(Mutex::new(Default::default()))),
    )
    .unwrap();
    engine
        .create_profile("other".into(), "Other".into())
        .unwrap();
    let public=engine.configure_source_integration("default".into(),0,"m3u".into(),serde_json::json!({"playlist_url":"https://subscription.example/list.m3u?token=private"}).to_string()).unwrap();
    assert!(!public.contains("token=private"));
    let connection: IntegrationConnection = serde_json::from_str(&public).unwrap();
    let channels=parse_m3u("#EXTM3U\n#EXTINF:-1 tvg-id=\"news\" group-title=\"News\",News TV\nhttps://media.example/live.m3u8?token=private\n",&url::Url::parse("https://subscription.example/list.m3u").unwrap(),&connection.id).unwrap();
    let channel_id = channels[0].channel.id.clone();
    engine.live_cache.lock().unwrap().insert(
        connection.id.clone(),
        Cache {
            revision: 0,
            expires: std::time::Instant::now() + std::time::Duration::from_secs(300),
            channels,
            groups: vec![],
            guides: Default::default(),
            xmltv: None,
        },
    );
    let page = engine.live_channels("default".into(), "{}".into()).unwrap();
    assert!(!page.contains("token=private"));
    assert!(!page.contains("media.example"));
    let end: LivePage = serde_json::from_str(&engine.live_channels(
        "default".into(), r#"{"offset":4294967295,"limit":200}"#.into(),
    ).unwrap()).unwrap();
    assert!(end.channels.is_empty() && end.complete && end.next_cursor.is_none());
    let sources: serde_json::Value = serde_json::from_str(
        &engine
            .live_sources("default".into(), channel_id.clone())
            .unwrap(),
    )
    .unwrap();
    let opened: serde_json::Value = serde_json::from_str(
        &engine
            .open_source(sources[0]["id"].as_str().unwrap().into())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(opened["live"], true);
    assert_eq!(opened["position_millis"], 0);
    let guide: serde_json::Value = serde_json::from_str(
        &engine
            .live_guide("default".into(), channel_id.clone())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(guide["availability"], "Unsupported");
    assert!(guide["programs"].as_array().unwrap().is_empty());
    assert!(engine.live_sources("other".into(), channel_id).is_err());
    engine
        .remove_source_integration("default".into(), 1, connection.id)
        .unwrap();
    assert!(
        engine
            .open_source(sources[0]["id"].as_str().unwrap().into())
            .is_err()
    );
    drop(engine);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn live_xmltv_is_coalesced_across_selected_channels_and_config_changes_discard_results() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let dir = std::env::temp_dir().join(format!("viptv-live-guide-{}", uuid::Uuid::new_v4()));
    let engine = Arc::new(
        LocalEngine::new(
            dir.join("live.sqlite").to_string_lossy().into(),
            Arc::new(Secrets(Mutex::new(Default::default()))),
        )
        .unwrap(),
    );
    let connection:IntegrationConnection=serde_json::from_str(&engine.configure_source_integration("default".into(),0,"m3u".into(),serde_json::json!({"playlist_url":"https://fixture.example/list.m3u","epg_url":"https://fixture.example/guide.xml"}).to_string()).unwrap()).unwrap();
    let reads = Arc::new(AtomicUsize::new(0));
    let guide_reads = reads.clone();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let stamp = |n| {
        chrono::DateTime::<chrono::Utc>::from_timestamp(n, 0)
            .unwrap()
            .format("%Y%m%d%H%M%S %z")
            .to_string()
    };
    let xml = format!(
        "<tv><programme channel=\"one\" start=\"{}\" stop=\"{}\"><title>One program</title></programme><programme channel=\"two\" start=\"{}\" stop=\"{}\"><title>Two program</title></programme></tv>",
        stamp(now - 60),
        stamp(now + 3600),
        stamp(now - 60),
        stamp(now + 3600)
    );
    *engine.live_fixture.lock().unwrap() = Some(Arc::new(move |_, url| {
        if url.ends_with("list.m3u") {
            Ok((url.into(),"#EXTM3U\n#EXTINF:-1 tvg-id=\"one\" group-title=\"News\",One\nhttps://media.example/one.m3u8\n#EXTINF:-1 tvg-id=\"two\" group-title=\"News\",Two\nhttps://media.example/two.m3u8\n".into()))
        } else {
            guide_reads.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(20));
            Ok((url.into(), xml.clone()))
        }
    }));
    let page: LivePage =
        serde_json::from_str(&engine.live_channels("default".into(), "{}".into()).unwrap())
            .unwrap();
    assert_eq!(page.channels.len(), 2);
    assert_eq!(page.groups.len(), 1);
    let first = page.channels[0].id.clone();
    let second = page.channels[1].id.clone();
    let a = engine.clone();
    let b = engine.clone();
    let one = std::thread::spawn(move || a.live_guide("default".into(), first).unwrap());
    let two = std::thread::spawn(move || b.live_guide("default".into(), second).unwrap());
    for result in [one.join().unwrap(), two.join().unwrap()] {
        let guide: LiveGuide = serde_json::from_str(&result).unwrap();
        assert_eq!(guide.programs.len(), 1);
        assert_eq!(guide.availability, Availability::Available);
    }
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    engine
        .remove_source_integration("default".into(), 1, connection.id)
        .unwrap();
    assert!(
        engine
            .live_sources("default".into(), page.channels[0].id.clone())
            .is_err()
    );
    drop(engine);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn live_operation_wait_is_bounded_and_different_connections_do_not_share_a_fetch_lock() {
    let dir = std::env::temp_dir().join(format!("viptv-live-lock-{}", uuid::Uuid::new_v4()));
    let engine = LocalEngine::new(
        dir.join("live.sqlite").to_string_lossy().into(),
        Arc::new(Secrets(Mutex::new(Default::default()))),
    )
    .unwrap();
    let a = engine.live_fetch_gate("a").unwrap();
    let b = engine.live_fetch_gate("b").unwrap();
    let held = a.lock().unwrap();
    let start = std::time::Instant::now();
    assert!(super::http::lock_until(&a, start + std::time::Duration::from_millis(20)).is_err());
    assert!(start.elapsed() < std::time::Duration::from_millis(200));
    assert!(super::http::lock_until(&b, deadline()).is_ok());
    drop(held);
    drop(engine);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn inflight_live_directory_cannot_publish_after_profile_revision_changes() {
    let dir = std::env::temp_dir().join(format!("viptv-live-revoke-{}", uuid::Uuid::new_v4()));
    let engine = Arc::new(
        LocalEngine::new(
            dir.join("live.sqlite").to_string_lossy().into(),
            Arc::new(Secrets(Mutex::new(Default::default()))),
        )
        .unwrap(),
    );
    let connection: IntegrationConnection = serde_json::from_str(
        &engine
            .configure_source_integration(
                "default".into(),
                0,
                "m3u".into(),
                serde_json::json!({"playlist_url":"https://fixture.example/list.m3u"}).to_string(),
            )
            .unwrap(),
    )
    .unwrap();
    let changed = engine.clone();
    *engine.live_fixture.lock().unwrap() = Some(Arc::new(move |_, url| {
        changed
            .remove_source_integration("default".into(), 1, connection.id.clone())
            .unwrap();
        Ok((
            url.into(),
            "#EXTM3U\n#EXTINF:-1,One\nhttps://media.example/one.m3u8\n".into(),
        ))
    }));
    assert!(engine.live_channels("default".into(), "{}".into()).is_err());
    assert!(engine.live_cache.lock().unwrap().is_empty());
    *engine.live_fixture.lock().unwrap() = None;
    drop(engine);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn xtream_directory_categories_and_guide_are_host_owned_and_credentials_stay_private() {
    use base64::Engine;
    let dir = std::env::temp_dir().join(format!("viptv-live-xtream-{}", uuid::Uuid::new_v4()));
    let engine = LocalEngine::new(
        dir.join("live.sqlite").to_string_lossy().into(),
        Arc::new(Secrets(Mutex::new(Default::default()))),
    )
    .unwrap();
    engine.configure_source_integration("default".into(),0,"xtream".into(),serde_json::json!({"server_url":"https://fixture.example","username":"fixture-user","password":"fixture-pass"}).to_string()).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    *engine.live_fixture.lock().unwrap() = Some(Arc::new(move |_, raw| {
        let uri = url::Url::parse(raw).unwrap();
        let query = uri
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(query["username"], "fixture-user");
        assert_eq!(query["password"], "fixture-pass");
        let value = match query["action"].as_ref() {
            "get_live_streams" => {
                serde_json::json!([{ "stream_id":7,"name":"Fixture News","category_id":99,"epg_channel_id":"news"}])
            }
            "get_live_categories" => {
                serde_json::json!([{ "category_id":99,"category_name":"News"}])
            }
            "get_short_epg" => {
                assert_eq!(query["stream_id"], "7");
                serde_json::json!({"epg_listings":[{"id":"one","title":base64::engine::general_purpose::STANDARD.encode("Fixture program"),"description":base64::engine::general_purpose::STANDARD.encode("Fixture description"),"start_timestamp":now-60,"stop_timestamp":now+3600}]})
            }
            _ => panic!("unexpected fixture action"),
        };
        Ok((raw.into(), value.to_string()))
    }));
    let raw = engine.live_channels("default".into(), "{}".into()).unwrap();
    assert!(!raw.contains("fixture-user") && !raw.contains("fixture-pass"));
    let page: LivePage = serde_json::from_str(&raw).unwrap();
    assert_eq!(page.groups[0].label, "News");
    assert_eq!(page.channels[0].group_name.as_deref(), Some("News"));
    let guide: LiveGuide = serde_json::from_str(
        &engine
            .live_guide("default".into(), page.channels[0].id.clone())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(guide.programs[0].title, "Fixture program");
    let candidates: Vec<viptv_engine::stremio::SourceCandidate> = serde_json::from_str(
        &engine
            .live_sources("default".into(), page.channels[0].id.clone())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(candidates.len(), 1);
    let opened: serde_json::Value =
        serde_json::from_str(&engine.open_source(candidates[0].id.clone()).unwrap()).unwrap();
    assert_eq!(opened["live"], true);
    assert!(
        opened["url"]
            .as_str()
            .unwrap()
            .contains("/live/fixture-user/fixture-pass/7.ts")
    );
    drop(engine);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn m3u_multi_categories_are_independent_memberships_and_legacy_json_defaults() {
    let dir = std::env::temp_dir().join(format!("viptv-live-groups-{}", uuid::Uuid::new_v4()));
    let engine = LocalEngine::new(
        dir.join("live.sqlite").to_string_lossy().into(),
        Arc::new(Secrets(Mutex::new(Default::default()))),
    )
    .unwrap();
    let connection: IntegrationConnection = serde_json::from_str(
        &engine
            .configure_source_integration(
                "default".into(),
                0,
                "m3u".into(),
                serde_json::json!({"playlist_url":"https://fixture.example/list.m3u"}).to_string(),
            )
            .unwrap(),
    )
    .unwrap();
    *engine.live_fixture.lock().unwrap() = Some(Arc::new(|_, _| {
        Ok(("https://fixture.example/list.m3u".into(),"#EXTM3U\n#EXTINF:-1 tvg-id=\"one\" group-title=\"Animation;Kids;Religious;Kids;;\",One\nhttps://stream.example/one.m3u8\n#EXTINF:-1 tvg-id=\"two\" group-title=\"News\",Two\nhttps://stream.example/two.m3u8\n".into()))
    }));
    let page: LivePage =
        serde_json::from_str(&engine.live_channels("default".into(), "{}".into()).unwrap())
            .unwrap();
    assert_eq!(page.groups.len(), 4);
    assert_eq!(page.channels[0].group_name.as_deref(), Some("Animation"));
    assert_eq!(page.channels[0].group_ids.len(), 3);
    for name in ["Animation", "Kids", "Religious"] {
        let id = &page.groups.iter().find(|g| g.label == name).unwrap().id;
        let filtered: LivePage = serde_json::from_str(
            &engine
                .live_channels(
                    "default".into(),
                    serde_json::json!({"group_id":id}).to_string(),
                )
                .unwrap(),
        )
        .unwrap();
        assert_eq!(filtered.channels.len(), 1);
        assert_eq!(filtered.channels[0].id, page.channels[0].id);
    }
    let mut legacy = serde_json::to_value(&page.channels[0]).unwrap();
    legacy.as_object_mut().unwrap().remove("group_ids");
    let decoded: LiveChannel = serde_json::from_value(legacy).unwrap();
    assert!(decoded.group_ids.is_empty());
    assert!(decoded.in_group(decoded.group_id.as_deref().unwrap()));
    drop(engine);
    std::fs::remove_dir_all(dir).unwrap();
}
