use super::*;
use serde::Serialize;

#[derive(Serialize)]
struct ProfileCard {
    id: String,
    name: String,
    locked: bool,
    avatar_id: Option<String>,
    avatar_style: String,
    avatar_choice: Option<u16>,
}

#[uniffi::export]
impl LocalEngine {
    pub fn profile_cards(&self) -> Result<String, CoreError> {
        let store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        let cards = store
            .profiles("local", "local")
            .map_err(|_| CoreError::Bridge)?
            .into_iter()
            .map(|profile| {
                let avatar_id = profile.shown_avatar_id().map(Into::into);
                let avatar_style = profile.avatar_style().into();
                Ok(ProfileCard {
                    locked: store
                        .has_profile_pin(&profile.scope)
                        .map_err(|_| CoreError::Bridge)?,
                    id: profile.scope.profile_id,
                    name: profile.name,
                    avatar_id,
                    avatar_style,
                    avatar_choice: profile.avatar_choice,
                })
            })
            .collect::<Result<Vec<_>, CoreError>>()?;
        serde_json::to_string(&cards).map_err(|_| CoreError::Bridge)
    }

    pub fn set_profile_avatar(
        &self,
        id: String,
        avatar_id: String,
        pin: String,
    ) -> Result<(), CoreError> {
        self.store
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .set_profile_avatar(&local_scope(&id), &avatar_id, &pin)
            .map_err(|_| CoreError::InvalidInput)
    }

    pub fn set_profile_avatar_choice(
        &self,
        id: String,
        avatar_style: String,
        avatar_choice: u16,
        pin: String,
    ) -> Result<(), CoreError> {
        self.store
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .set_profile_avatar_choice(&local_scope(&id), &avatar_style, avatar_choice, &pin)
            .map_err(|_| CoreError::InvalidInput)
    }

    pub fn rename_profile(&self, id: String, name: String, pin: String) -> Result<(), CoreError> {
        self.store
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .rename_profile(&local_scope(&id), &name, &pin)
            .map_err(|_| CoreError::InvalidInput)
    }

    pub fn delete_profile(&self, id: String, pin: String) -> Result<(), CoreError> {
        // Keep storage before each job/cache lock until in-memory teardown has
        // finished, so a new incarnation cannot be created during cleanup.
        let mut store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        let private = store
            .delete_profile_with_private_connections(&local_scope(&id), &pin)
            .map_err(|_| CoreError::InvalidInput)?;
        self.forget_integration_profile(&id)?;
        self.forget_source_profile(&id)?;
        self.forget_subtitle_profile(&id)?;
        self.forget_metadata_profile(&id)?;
        self.pending
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .retain(|_, pending| pending.scope.profile_id != id);
        let ids: std::collections::HashSet<_> = private
            .iter()
            .map(|connection| connection.id.as_str())
            .collect();
        self.live_cache
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .retain(|key, _| !ids.contains(key.as_str()));
        self.live_fetch_locks
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .retain(|key, _| !ids.contains(key.as_str()));
        for connection in private {
            // Database deletion is committed. A vault outage must not present an
            // already deleted profile as a failed operation or restore its access.
            let key = if connection.plugin_id == "stremio" {
                format!("{}:{}", self.namespace, connection.id)
            } else {
                self.source_key(&id, &connection.id)
            };
            let _ = self.secrets.remove(key);
        }
        Ok(())
    }

    pub fn set_profile_pin(
        &self,
        id: String,
        old_pin: String,
        new_pin: String,
    ) -> Result<(), CoreError> {
        self.store
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .set_profile_pin(&local_scope(&id), &old_pin, &new_pin)
            .map_err(|_| CoreError::InvalidInput)
    }

    pub fn verify_profile_pin(&self, id: String, pin: String) -> Result<bool, CoreError> {
        self.store
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .verify_profile_pin(&local_scope(&id), &pin)
            .map_err(|_| CoreError::Bridge)
    }
}

#[cfg(test)]
#[path = "profiles/tests.rs"]
mod tests;
