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
fn fixture() -> (Arc<LocalEngine>, std::path::PathBuf, Vec<String>) {
    let dir = std::env::temp_dir().join(format!("viptv-sdk-lifecycle-{}", uuid::Uuid::new_v4()));
    let engine = Arc::new(
        LocalEngine::new(
            dir.join("local.sqlite").to_string_lossy().into(),
            Arc::new(Secrets::default()),
        )
        .unwrap(),
    );
    let providers=["first","second"].map(|name|{
        let addon:serde_json::Value=serde_json::from_str(&engine.install_addon("default".into(),format!("https://{name}.example/manifest.json"),
            format!(r#"{{"id":"{name}","name":"{name}","version":"1","resources":["meta"],"types":["movie","series"]}}"#)).unwrap()).unwrap();
        addon["id"].as_str().unwrap().to_owned()
    });
    (engine, dir, providers.into())
}
fn metadata(
    engine: &LocalEngine,
    provider: &str,
    name: &str,
) -> viptv_engine::presentation::DetailSnapshot {
    let scope = engine
        .store
        .lock()
        .unwrap()
        .profile(&local_scope("default"))
        .unwrap()
        .unwrap()
        .scope;
    let origin = engine
        .store
        .lock()
        .unwrap()
        .metadata_origin(&scope, provider)
        .unwrap();
    let expected: viptv_engine::presentation::Card=serde_json::from_value(json!({"identity":{"id":"shared-id","kind":"series","ids":[]},"metadata_origin":origin,"provider_id":provider,"title":name,"badges":[]})).unwrap();
    let plan: serde_json::Value = serde_json::from_str(
        &engine
            .plan_detail_for_origin(
                "default".into(),
                "series".into(),
                "shared-id".into(),
                serde_json::to_string(&expected).unwrap(),
            )
            .unwrap(),
    )
    .unwrap();
    serde_json::from_str(&engine.project_detail(plan["operation"].as_str().unwrap().into(),json!({"meta":{"id":"shared-id","name":name,"type":"series","imdb_id":if name=="First"{"tt1111111"}else{"tt2222222"},"videos":[{"id":"shared-episode","name":format!("{name} episode"),"season":1,"episode":1}]}}).to_string(),0).unwrap()).unwrap()
}

#[test]
fn provider_scoped_metadata_cache_and_sdk_proof_do_not_mix_identical_opaque_ids() {
    let (engine, dir, providers) = fixture();
    let first = metadata(&engine, &providers[0], "First");
    let second = metadata(&engine, &providers[1], "Second");
    assert!(
        engine
            .detail_page("default".into(), "series".into(), "shared-id".into(), 0, 24)
            .unwrap()
            .is_none(),
        "Legacy origin cannot choose an ambiguous cached producer"
    );
    for (provider, name) in providers.iter().zip(["First", "Second"]) {
        let page: serde_json::Value = serde_json::from_str(
            &engine
                .detail_page_for_origin(
                    "default".into(),
                    "series".into(),
                    "shared-id".into(),
                    serde_json::to_string(if name == "First" {
                        &first.item
                    } else {
                        &second.item
                    })
                    .unwrap(),
                    0,
                    24,
                )
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(page["item"]["title"], name);
        assert_eq!(page["episodes"][0]["provider_id"], provider.as_str());
    }
    let mut hint = first.item.clone();
    hint.identity.ids = vec![viptv_engine::identity::ExternalId {
        namespace: "tmdb".into(),
        value: "forged-sibling-id".into(),
    }];
    let (_, trusted) = engine.verified_source_card("default", &hint).unwrap();
    assert_eq!(trusted.title, "First");
    assert_eq!(trusted.identity.ids, first.item.identity.ids);
    let (_, episode) = engine
        .verified_source_card("default", &first.episodes[0])
        .unwrap();
    assert_eq!(episode.title, "First episode");
    let mut forged = first.episodes[0].clone();
    forged.identity.episode.as_mut().unwrap().episode = 2;
    assert!(engine.verified_source_card("default", &forged).is_err());
    engine
        .set_addon_enabled("default".into(), providers[0].clone(), false)
        .unwrap();
    assert!(engine.verified_source_card("default", &first.item).is_err());
    assert!(
        engine
            .detail_page_for_provider(
                "default".into(),
                "series".into(),
                "shared-id".into(),
                providers[0].clone(),
                0,
                24
            )
            .unwrap()
            .is_none()
    );
    assert!(
        engine
            .plan_detail_for_origin(
                "default".into(),
                "series".into(),
                "shared-id".into(),
                serde_json::to_string(&first.item).unwrap()
            )
            .is_err(),
        "Disabled explicit origin must not fall through to sibling metadata"
    );
    drop(engine);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn sdk_job_replacement_preserves_issued_choice_and_cancels_only_pending_work() {
    let (engine, dir, providers) = fixture();
    let detail = metadata(&engine, &providers[0], "First");
    let scope: Scope =
        serde_json::from_str::<ProfileConfiguration>(&engine.profile("default".into()).unwrap())
            .unwrap()
            .scope;
    let (old, old_job) = engine.admit_integration_job(scope.clone()).unwrap();
    let candidate = viptv_engine::stremio::SourceCandidate {
        id: "issued".into(),
        provider_id: "vsources:fixture".into(),
        label: "Source".into(),
        detail: None,
        quality_height: None,
        audio_languages: vec![],
        presentation: None,
        ..Default::default()
    };
    engine.playable.lock().unwrap().insert(
        "issued".into(),
        sources::Playable {
            metadata_origin: None,
            provider_origin: None,
            metadata_card: None,
            scope: scope.clone(),
            candidate,
            kind: "series".into(),
            media_id: detail.item.identity.id,
            url: "https://media.example/selected.mp4".into(),
            headers: vec![],
            required_audio_index: Some(1),
            job_id: Some(old.clone()),
            expires: Some(Instant::now() + std::time::Duration::from_secs(60)),
        },
    );
    let (new, _) = engine.admit_integration_job(scope).unwrap();
    assert!(old_job.cancel.load(Ordering::Relaxed));
    assert!(
        engine
            .poll_integration_sources("default".into(), old, 0)
            .is_err()
    );
    let opened: serde_json::Value =
        serde_json::from_str(&engine.open_source("issued".into()).unwrap()).unwrap();
    assert_eq!(opened["required_audio_index"], 1);
    engine
        .cancel_integration_sources("default".into(), new)
        .unwrap();
    assert!(engine.open_source("issued".into()).is_ok());
    drop(engine);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn sdk_poll_releases_jobs_map_before_waiting_for_profile_storage() {
    let (engine, dir, _) = fixture();
    let scope: Scope =
        serde_json::from_str::<ProfileConfiguration>(&engine.profile("default".into()).unwrap())
            .unwrap()
            .scope;
    let (id, job) = engine.admit_integration_job(scope).unwrap();
    let store = engine.store.lock().unwrap();
    let worker = engine.clone();
    let poll = std::thread::spawn(move || worker.poll_integration_sources("default".into(), id, 0));
    // The cloned job proves poll reached its profile-storage wait. A borrowed
    // jobs-map entry cannot satisfy this observation while storage is held.
    let began = Instant::now();
    while Arc::strong_count(&job) < 3 && began.elapsed() < std::time::Duration::from_secs(1) {
        std::thread::yield_now();
    }
    let cloned = Arc::strong_count(&job) >= 3;
    let mut acquired = false;
    while began.elapsed() < std::time::Duration::from_secs(1) {
        if engine.integration_jobs.try_lock().is_ok() {
            acquired = true;
            break;
        }
        std::thread::yield_now();
    }
    drop(store);
    assert!(cloned && acquired);
    assert!(poll.join().unwrap().is_ok());
    drop(engine);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn deleted_profile_cancels_sdk_job_and_never_republishes_its_issued_grant() {
    let (engine, dir, _) = fixture();
    engine
        .create_profile("other".into(), "Other".into())
        .unwrap();
    let scope =
        serde_json::from_str::<ProfileConfiguration>(&engine.profile("default".into()).unwrap())
            .unwrap()
            .scope;
    let (id, job) = engine.admit_integration_job(scope).unwrap();
    engine.delete_profile("default".into(), "".into()).unwrap();
    assert!(job.cancel.load(Ordering::Relaxed));
    assert!(
        engine
            .poll_integration_sources("default".into(), id, 0)
            .is_err()
    );
    assert!(engine.integration_jobs.lock().unwrap().is_empty());
    drop(engine);
    std::fs::remove_dir_all(dir).unwrap();
}
