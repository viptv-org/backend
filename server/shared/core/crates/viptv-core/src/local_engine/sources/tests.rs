use super::*;
use std::{collections::HashMap, sync::Mutex};
#[derive(Default)]
struct Secrets(Mutex<HashMap<String, String>>);
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
fn fixture() -> (LocalEngine, std::path::PathBuf, Vec<String>) {
    let dir = std::env::temp_dir().join(format!("viptv-source-progress-{}", uuid::Uuid::new_v4()));
    let engine = LocalEngine::new(
        dir.join("local.sqlite").to_string_lossy().into(),
        Arc::new(Secrets::default()),
    )
    .unwrap();
    let mut providers = Vec::new();
    for name in ["first", "second"] {
        let addon:Value=serde_json::from_str(&engine.install_addon("default".into(),format!("https://{name}.example/manifest.json"),format!(r#"{{"id":"{name}","name":"{name}","version":"1","resources":["stream"],"types":["movie"]}}"#)).unwrap()).unwrap();
        providers.push(addon["id"].as_str().unwrap().to_owned());
    }
    (engine, dir, providers)
}
fn plan(engine: &LocalEngine, target: &str) -> String {
    let plan: Value = serde_json::from_str(
        &engine
            .plan_sources("default".into(), "movie".into(), target.into())
            .unwrap(),
    )
    .unwrap();
    plan["operation"].as_str().unwrap().into()
}
fn responses(provider: &str, count: usize) -> String {
    json!([{"provider":provider,"body":json!({"streams":(0..count).map(|i|json!({"name":format!("1080p {i}"),"url":format!("https://media.example/{provider}/{i}.mp4"),"behaviorHints":{"proxyHeaders":{"request":{"Referer":"https://provider.example/"}}}})).collect::<Vec<_>>()} ).to_string()}]).to_string()
}
fn append(engine: &LocalEngine, op: &str, body: String, finish: bool) -> Vec<SourceCandidate> {
    serde_json::from_str(
        &engine
            .project_sources_partial(op.into(), body, finish)
            .unwrap(),
    )
    .unwrap()
}

#[test]
fn incremental_results_keep_ids_open_grants_and_finish_closes_only_operation() {
    let (engine, dir, providers) = fixture();
    let op = plan(&engine, "movie");
    let first = append(&engine, &op, responses(&providers[0], 1), false);
    assert_eq!(first.len(), 1);
    let replay = append(&engine, &op, responses(&providers[0], 2), false);
    assert_eq!(first[0].id, replay[0].id);
    assert_eq!(replay.len(), 1);
    let both = append(&engine, &op, responses(&providers[1], 1), false);
    assert_eq!(both.len(), 2);
    assert_eq!(first[0].id, both[0].id);
    assert!(
        !serde_json::to_string(&both)
            .unwrap()
            .contains("media.example")
    );
    let opened: Value =
        serde_json::from_str(&engine.open_source(first[0].id.clone()).unwrap()).unwrap();
    assert_eq!(opened["headers"][0][0], "Referer");
    let done = append(&engine, &op, "[]".into(), true);
    assert_eq!(done.len(), 2);
    assert_eq!(done[0].id, first[0].id);
    assert!(
        engine
            .project_sources_partial(op.clone(), responses(&providers[1], 1), false)
            .is_err()
    );
    engine
        .cancel_source_discovery("default".into(), op)
        .unwrap();
    assert!(engine.open_source(first[0].id.clone()).is_ok());
    drop(engine);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn cancel_replacement_and_wrong_profile_cannot_publish_or_revoke_selected_choice() {
    let (engine, dir, providers) = fixture();
    let op = plan(&engine, "movie");
    let first = append(&engine, &op, responses(&providers[0], 1), false);
    assert!(
        engine
            .cancel_source_discovery("other".into(), op.clone())
            .is_err()
    );
    let replacement = plan(&engine, "another-movie");
    assert!(
        engine
            .project_sources_partial(op.clone(), responses(&providers[1], 1), false)
            .is_err()
    );
    engine
        .cancel_source_discovery("default".into(), op)
        .unwrap(); // delayed cleanup must not remove the replacement
    assert_eq!(
        append(&engine, &replacement, responses(&providers[1], 1), false).len(),
        1
    );
    engine
        .cancel_source_discovery("default".into(), replacement.clone())
        .unwrap();
    assert!(
        engine
            .project_sources_partial(replacement, responses(&providers[0], 1), false)
            .is_err()
    );
    assert!(engine.open_source(first[0].id.clone()).is_ok());
    drop(engine);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn profile_integration_revision_and_deleted_profiles_reject_pending_and_issued_results() {
    for delete in [false, true] {
        let (engine, dir, providers) = fixture();
        engine
            .create_profile("other".into(), "Other".into())
            .unwrap();
        let op = plan(&engine, "movie");
        let first = append(&engine, &op, responses(&providers[0], 1), false);
        if delete {
            engine.delete_profile("default".into(), "".into()).unwrap();
        } else {
            engine
                .set_addon_enabled("default".into(), providers[1].clone(), false)
                .unwrap();
        }
        assert!(
            engine
                .project_sources_partial(op, responses(&providers[1], 1), false)
                .is_err()
        );
        assert!(engine.open_source(first[0].id.clone()).is_err());
        drop(engine);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn invalid_batches_are_atomic_and_each_provider_keeps_capacity() {
    let (engine, dir, providers) = fixture();
    let op = plan(&engine, "movie");
    assert!(
        engine
            .project_sources_partial(
                op.clone(),
                json!([{"provider":providers[0]},{"provider":"unplanned"}]).to_string(),
                false
            )
            .is_err()
    );
    assert!(
        engine
            .project_sources_partial(
                op.clone(),
                json!([{"provider":providers[0]},{"provider":providers[0]}]).to_string(),
                false
            )
            .is_err()
    );
    let first = append(&engine, &op, responses(&providers[0], 80), false);
    assert_eq!(first.len(), 40);
    let both = append(&engine, &op, responses(&providers[1], 80), true);
    assert_eq!(both.len(), 80);
    assert_eq!(
        both.iter()
            .filter(|s| s.provider_id == providers[1])
            .count(),
        40
    );
    assert!(engine.open_source(first[0].id.clone()).is_ok());
    drop(engine);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn operation_expiry_and_unrelated_grants_remain_distinct() {
    let (engine, dir, providers) = fixture();
    let op = plan(&engine, "movie");
    let first = append(&engine, &op, responses(&providers[0], 1), false);
    engine
        .source_jobs
        .lock()
        .unwrap()
        .get_mut(&op)
        .unwrap()
        .created = std::time::Instant::now() - std::time::Duration::from_secs(121);
    assert!(
        engine
            .project_sources_partial(op, responses(&providers[1], 1), false)
            .is_err()
    );
    assert!(engine.open_source(first[0].id.clone()).is_ok());
    // A live/SDK grant must not be erased by Stremio projection or replacement.
    engine
        .playable
        .lock()
        .unwrap()
        .get_mut(&first[0].id)
        .unwrap()
        .kind = "live".into();
    let next = plan(&engine, "new-movie");
    append(&engine, &next, responses(&providers[1], 1), true);
    assert!(engine.open_source(first[0].id.clone()).is_ok());
    engine
        .playable
        .lock()
        .unwrap()
        .get_mut(&first[0].id)
        .unwrap()
        .expires = Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
    assert!(engine.open_source(first[0].id.clone()).is_err());
    drop(engine);
    std::fs::remove_dir_all(dir).unwrap();
}
