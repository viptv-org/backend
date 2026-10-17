use crate::CoreError;
use sha1::Digest;
use std::{
    path::Path,
    sync::{Arc, Mutex},
};
mod addons;
mod detail;
mod detail_cache;
mod browse;
mod history;
mod library;
mod profiles;
mod preferences;
mod live;
mod search;
mod secrets;
mod sources;
mod source_integrations;
mod subtitles;
mod subtitle_content;
pub use secrets::SecretStore;
use viptv_engine::{
    profile::{ProfileConfiguration, Scope},
    store::Store,
};

#[derive(uniffi::Object)]
pub struct LocalEngine {
    store: Arc<Mutex<Store>>,
    secrets: Arc<dyn SecretStore>,
    namespace: String,
    pending: Mutex<std::collections::HashMap<String, addons::PendingHome>>,
    details: Mutex<std::collections::HashMap<String, detail::PendingDetail>>,
    source_jobs: Mutex<std::collections::HashMap<String, sources::PendingSources>>,
    playable: Arc<Mutex<std::collections::HashMap<String, sources::Playable>>>,
    live_cache: Mutex<std::collections::HashMap<String,live::Cache>>,
    live_fetch_locks: Mutex<std::collections::HashMap<String,Arc<Mutex<()>>>>,
    #[cfg(all(test,feature="live-runtime"))]
    live_fixture: Mutex<Option<live::Fixture>>,
    enrichments: Mutex<std::collections::HashMap<String, detail::PendingEnrichment>>,
    detail_documents: Mutex<std::collections::HashMap<(String,String,String,String), detail_cache::Document>>,
    detail_snapshots: Mutex<std::collections::HashMap<(String,String,String,String,u32,u32),(Scope,std::time::Instant,viptv_engine::presentation::DetailSnapshot)>>,
    verified_cards: Mutex<std::collections::HashMap<(String,String,String,String), (Scope,viptv_engine::presentation::Card)>>,
    integration_jobs: Mutex<std::collections::HashMap<String, Arc<source_integrations::Job>>>,
    subtitle_jobs: Mutex<std::collections::HashMap<String, subtitles::PendingSubtitles>>,
    subtitle_selections: Mutex<std::collections::HashMap<String, subtitles::SelectedSubtitle>>,
}

fn local_scope(profile: &str) -> Scope {
    Scope {
        connection_id: "local".into(),
        account_id: "local".into(),
        profile_id: profile.into(),
        revision: 0,
    }
}

#[uniffi::export]
impl LocalEngine {
    #[uniffi::constructor]
    pub fn new(database_path: String, secrets: Arc<dyn SecretStore>) -> Result<Self, CoreError> {
        let path = Path::new(&database_path);
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|_| CoreError::Bridge)?;
        }
        let mut store = Store::open(path).map_err(|_| CoreError::Bridge)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .map_err(|_| CoreError::Bridge)?;
        }
        if store
            .profiles("local", "local")
            .map_err(|_| CoreError::Bridge)?
            .is_empty()
        {
            let profile =
                ProfileConfiguration::local("local".into(), "default".into(), "Default".into());
            store
                .save_profile(&profile, None)
                .map_err(|_| CoreError::Bridge)?;
        }
        Ok(Self {
            store: Arc::new(Mutex::new(store)),
            secrets,
            namespace: format!(
                "{:x}",
                sha1::Sha1::digest(
                    path.canonicalize()
                        .map_err(|_| CoreError::Bridge)?
                        .to_string_lossy()
                        .as_bytes()
                )
            ),
            pending: Mutex::new(std::collections::HashMap::new()),
            details: Mutex::new(std::collections::HashMap::new()),
            source_jobs: Mutex::new(std::collections::HashMap::new()),
            playable: Arc::new(Mutex::new(std::collections::HashMap::new())),
            live_cache: Mutex::new(std::collections::HashMap::new()),
            live_fetch_locks: Mutex::new(Default::default()),
            #[cfg(all(test,feature="live-runtime"))]
            live_fixture: Mutex::new(None),
            enrichments:Mutex::new(Default::default()),
            detail_snapshots:Mutex::new(Default::default()),
            detail_documents:Mutex::new(Default::default()),
            verified_cards: Mutex::new(std::collections::HashMap::new()),
            integration_jobs: Mutex::new(std::collections::HashMap::new()),
            subtitle_jobs: Mutex::new(std::collections::HashMap::new()),
            subtitle_selections: Mutex::new(std::collections::HashMap::new()),
        })
    }

    pub fn profiles(&self) -> Result<String, CoreError> {
        let store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        serde_json::to_string(
            &store
                .profiles("local", "local")
                .map_err(|_| CoreError::Bridge)?,
        )
        .map_err(|_| CoreError::Bridge)
    }

    pub fn home(&self, id: String) -> Result<String, CoreError> {
        let profile: ProfileConfiguration =
            serde_json::from_str(&self.profile(id)?).map_err(|_| CoreError::Bridge)?;
        let mut queue: Vec<_> = self.continue_row(&profile.scope)?.into_iter().collect();
        queue.extend(self.favorites_row(&profile.scope)?);
        let revision = self.home_revision(&profile.scope)?;
        let snapshot = viptv_engine::presentation::HomeSnapshot {
            revision,
            scope: profile.scope,
            hero: None,
            featured:vec![],
            rows: queue,
            complete: true,
        };
        serde_json::to_string(&snapshot).map_err(|_| CoreError::Bridge)
    }

    pub fn create_profile(&self, id: String, name: String) -> Result<String, CoreError> {
        if id.is_empty()
            || id.len() > 128
            || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || name.trim().is_empty()
            || name.chars().count() > 64
        {
            return Err(CoreError::InvalidInput);
        }
        let profile = ProfileConfiguration::local("local".into(), id, name.trim().into());
        let mut store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        if store
            .profiles("local", "local")
            .map_err(|_| CoreError::Bridge)?
            .len()
            >= 12
        {
            return Err(CoreError::InvalidInput);
        }
        store
            .save_profile(&profile, None)
            .map_err(|_| CoreError::Bridge)?;
        serde_json::to_string(&profile).map_err(|_| CoreError::Bridge)
    }

    pub fn profile(&self, id: String) -> Result<String, CoreError> {
        let profile = self
            .store
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .profile(&local_scope(&id))
            .map_err(|_| CoreError::Bridge)?
            .ok_or(CoreError::InvalidInput)?;
        serde_json::to_string(&profile).map_err(|_| CoreError::Bridge)
    }
}
