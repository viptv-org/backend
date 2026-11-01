use super::*;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::atomic::{AtomicBool, Ordering},
};
#[derive(Default)]
struct Secrets {
    values: Mutex<HashMap<String, String>>,
    fail_remove: AtomicBool,
}
impl SecretStore for Secrets {
    fn get(&self, key: String) -> Result<Option<String>, CoreError> {
        Ok(self.values.lock().unwrap().get(&key).cloned())
    }
    fn put(&self, key: String, value: String) -> Result<(), CoreError> {
        self.values.lock().unwrap().insert(key, value);
        Ok(())
    }
    fn remove(&self, key: String) -> Result<(), CoreError> {
        if self.fail_remove.load(Ordering::Relaxed) {
            return Err(CoreError::Bridge);
        }
        self.values.lock().unwrap().remove(&key);
        Ok(())
    }
}
struct Fixture {
    engine: LocalEngine,
    dir: std::path::PathBuf,
    secrets: Arc<Secrets>,
    shared: String,
    default_private: String,
    other_private: String,
}
fn fixture() -> Fixture {
    let dir = std::env::temp_dir().join(format!("viptv-profile-cleanup-{}", uuid::Uuid::new_v4()));
    let secrets = Arc::new(Secrets::default());
    let engine = LocalEngine::new(
        dir.join("local.sqlite").to_string_lossy().into(),
        secrets.clone(),
    )
    .unwrap();
    engine
        .create_profile("other".into(), "Other".into())
        .unwrap();
    let manifest = r#"{"id":"shared","name":"Shared","version":"1","resources":["catalog","meta","stream","subtitles"],"types":["movie","series"],"catalogs":[{"id":"top","type":"movie"}]}"#;
    let shared: Value = serde_json::from_str(
        &engine
            .install_addon(
                "default".into(),
                "https://shared.example/account-secret/manifest.json".into(),
                manifest.into(),
            )
            .unwrap(),
    )
    .unwrap();
    engine
        .install_addon(
            "other".into(),
            "https://shared.example/account-secret/manifest.json".into(),
            manifest.into(),
        )
        .unwrap();
    let shared = shared["id"].as_str().unwrap().to_owned();
    for (profile, id) in [
        ("default", "m3u:default-private"),
        ("other", "m3u:other-private"),
    ] {
        let connection = viptv_engine::profile::IntegrationConnection {
            id: id.into(),
            plugin_id: "m3u".into(),
            account_id: "local".into(),
            private_profile_id: Some(profile.into()),
            name: profile.into(),
            enabled: true,
            revision: 0,
            capabilities: vec![
                viptv_engine::capabilities::Capability::LiveTv,
                viptv_engine::capabilities::Capability::Sources,
            ],
        };
        engine
            .store
            .lock()
            .unwrap()
            .install(profile, &connection)
            .unwrap();
        engine
            .secrets
            .put(
                engine.source_key(profile, id),
                format!("{profile}-private-value"),
            )
            .unwrap();
    }
    engine
        .set_profile_pin("default".into(), "".into(), "1234".into())
        .unwrap();
    Fixture {
        engine,
        dir,
        secrets,
        shared,
        default_private: "m3u:default-private".into(),
        other_private: "m3u:other-private".into(),
    }
}
fn source(engine: &LocalEngine, profile: &str, provider: &str) -> (String, String) {
    let plan: Value = serde_json::from_str(
        &engine
            .plan_sources(profile.into(), "movie".into(), "tt123".into())
            .unwrap(),
    )
    .unwrap();
    let op = plan["operation"].as_str().unwrap().to_owned();
    let output:Value=serde_json::from_str(&engine.project_sources_partial(op.clone(),json!([{"provider":provider,"body":r#"{"streams":[{"url":"https://media.example/owned.mp4"}]}"#}]).to_string(),false).unwrap()).unwrap();
    (op, output[0]["id"].as_str().unwrap().to_owned())
}
fn detail(engine: &LocalEngine, profile: &str, provider: &str) {
    let plan: Value = serde_json::from_str(
        &engine
            .plan_detail(
                profile.into(),
                "series".into(),
                "tt123".into(),
                provider.into(),
            )
            .unwrap(),
    )
    .unwrap();
    engine.project_detail(plan["operation"].as_str().unwrap().into(),r#"{"meta":{"id":"tt123","name":"Series","videos":[{"id":"episode-1","season":1,"episode":1}]}}"#.into(),0).unwrap();
}

#[test]
fn deleting_profile_clears_its_private_resources_and_preserves_shared_and_other_profile() {
    let f = fixture();
    let (default_op, default_grant) = source(&f.engine, "default", &f.shared);
    let (_, other_grant) = source(&f.engine, "other", &f.shared);
    detail(&f.engine, "default", &f.shared);
    detail(&f.engine, "other", &f.shared);
    f.engine.update_profile_preferences("default".into(),0,json!({"preferred_audio_language":"fr","preferred_subtitle_language":null,"subtitles_enabled":true,"source_selection":"ask","max_quality_height":null}).to_string()).unwrap();
    f.engine.update_profile_preferences("other".into(),0,json!({"preferred_audio_language":"ja","preferred_subtitle_language":null,"subtitles_enabled":true,"source_selection":"ask","max_quality_height":null}).to_string()).unwrap();
    let plan: Value = serde_json::from_str(
        &f.engine
            .plan_subtitles(
                "default".into(),
                "movie".into(),
                "tt123".into(),
                "{}".into(),
            )
            .unwrap(),
    )
    .unwrap();
    let subtitle_op = plan["operation"].as_str().unwrap().to_owned();
    let default_key = f.engine.source_key("default", &f.default_private);
    let other_key = f.engine.source_key("other", &f.other_private);
    let shared_key = format!("{}:{}", f.engine.namespace, f.shared);
    f.engine
        .delete_profile("default".into(), "1234".into())
        .unwrap();
    assert!(f.engine.profile("default".into()).is_err());
    assert!(f.engine.open_source(default_grant).is_err());
    assert!(f.engine.open_source(other_grant).is_ok());
    assert!(
        f.engine
            .project_sources_partial(default_op, "[]".into(), false)
            .is_err()
    );
    assert!(
        f.engine
            .project_subtitles(subtitle_op, "[]".into())
            .is_err()
    );
    let connections = f.engine.store.lock().unwrap().connections("local").unwrap();
    assert!(!connections.iter().any(|c| c.id == f.default_private));
    assert!(connections.iter().any(|c| c.id == f.other_private));
    assert!(connections.iter().any(|c| c.id == f.shared));
    assert!(!f.secrets.values.lock().unwrap().contains_key(&default_key));
    assert!(f.secrets.values.lock().unwrap().contains_key(&other_key));
    assert!(f.secrets.values.lock().unwrap().contains_key(&shared_key));
    assert!(
        f.engine
            .verified_cards
            .lock()
            .unwrap()
            .keys()
            .all(|(profile, _, _, _)| profile != "default")
    );
    assert!(
        f.engine
            .verified_cards
            .lock()
            .unwrap()
            .keys()
            .any(|(profile, _, _, _)| profile == "other")
    );
    assert!(
        f.engine
            .detail_page_for_provider(
                "other".into(),
                "series".into(),
                "tt123".into(),
                f.shared.clone(),
                0,
                24
            )
            .unwrap()
            .is_some()
    );
    let other_preferences: Value =
        serde_json::from_str(&f.engine.profile_preferences("other".into()).unwrap()).unwrap();
    assert_eq!(
        other_preferences["preferences"]["preferred_audio_language"],
        "ja"
    );
    f.engine
        .create_profile("default".into(), "Fresh profile".into())
        .unwrap();
    let fresh: Value =
        serde_json::from_str(&f.engine.profile_preferences("default".into()).unwrap()).unwrap();
    assert!(fresh["preferences"]["preferred_audio_language"].is_null());
    drop(f.engine);
    std::fs::remove_dir_all(f.dir).unwrap();
}

#[test]
fn wrong_pin_does_not_delete_connections_vault_entries_or_valid_grants() {
    let f = fixture();
    let (op, grant) = source(&f.engine, "default", &f.shared);
    let key = f.engine.source_key("default", &f.default_private);
    assert!(
        f.engine
            .delete_profile("default".into(), "9999".into())
            .is_err()
    );
    assert!(f.engine.profile("default".into()).is_ok());
    assert!(f.engine.open_source(grant).is_ok());
    assert!(
        f.engine
            .project_sources_partial(op, "[]".into(), false)
            .is_ok()
    );
    assert!(f.secrets.values.lock().unwrap().contains_key(&key));
    assert!(
        f.engine
            .store
            .lock()
            .unwrap()
            .connections("local")
            .unwrap()
            .iter()
            .any(|c| c.id == f.default_private)
    );
    assert!(
        f.engine
            .set_addon_enabled("other".into(), f.default_private, false)
            .is_err(),
        "Private source assignment must not poison another profile"
    );
    drop(f.engine);
    std::fs::remove_dir_all(f.dir).unwrap();
}

#[test]
fn failed_vault_cleanup_does_not_report_committed_deletion_as_failed_or_restore_access() {
    let f = fixture();
    let (op, grant) = source(&f.engine, "default", &f.shared);
    f.secrets.fail_remove.store(true, Ordering::Relaxed);
    f.engine
        .delete_profile("default".into(), "1234".into())
        .unwrap();
    assert!(f.engine.profile("default".into()).is_err());
    assert!(f.engine.open_source(grant).is_err());
    assert!(
        f.engine
            .project_sources_partial(op, "[]".into(), false)
            .is_err()
    );
    assert!(
        f.engine
            .store
            .lock()
            .unwrap()
            .connections("local")
            .unwrap()
            .iter()
            .all(|c| c.private_profile_id.as_deref() != Some("default"))
    );
    assert!(
        f.secrets
            .values
            .lock()
            .unwrap()
            .contains_key(&f.engine.source_key("other", &f.other_private))
    );
    drop(f.engine);
    std::fs::remove_dir_all(f.dir).unwrap();
}

#[test]
fn recreated_profile_id_cannot_authorize_results_or_grants_from_deleted_incarnation() {
    let f = fixture();
    let (operation, grant) = source(&f.engine, "default", &f.shared);
    let plan: Value = serde_json::from_str(
        &f.engine
            .plan_detail(
                "default".into(),
                "series".into(),
                "tt123".into(),
                f.shared.clone(),
            )
            .unwrap(),
    )
    .unwrap();
    let detail_operation = plan["operation"].as_str().unwrap().to_owned();
    // A projector may have already taken its work out of the queue while parsing.
    let popped = f
        .engine
        .details
        .lock()
        .unwrap()
        .remove(&detail_operation)
        .unwrap();
    let prior = popped.scope.revision;
    f.engine
        .delete_profile("default".into(), "1234".into())
        .unwrap();
    let recreated: ProfileConfiguration = serde_json::from_str(
        &f.engine
            .create_profile("default".into(), "Fresh".into())
            .unwrap(),
    )
    .unwrap();
    assert!(recreated.scope.revision > prior);
    f.engine.install_addon("default".into(),"https://shared.example/account-secret/manifest.json".into(),r#"{"id":"shared","name":"Shared","version":"1","resources":["catalog","meta","stream"],"types":["movie","series"],"catalogs":[{"id":"top","type":"movie"}]}"#.into()).unwrap();
    f.engine
        .details
        .lock()
        .unwrap()
        .insert(detail_operation.clone(), popped);
    assert!(
        f.engine
            .project_detail(
                detail_operation,
                r#"{"meta":{"id":"tt123","name":"Old incarnation"}}"#.into(),
                0
            )
            .is_err()
    );
    assert!(
        f.engine
            .project_sources_partial(operation, "[]".into(), false)
            .is_err()
    );
    assert!(f.engine.open_source(grant).is_err());
    assert!(
        f.engine
            .verified_cards
            .lock()
            .unwrap()
            .keys()
            .all(|(owner, _, _, _)| owner != "default")
    );
    drop(f.engine);
    std::fs::remove_dir_all(f.dir).unwrap();
}
