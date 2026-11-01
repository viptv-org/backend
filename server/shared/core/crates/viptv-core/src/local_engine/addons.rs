use super::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use viptv_engine::{
    capabilities::Capability,
    presentation::{HomeRow, HomeSnapshot, RowState},
    profile::{IntegrationConnection, Scope},
    stremio,
};

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct StoredAddon {
    pub(super) manifest_url: String,
    pub(super) manifest: Value,
}

#[derive(Clone, Serialize)]
pub(super) struct CatalogRequest {
    #[serde(skip)]
    pub(super) origin: MetadataOrigin,
    pub(super) browse: Option<viptv_engine::home_browse::HomeBrowseAction>,
    pub(super) id: String,
    pub(super) title: String,
    pub(super) provider_id: String,
    pub(super) url: Option<String>,
}

#[derive(Clone)]
pub(super) struct PendingHome {
    pub(super) scope: Scope,
    pub(super) rows: Vec<CatalogRequest>,
    pub(super) cached: std::collections::HashMap<String, Vec<viptv_engine::presentation::Card>>,
    pub(super) cache_home: bool,
    pub(super) discover: Option<(u32, bool)>,
}

impl LocalEngine {
    fn key(&self, id: &str) -> String {
        format!("{}:{id}", self.namespace)
    }
    pub(super) fn addon(&self, id: &str) -> Result<StoredAddon, CoreError> {
        let raw = self.secrets.get(self.key(id))?.ok_or(CoreError::Bridge)?;
        serde_json::from_str(&raw).map_err(|_| CoreError::Bridge)
    }
}

mod home;
#[cfg(test)]
mod tests;

#[uniffi::export]
impl LocalEngine {
    pub fn check_addon_url(&self, url: String) -> Result<String, CoreError> {
        stremio::manifest_url(&url).map_err(|_| CoreError::InvalidInput)
    }

    pub fn install_addon(
        &self,
        profile: String,
        url: String,
        raw_manifest: String,
    ) -> Result<String, CoreError> {
        let manifest_url = stremio::manifest_url(&url).map_err(|_| CoreError::InvalidInput)?;
        let manifest =
            stremio::parse_manifest(&raw_manifest).map_err(|_| CoreError::InvalidInput)?;
        let existing = self
            .store
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .connections("local")
            .map_err(|_| CoreError::Bridge)?;
        for connection in existing {
            if self
                .addon(&connection.id)
                .is_ok_and(|stored| stored.manifest_url == manifest_url)
            {
                self.store
                    .lock()
                    .map_err(|_| CoreError::Bridge)?
                    .assign(&profile, &connection)
                    .map_err(|_| CoreError::Bridge)?;
                return serde_json::to_string(&connection).map_err(|_| CoreError::Bridge);
            }
        }
        let id = format!("addon:{}", uuid::Uuid::new_v4());
        let record = IntegrationConnection {
            id: id.clone(),
            plugin_id: "stremio".into(),
            account_id: "local".into(),
            private_profile_id: None,
            name: manifest.name,
            enabled: true,
            revision: 0,
            capabilities: manifest.capabilities,
        };
        let secret = StoredAddon {
            manifest_url,
            manifest: serde_json::from_str(&raw_manifest).map_err(|_| CoreError::InvalidInput)?,
        };
        self.secrets.put(
            self.key(&id),
            serde_json::to_string(&secret).map_err(|_| CoreError::Bridge)?,
        )?;
        if self
            .store
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .install(&profile, &record)
            .is_err()
        {
            let _ = self.secrets.remove(self.key(&id));
            return Err(CoreError::Bridge);
        }
        serde_json::to_string(&record).map_err(|_| CoreError::Bridge)
    }

    pub fn addons(&self) -> Result<String, CoreError> {
        serde_json::to_string(
            &self
                .store
                .lock()
                .map_err(|_| CoreError::Bridge)?
                .connections("local")
                .map_err(|_| CoreError::Bridge)?,
        )
        .map_err(|_| CoreError::Bridge)
    }

    pub fn set_addon_enabled(
        &self,
        profile: String,
        id: String,
        enabled: bool,
    ) -> Result<(), CoreError> {
        self.store
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .set_assignment(&profile, &id, enabled)
            .map_err(|_| CoreError::Bridge)
    }

    pub fn remove_addon(&self, id: String) -> Result<(), CoreError> {
        self.store
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .remove_connection(&id)
            .map_err(|_| CoreError::Bridge)?;
        self.secrets.remove(self.key(&id))
    }
}
