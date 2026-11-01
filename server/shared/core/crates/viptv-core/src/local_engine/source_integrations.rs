use super::*;
use serde_json::json;
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};
use viptv_engine::{
    capabilities::Capability,
    profile::IntegrationConnection,
    source_integration::{VsourcesConfiguration, vsources_schema},
};
pub(super) struct Job {
    scope: Scope,
    metadata_origin: Option<MetadataOrigin>,
    created: Instant,
    cancel: AtomicBool,
    state: Mutex<JobState>,
}
struct JobState {
    events: Vec<serde_json::Value>,
    done: bool,
    error: Option<String>,
}
impl LocalEngine {
    pub(super) fn source_key(&self, profile: &str, id: &str) -> String {
        format!("{}:source:{profile}:{id}", self.namespace)
    }
}
#[uniffi::export]
impl LocalEngine {
    pub fn source_integration_schemas(&self, profile: String) -> Result<String, CoreError> {
        let store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        let selected = store
            .profile(&local_scope(&profile))
            .map_err(|_| CoreError::Bridge)?
            .ok_or(CoreError::InvalidInput)?;
        let connection = store
            .connections("local")
            .map_err(|_| CoreError::Bridge)?
            .into_iter()
            .find(|c| {
                c.plugin_id == "vsources" && c.private_profile_id.as_deref() == Some(&profile)
            });
        let configured = connection.as_ref().is_some_and(|c| {
            self.secrets
                .get(self.source_key(&profile, &c.id))
                .ok()
                .flatten()
                .is_some()
        });
        let connections = store
            .connections("local")
            .map_err(|_| CoreError::Bridge)?
            .into_iter()
            .filter(|c| c.private_profile_id.as_deref() == Some(&profile))
            .collect::<Vec<_>>();
        let mut schemas = vec![vsources_schema(
            configured,
            connection.as_ref().map(|c| c.id.clone()),
        )];
        for plugin in ["xtream", "m3u"] {
            let connection = connections.iter().find(|c| c.plugin_id == plugin);
            schemas.push(viptv_engine::source_integration::live_schema(
                plugin,
                connection.is_some(),
                connection.map(|c| c.id.clone()),
                cfg!(feature = "live-runtime"),
            ));
        }
        serde_json::to_string(&json!({"schemas":schemas,"revision":selected.scope.revision,"connections":connections})).map_err(|_|CoreError::Bridge)
    }
    pub fn configure_source_integration(
        &self,
        profile: String,
        revision: u64,
        plugin_id: String,
        configuration_json: String,
    ) -> Result<String, CoreError> {
        if configuration_json.len() > 32768 {
            return Err(CoreError::InvalidInput);
        }
        let value: serde_json::Value =
            serde_json::from_str(&configuration_json).map_err(|_| CoreError::InvalidInput)?;
        match plugin_id.as_str() {
            "vsources" => {
                if !cfg!(feature = "vsources-runtime") {
                    return Err(CoreError::InvalidInput);
                }
                let config: VsourcesConfiguration =
                    serde_json::from_value(value.clone()).map_err(|_| CoreError::InvalidInput)?;
                config.validate().map_err(|_| CoreError::InvalidInput)?;
                #[cfg(feature = "vsources-runtime")]
                viptv_engine::vsources_runtime::engine(
                    &config,
                    Arc::new(viptv_engine::vsources_runtime::ScopedFetcher::new(
                        Arc::new(|| false),
                    )),
                )
                .map_err(|_| CoreError::InvalidInput)?;
            }
            "m3u" => {
                if !cfg!(feature = "live-runtime") {
                    return Err(CoreError::InvalidInput);
                }
                serde_json::from_value::<viptv_engine::live::M3uConfiguration>(value.clone())
                    .map_err(|_| CoreError::InvalidInput)?
                    .validate()
                    .map_err(|_| CoreError::InvalidInput)?;
            }
            "xtream" => {
                if !cfg!(feature = "live-runtime") {
                    return Err(CoreError::InvalidInput);
                }
                serde_json::from_value::<viptv_engine::live::XtreamConfiguration>(value.clone())
                    .map_err(|_| CoreError::InvalidInput)?
                    .validate()
                    .map_err(|_| CoreError::InvalidInput)?;
            }
            _ => return Err(CoreError::InvalidInput),
        }
        let mut store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        let selected = store
            .profile(&local_scope(&profile))
            .map_err(|_| CoreError::Bridge)?
            .ok_or(CoreError::InvalidInput)?;
        if selected.scope.revision != revision {
            return Err(CoreError::InvalidInput);
        }
        let existing = store
            .connections("local")
            .map_err(|_| CoreError::Bridge)?
            .into_iter()
            .find(|c| {
                c.plugin_id == plugin_id && c.private_profile_id.as_deref() == Some(&profile)
            });
        let connection = IntegrationConnection {
            id: existing
                .as_ref()
                .map(|c| c.id.clone())
                .unwrap_or_else(|| format!("{}:{}", plugin_id, uuid::Uuid::new_v4())),
            plugin_id: plugin_id.clone(),
            account_id: "local".into(),
            private_profile_id: Some(profile.clone()),
            name: match plugin_id.as_str() {
                "m3u" => "M3U live TV",
                "xtream" => "Xtream live TV",
                _ => "VIPTV Sources",
            }
            .into(),
            enabled: true,
            revision: existing
                .as_ref()
                .map(|c| c.revision.checked_add(1).ok_or(CoreError::InvalidInput))
                .transpose()?
                .unwrap_or(0),
            capabilities: if plugin_id == "vsources" {
                vec![Capability::Sources]
            } else {
                vec![Capability::Sources, Capability::LiveTv]
            },
        };
        let key = self.source_key(&profile, &connection.id);
        let old = self.secrets.get(key.clone())?;
        self.secrets.put(
            key.clone(),
            serde_json::to_string(&value).map_err(|_| CoreError::Bridge)?,
        )?;
        let saved = if existing.is_some() {
            store.replace_source_connection(&profile, revision, &connection)
        } else {
            store.install(&profile, &connection)
        };
        if saved.is_err() {
            if let Some(old) = old {
                let _ = self.secrets.put(key, old);
            } else {
                let _ = self.secrets.remove(key);
            }
            return Err(CoreError::Bridge);
        }
        serde_json::to_string(&connection).map_err(|_| CoreError::Bridge)
    }
    pub fn remove_source_integration(
        &self,
        profile: String,
        revision: u64,
        connection_id: String,
    ) -> Result<(), CoreError> {
        let mut store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        let selected = store
            .profile(&local_scope(&profile))
            .map_err(|_| CoreError::Bridge)?
            .ok_or(CoreError::InvalidInput)?;
        if selected.scope.revision != revision
            || !store
                .connections("local")
                .map_err(|_| CoreError::Bridge)?
                .iter()
                .any(|c| {
                    c.id == connection_id
                        && ["vsources", "xtream", "m3u"].contains(&c.plugin_id.as_str())
                        && c.private_profile_id.as_deref() == Some(&profile)
                })
        {
            return Err(CoreError::InvalidInput);
        }
        store
            .remove_connection(&connection_id)
            .map_err(|_| CoreError::Bridge)?;
        self.secrets
            .remove(self.source_key(&profile, &connection_id))
    }
    pub fn start_integration_sources(
        &self,
        profile: String,
        card_json: String,
    ) -> Result<String, CoreError> {
        #[cfg(not(feature = "vsources-runtime"))]
        {
            let _ = (profile, card_json);
            Err(CoreError::InvalidInput)
        }
        #[cfg(feature = "vsources-runtime")]
        {
            self.start_sdk(profile, card_json)
        }
    }
    pub fn poll_integration_sources(
        &self,
        profile: String,
        job_id: String,
        after: u64,
    ) -> Result<String, CoreError> {
        // Never retain the jobs-map lock while waiting for profile storage:
        // admission holds storage before replacing jobs.
        let job = self
            .integration_jobs
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .get(&job_id)
            .cloned()
            .ok_or(CoreError::InvalidInput)?;
        if job.scope.profile_id != profile
            || job.created.elapsed().as_secs() > 120
            || job.cancel.load(Ordering::Relaxed)
        {
            return Err(CoreError::InvalidInput);
        }
        let store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        if let Some(origin) = job.metadata_origin.as_ref() {
            store
                .validate_metadata_origin(&job.scope, origin)
                .map_err(|_| CoreError::InvalidInput)?;
        }
        let current = store
            .profile(&job.scope)
            .map_err(|_| CoreError::Bridge)?
            .ok_or(CoreError::InvalidInput)?;
        if current.scope.revision != job.scope.revision {
            return Err(CoreError::InvalidInput);
        }
        let state = job.state.lock().map_err(|_| CoreError::Bridge)?;
        Ok(json!({"id":job_id,"events":state.events.iter().filter(|e|e["seq"].as_u64().unwrap_or(0)>after).collect::<Vec<_>>(),"done":state.done,"error_code":state.error}).to_string())
    }
    pub fn cancel_integration_sources(
        &self,
        profile: String,
        job_id: String,
    ) -> Result<(), CoreError> {
        let mut jobs = self
            .integration_jobs
            .lock()
            .map_err(|_| CoreError::Bridge)?;
        if jobs
            .get(&job_id)
            .is_none_or(|job| job.scope.profile_id != profile)
        {
            return Err(CoreError::InvalidInput);
        }
        if let Some(job) = jobs.remove(&job_id) {
            job.cancel.store(true, Ordering::Relaxed);
        }
        Ok(())
    }
}
impl LocalEngine {
    pub(super) fn verified_source_card(
        &self,
        profile: &str,
        hint: &viptv_engine::presentation::Card,
    ) -> Result<(Scope, viptv_engine::presentation::Card), CoreError> {
        self.expected_source_card(profile, hint, true)
    }
    fn admit_integration_job(&self, scope: Scope) -> Result<(String, Arc<Job>), CoreError> {
        self.admit_integration_job_with_origin(scope, None)
    }
    fn admit_integration_job_with_origin(
        &self,
        scope: Scope,
        metadata_origin: Option<MetadataOrigin>,
    ) -> Result<(String, Arc<Job>), CoreError> {
        let id = uuid::Uuid::new_v4().to_string();
        let job = Arc::new(Job {
            scope: scope.clone(),
            metadata_origin,
            created: Instant::now(),
            cancel: AtomicBool::new(false),
            state: Mutex::new(JobState {
                events: vec![],
                done: false,
                error: None,
            }),
        });
        let mut jobs = self
            .integration_jobs
            .lock()
            .map_err(|_| CoreError::Bridge)?;
        jobs.retain(|_, old| {
            let keep =
                old.scope.profile_id != scope.profile_id && old.created.elapsed().as_secs() < 120;
            if !keep {
                old.cancel.store(true, Ordering::Relaxed);
            }
            keep
        });
        if jobs.len() >= 12 {
            return Err(CoreError::InvalidInput);
        }
        let mut grants = self.playable.lock().map_err(|_| CoreError::Bridge)?;
        // Replacing discovery cannot revoke a choice already given to a player.
        // Expiry and profile/configuration revisions still revoke it at open.
        grants.retain(|_, grant| {
            !grant.expires.is_some_and(|expiry| Instant::now() >= expiry)
                && (grant.scope.profile_id != scope.profile_id || grant.scope == scope)
        });
        jobs.insert(id.clone(), job.clone());
        Ok((id, job))
    }
}
#[cfg(feature = "vsources-runtime")]
impl LocalEngine {
    fn start_sdk(&self, profile: String, card_json: String) -> Result<String, CoreError> {
        if card_json.len() > 64 * 1024 {
            return Err(CoreError::InvalidInput);
        }
        let card: viptv_engine::presentation::Card =
            serde_json::from_str(&card_json).map_err(|_| CoreError::InvalidInput)?;
        card.validate_library()
            .map_err(|_| CoreError::InvalidInput)?;
        let (verified_scope, card) = self.verified_source_card(&profile, &card)?;
        let media = viptv_engine::vsources_runtime::media_ref(
            &card.identity,
            card.parent_identity.as_ref(),
            &[],
        )
        .map_err(|_| CoreError::InvalidInput)?;
        let store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        let selected = store
            .profile(&local_scope(&profile))
            .map_err(|_| CoreError::Bridge)?
            .ok_or(CoreError::InvalidInput)?;
        if verified_scope.revision != selected.scope.revision {
            return Err(CoreError::InvalidInput);
        }
        let connection = store
            .connections("local")
            .map_err(|_| CoreError::Bridge)?
            .into_iter()
            .find(|c| {
                c.plugin_id == "vsources"
                    && c.enabled
                    && c.private_profile_id.as_deref() == Some(&profile)
                    && selected.assignments.iter().any(|a| {
                        a.connection_id == c.id && a.enabled.contains(&Capability::Sources)
                    })
            })
            .ok_or(CoreError::InvalidInput)?;
        let raw = self
            .secrets
            .get(self.source_key(&profile, &connection.id))?
            .ok_or(CoreError::Bridge)?;
        let config: VsourcesConfiguration =
            serde_json::from_str(&raw).map_err(|_| CoreError::Bridge)?;
        if let Some(origin) = card.metadata_origin.as_ref() {
            store
                .validate_metadata_origin(&selected.scope, origin)
                .map_err(|_| CoreError::InvalidInput)?;
        }
        let (id, job) = self.admit_integration_job_with_origin(
            selected.scope.clone(),
            card.metadata_origin.clone(),
        )?;
        drop(store);
        let store = self.store.clone();
        let scope = job.scope.clone();
        let guard_job = job.clone();
        let authorize: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
            !guard_job.cancel.load(Ordering::Relaxed)
                && store.lock().ok().is_some_and(|store| {
                    store
                        .profile(&scope)
                        .ok()
                        .flatten()
                        .is_some_and(|profile| profile.scope == scope)
                        && guard_job.metadata_origin.as_ref().is_none_or(|origin| {
                            store.validate_metadata_origin(&scope, origin).is_ok()
                        })
                })
        });
        let playable = self.playable.clone();
        let worker_id = id.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build();
            let result = runtime.map_err(|_| ()).and_then(|rt| {
                rt.block_on(viptv_engine::vsources_runtime::resolve_progressive(
                    config,
                    media,
                    authorize.clone(),
                    None,
                    |streams| {
                        if !authorize() {
                            return;
                        }
                        let mut grants = playable.lock().unwrap();
                        if job.cancel.load(Ordering::Relaxed) {
                            return;
                        }
                        let capacity = 4096usize.saturating_sub(grants.len());
                        let sources = streams
                            .into_iter()
                            .take(capacity)
                            .map(|stream| {
                                let (id, grant) = sources::sdk_playable(
                                    job.scope.clone(),
                                    &card,
                                    &connection.id,
                                    &worker_id,
                                    stream,
                                );
                                let candidate = grant.candidate.clone();
                                grants.insert(id, grant);
                                candidate
                            })
                            .collect::<Vec<_>>();
                        let mut state = job.state.lock().unwrap();
                        let seq = state.events.len() + 1;
                        state.events.push(json!({"seq":seq,"sources":sources}));
                    },
                ))
                .map_err(|_| ())
            });
            let mut state = job.state.lock().unwrap();
            state.done = true;
            if result.is_err() {
                state.error = Some("source_unavailable".into());
            }
        });
        Ok(json!({"id":id}).to_string())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Secrets(Mutex<std::collections::HashMap<String, String>>);
    impl SecretStore for Secrets {
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
    fn credentials_are_write_only_and_feature_availability_is_honest() {
        let dir =
            std::env::temp_dir().join(format!("viptv-source-config-{}", uuid::Uuid::new_v4()));
        let secrets = Arc::new(Secrets::default());
        let engine = LocalEngine::new(
            dir.join("local.sqlite").to_string_lossy().into(),
            secrets.clone(),
        )
        .unwrap();
        engine
            .create_profile("other".into(), "Other".into())
            .unwrap();
        let schemas = engine.source_integration_schemas("default".into()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&schemas).unwrap();
        assert_eq!(
            value["schemas"][0]["runtime_enabled"],
            cfg!(feature = "vsources-runtime")
        );
        let configured = engine.configure_source_integration(
            "default".into(),
            0,
            "vsources".into(),
            json!({"tmdb_api_key":"fixture-write-only-key"}).to_string(),
        );
        if cfg!(feature = "vsources-runtime") {
            let public = configured.unwrap();
            assert!(!public.contains("fixture-write-only-key"));
            let id: serde_json::Value = serde_json::from_str(&public).unwrap();
            assert!(
                engine
                    .remove_source_integration("other".into(), 0, id["id"].as_str().unwrap().into())
                    .is_err()
            );
            assert!(
                !engine
                    .profiles()
                    .unwrap()
                    .contains("fixture-write-only-key")
            );
            assert_eq!(secrets.0.lock().unwrap().len(), 1);
            let update = engine
                .configure_source_integration(
                    "default".into(),
                    1,
                    "vsources".into(),
                    json!({"tmdb_api_key":"fixture-updated-key"}).to_string(),
                )
                .unwrap();
            assert!(!update.contains("fixture-updated-key"));
            let snapshot: serde_json::Value =
                serde_json::from_str(&engine.source_integration_schemas("default".into()).unwrap())
                    .unwrap();
            assert_eq!(snapshot["revision"], 2);
            assert_eq!(snapshot["schemas"][0]["state"], "Ready");
            assert!(
                snapshot["schemas"][0]["provider_choices"]
                    .as_array()
                    .unwrap()
                    .len()
                    > 20
            );

            engine
                .remove_source_integration("default".into(), 2, id["id"].as_str().unwrap().into())
                .unwrap();
            assert!(secrets.0.lock().unwrap().is_empty());
        } else {
            assert!(configured.is_err());
            assert!(secrets.0.lock().unwrap().is_empty());
        }
        drop(engine);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
#[cfg(all(test, feature = "vsources-runtime"))]
mod real_native_qualification {
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
    fn read_real_media(opened: &serde_json::Value) -> bool {
        use viptv_engine::egress::{HttpRequest, ScopedHttp};
        let Some(value) = opened["url"].as_str() else {
            return false;
        };
        let Ok(mut url) = url::Url::parse(value) else {
            return false;
        };
        let origin = url.origin();
        let mut headers = std::collections::BTreeMap::new();
        for pair in opened["headers"].as_array().into_iter().flatten() {
            if let (Some(name), Some(value)) = (pair[0].as_str(), pair[1].as_str()) {
                headers.insert(name.to_owned(), value.to_owned());
            }
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let http = ScopedHttp::new(Arc::new(|| true));
            let mut encrypted = false;
            for _ in 0..3 {
                let mut request = HttpRequest::get(url.clone());
                request.timeout = Some(std::time::Duration::from_secs(8));
                if url.origin() == origin {
                    request.headers = headers.clone();
                }
                let Ok(response) = http.probe(request, 16384).await else {
                    return false;
                };
                if !(200..300).contains(&response.status) {
                    return false;
                }
                let body = &response.body;
                if body.starts_with(b"#EXTM3U") {
                    let Ok(text) = std::str::from_utf8(body) else {
                        return false;
                    };
                    if let Some(key) = text.lines().find(|line| {
                        line.starts_with("#EXT-X-KEY:") && line.contains("METHOD=AES-128")
                    }) {
                        let Some(uri) = key
                            .split("URI=\"")
                            .nth(1)
                            .and_then(|tail| tail.split('"').next())
                        else {
                            return false;
                        };
                        let Ok(key_url) = response.url.join(uri) else {
                            return false;
                        };
                        let mut key_request = HttpRequest::get(key_url.clone());
                        key_request.timeout = Some(std::time::Duration::from_secs(8));
                        if key_url.origin() == origin {
                            key_request.headers = headers.clone();
                        }
                        let Ok(key) = http.probe(key_request, 32).await else {
                            return false;
                        };
                        if !(200..300).contains(&key.status)
                            || key.body.len() != 16
                            || key.truncated
                        {
                            return false;
                        }
                        encrypted = true;
                    }
                    let Some(next) = text
                        .lines()
                        .map(str::trim)
                        .find(|line| !line.is_empty() && !line.starts_with('#'))
                    else {
                        return false;
                    };
                    let Ok(next) = response.url.join(next) else {
                        return false;
                    };
                    url = next;
                    continue;
                }
                return (encrypted
                    && body.len() >= 188
                    && body.len() % 16 == 0
                    && std::str::from_utf8(body).is_err())
                    || body.first() == Some(&0x47)
                    || body.get(4..8).is_some_and(|bytes| {
                        matches!(bytes, b"ftyp" | b"styp" | b"moof" | b"moov")
                    })
                    || body.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]);
            }
            false
        })
    }
    #[test]
    #[ignore = "requires explicitly injected owned TMDB credential and real public provider access"]
    fn native_sdk_job_publishes_opaque_grants_and_early_cancel_preserves_selection() {
        let dir = std::env::temp_dir().join(format!("viptv-native-sdk-{}", uuid::Uuid::new_v4()));
        let engine = LocalEngine::new(
            dir.join("native.sqlite").to_string_lossy().into(),
            Arc::new(Memory(Mutex::new(Default::default()))),
        )
        .unwrap();
        let manifest = r#"{"id":"metadata","name":"Metadata","version":"1","resources":["meta"],"types":["movie"]}"#;
        let addon: serde_json::Value = serde_json::from_str(
            &engine
                .install_addon(
                    "default".into(),
                    "https://v3-cinemeta.strem.io/manifest.json".into(),
                    manifest.into(),
                )
                .unwrap(),
        )
        .unwrap();
        engine
            .configure_source_integration(
                "default".into(),
                1,
                "vsources".into(),
                json!({"tmdb_api_key":std::env::var("VIPTV_QA_TMDB_KEY").unwrap()}).to_string(),
            )
            .unwrap();
        let plan: serde_json::Value = serde_json::from_str(
            &engine
                .plan_detail(
                    "default".into(),
                    "movie".into(),
                    "tt1375666".into(),
                    addon["id"].as_str().unwrap().into(),
                )
                .unwrap(),
        )
        .unwrap();
        let detail: serde_json::Value = serde_json::from_str(
            &engine
                .project_detail(
                    plan["operation"].as_str().unwrap().into(),
                    json!({"meta":{"id":"tt1375666","name":"Inception","type":"movie"}})
                        .to_string(),
                    0,
                )
                .unwrap(),
        )
        .unwrap();
        let mut hint = detail["item"].clone();
        hint["identity"]["ids"] = json!([{"namespace":"tmdb","value":"1"}]);
        let job: serde_json::Value = serde_json::from_str(
            &engine
                .start_integration_sources("default".into(), hint.to_string())
                .unwrap(),
        )
        .unwrap();
        let job_id = job["id"].as_str().unwrap();
        let start = Instant::now();
        let selected = loop {
            let poll = engine
                .poll_integration_sources("default".into(), job_id.into(), 0)
                .unwrap();
            assert!(!poll.contains("request_headers"));
            assert!(!poll.contains("https://"));
            let poll: serde_json::Value = serde_json::from_str(&poll).unwrap();
            if let Some(id) = poll["events"].as_array().and_then(|events| {
                events
                    .iter()
                    .find_map(|event| event["sources"][0]["id"].as_str())
            }) {
                break id.to_string();
            }
            assert!(
                start.elapsed().as_secs() < 55 && poll["done"] != true,
                "No native SDK candidate qualified"
            );
            std::thread::sleep(std::time::Duration::from_millis(250));
        };
        engine
            .cancel_integration_sources("default".into(), job_id.into())
            .unwrap();
        let opened: serde_json::Value =
            serde_json::from_str(&engine.open_source(selected).unwrap()).unwrap();
        assert!(
            read_real_media(&opened),
            "SDK candidate did not yield a bounded real media read"
        );
        println!(
            "Native SDK opaque grant and media bytes qualified; metadata-only Stremio manifest; early cancel preserves selected source; elapsed_seconds={}",
            start.elapsed().as_secs()
        );
        drop(engine);
    }
}

#[cfg(test)]
#[path = "source_integrations/lifecycle_tests.rs"]
mod lifecycle_tests;

impl LocalEngine {
    pub(super) fn forget_integration_profile(&self, profile: &str) -> Result<(), CoreError> {
        self.integration_jobs
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .retain(|_, job| {
                let keep = job.scope.profile_id != profile;
                if !keep {
                    job.cancel.store(true, Ordering::Relaxed);
                }
                keep
            });
        Ok(())
    }
}
