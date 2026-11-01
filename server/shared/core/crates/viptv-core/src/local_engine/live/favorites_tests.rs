use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
#[derive(Default)]
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
fn hook(engine: &LocalEngine, reads: Arc<AtomicUsize>) {
    *engine.live_fixture.lock().unwrap() = Some(Arc::new(move |_, url| {
        reads.fetch_add(1, Ordering::SeqCst);
        Ok((url.into(),"#EXTM3U\n#EXTINF:-1 tvg-id=\"news\" group-title=\"News\",News\nhttps://media.example/private.m3u8?credential=fixture-private-value\n".into()))
    }));
}
fn ids(raw: String) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(&raw).unwrap()["channel_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect()
}
fn channel(engine: &LocalEngine) -> String {
    serde_json::from_str::<LivePage>(&engine.live_channels("default".into(), "{}".into()).unwrap())
        .unwrap()
        .channels[0]
        .id
        .clone()
}

#[test]
fn favorite_uses_owned_directory_persists_and_reads_never_fetch_upstream() {
    let dir = std::env::temp_dir().join(format!("viptv-live-favorite-{}", uuid::Uuid::new_v4()));
    let path = dir.join("local.sqlite").to_string_lossy().into_owned();
    let secrets = Arc::new(Secrets::default());
    let engine = LocalEngine::new(path.clone(), secrets.clone()).unwrap();
    engine
        .create_profile("other".into(), "Other".into())
        .unwrap();
    let connection:IntegrationConnection=serde_json::from_str(&engine.configure_source_integration("default".into(),0,"m3u".into(),serde_json::json!({"playlist_url":"https://fixture.example/list.m3u?credential=fixture-private-value"}).to_string()).unwrap()).unwrap();
    let reads = Arc::new(AtomicUsize::new(0));
    hook(&engine, reads.clone());
    let id = channel(&engine);
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert_eq!(
        ids(engine
            .set_live_favorite("default".into(), id.clone(), true)
            .unwrap()),
        vec![id.clone()]
    );
    assert_eq!(
        ids(engine
            .set_live_favorite("default".into(), id.clone(), true)
            .unwrap()),
        vec![id.clone()]
    );
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert!(
        engine
            .set_live_favorite("other".into(), id.clone(), true)
            .is_err()
    );
    assert!(
        engine
            .set_live_favorite("other".into(), id.clone(), false)
            .is_err()
    );
    engine
        .live_cache
        .lock()
        .unwrap()
        .get_mut(&connection.id)
        .unwrap()
        .expires = std::time::Instant::now() - std::time::Duration::from_secs(1);
    let public = engine.live_favorites("default".into()).unwrap();
    assert_eq!(ids(public.clone()), vec![id.clone()]);
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert!(!public.contains("private") && !public.contains("https://"));
    drop(engine);
    let reopened = LocalEngine::new(path.clone(), secrets).unwrap();
    hook(&reopened, reads.clone());
    assert!(ids(reopened.live_favorites("default".into()).unwrap()).is_empty());
    channel(&reopened);
    assert_eq!(
        ids(reopened.live_favorites("default".into()).unwrap()),
        vec![id.clone()]
    );
    assert!(
        reopened
            .set_live_favorite("default".into(), "unknown-channel".into(), true)
            .is_err()
    );
    reopened
        .live_cache
        .lock()
        .unwrap()
        .get_mut(&connection.id)
        .unwrap()
        .channels
        .clear();
    assert!(ids(reopened.live_favorites("default".into()).unwrap()).is_empty());
    assert!(
        ids(reopened
            .set_live_favorite("default".into(), id, false)
            .unwrap())
        .is_empty()
    );
    drop(reopened);
    let db = std::fs::read(path).unwrap();
    assert!(!String::from_utf8_lossy(&db).contains("fixture-private-value"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn connection_disable_replace_and_remove_prune_results_without_changing_vod_library() {
    let dir = std::env::temp_dir().join(format!(
        "viptv-live-favorite-revision-{}",
        uuid::Uuid::new_v4()
    ));
    let engine = LocalEngine::new(
        dir.join("local.sqlite").to_string_lossy().into(),
        Arc::new(Secrets::default()),
    )
    .unwrap();
    let connection: IntegrationConnection = serde_json::from_str(
        &engine
            .configure_source_integration(
                "default".into(),
                0,
                "m3u".into(),
                serde_json::json!({"playlist_url":"https://fixture.example/one.m3u"}).to_string(),
            )
            .unwrap(),
    )
    .unwrap();
    hook(&engine, Arc::new(AtomicUsize::new(0)));
    let id = channel(&engine);
    engine
        .set_live_favorite("default".into(), id.clone(), true)
        .unwrap();
    engine
        .configure_source_integration(
            "default".into(),
            1,
            "m3u".into(),
            serde_json::json!({"playlist_url":"https://fixture.example/two.m3u"}).to_string(),
        )
        .unwrap();
    assert!(ids(engine.live_favorites("default".into()).unwrap()).is_empty());
    channel(&engine);
    assert!(ids(engine.live_favorites("default".into()).unwrap()).is_empty());
    engine
        .set_live_favorite("default".into(), id.clone(), true)
        .unwrap();
    engine
        .set_addon_enabled("default".into(), connection.id.clone(), false)
        .unwrap();
    assert!(ids(engine.live_favorites("default".into()).unwrap()).is_empty());
    assert!(
        engine
            .set_live_favorite("default".into(), id.clone(), true)
            .is_err()
    );
    engine
        .set_addon_enabled("default".into(), connection.id.clone(), true)
        .unwrap();
    assert_eq!(
        ids(engine.live_favorites("default".into()).unwrap()),
        vec![id]
    );
    let revision =
        serde_json::from_str::<ProfileConfiguration>(&engine.profile("default".into()).unwrap())
            .unwrap()
            .scope
            .revision;
    engine
        .remove_source_integration("default".into(), revision, connection.id)
        .unwrap();
    assert!(ids(engine.live_favorites("default".into()).unwrap()).is_empty());
    let library: serde_json::Value =
        serde_json::from_str(&engine.library("default".into(), 0, 40).unwrap()).unwrap();
    assert!(library["cards"].as_array().unwrap().is_empty());
    drop(engine);
    std::fs::remove_dir_all(dir).unwrap();
}
