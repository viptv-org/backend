use super::*;

struct Memory(Mutex<std::collections::HashMap<String, String>>);
impl SecretStore for Memory {
    fn get(&self, k: String) -> Result<Option<String>, CoreError> {
        Ok(self.0.lock().unwrap().get(&k).cloned())
    }
    fn put(&self, k: String, v: String) -> Result<(), CoreError> {
        self.0.lock().unwrap().insert(k, v);
        Ok(())
    }
    fn remove(&self, k: String) -> Result<(), CoreError> {
        self.0.lock().unwrap().remove(&k);
        Ok(())
    }
}
#[test]
#[ignore = "reads the real published public M3U fixture through scoped HTTPS"]
fn real_public_m3u_directory_uses_host_fetch_cache_and_live_grants() {
    let dir = std::env::temp_dir().join(format!("viptv-public-live-{}", uuid::Uuid::new_v4()));
    let engine = LocalEngine::new(
        dir.join("live.sqlite").to_string_lossy().into(),
        Arc::new(Memory(Mutex::new(Default::default()))),
    )
    .unwrap();
    engine
        .configure_source_integration(
            "default".into(),
            0,
            "m3u".into(),
            serde_json::json!({"playlist_url":"https://iptv-org.github.io/iptv/countries/us.m3u"})
                .to_string(),
        )
        .unwrap();
    let start = std::time::Instant::now();
    let page: LivePage = serde_json::from_str(
        &engine
            .live_channels("default".into(), "{\"limit\":20}".into())
            .unwrap(),
    )
    .unwrap();
    assert!(!page.channels.is_empty());
    let first = page.channels[0].id.clone();
    let warm = std::time::Instant::now();
    let repeat: LivePage = serde_json::from_str(
        &engine
            .live_channels("default".into(), "{\"limit\":20}".into())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(repeat.channels[0].id, first);
    assert!(warm.elapsed().as_millis() < 500);
    let sources: Vec<viptv_engine::stremio::SourceCandidate> = serde_json::from_str(
        &engine
            .live_sources("default".into(), first.clone())
            .unwrap(),
    )
    .unwrap();
    assert!(!sources.is_empty());
    let open: serde_json::Value =
        serde_json::from_str(&engine.open_source(sources[0].id.clone()).unwrap()).unwrap();
    assert_eq!(open["live"], true);
    let guide: LiveGuide =
        serde_json::from_str(&engine.live_guide("default".into(), first).unwrap()).unwrap();
    assert_eq!(guide.availability, Availability::Unsupported);
    println!(
        "Public M3U qualification: channels={}, source_grants={}, elapsed_seconds={}, warm_cache_ms={}",
        page.channels.len(),
        sources.len(),
        start.elapsed().as_secs(),
        warm.elapsed().as_millis()
    );
    drop(engine);
}
