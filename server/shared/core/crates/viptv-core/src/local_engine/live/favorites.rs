use super::*;

#[uniffi::export]
impl LocalEngine {
    pub fn live_favorites(&self, profile: String) -> Result<String, CoreError> {
        let store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        let selected = store
            .profile(&local_scope(&profile))
            .map_err(|_| CoreError::Bridge)?
            .ok_or(CoreError::InvalidInput)?;
        let saved = store
            .live_favorites(&selected.scope)
            .map_err(|_| CoreError::InvalidInput)?;
        let connections = store.connections("local").map_err(|_| CoreError::Bridge)?;
        let caches = self.live_cache.lock().map_err(|_| CoreError::Bridge)?;
        let channel_ids: Vec<_> = saved
            .into_iter()
            .filter(|id| {
                connections.iter().any(|connection| {
                    caches.get(&connection.id).is_some_and(|cache| {
                        cache.revision == connection.revision
                            && cache
                                .channels
                                .iter()
                                .any(|channel| channel.channel.id == *id)
                    })
                })
            })
            .collect();
        serde_json::to_string(&serde_json::json!({"channel_ids":channel_ids}))
            .map_err(|_| CoreError::Bridge)
    }

    pub fn set_live_favorite(
        &self,
        profile: String,
        channel: String,
        saved: bool,
    ) -> Result<String, CoreError> {
        if channel.is_empty() || channel.len() > 512 || channel.chars().any(char::is_control) {
            return Err(CoreError::InvalidInput);
        }
        #[cfg(not(feature = "live-runtime"))]
        {
            let _ = (profile, channel, saved);
            Err(CoreError::InvalidInput)
        }
        #[cfg(feature = "live-runtime")]
        {
            if saved {
                let (scope, connection, _, _) =
                    self.live_channel(&profile, &channel, deadline())?;
                self.store
                    .lock()
                    .map_err(|_| CoreError::Bridge)?
                    .set_live_favorite(&scope, &connection.id, &channel, true)
                    .map_err(|_| CoreError::InvalidInput)?;
            } else {
                // Removing a saved preference remains possible when a channel or
                // provider is temporarily absent; no other profile row is addressed.
                let (scope, known) = {
                    let store = self.store.lock().map_err(|_| CoreError::Bridge)?;
                    let selected = store
                        .profile(&local_scope(&profile))
                        .map_err(|_| CoreError::Bridge)?
                        .ok_or(CoreError::InvalidInput)?;
                    let known = store
                        .has_live_favorite(&selected.scope, &channel)
                        .map_err(|_| CoreError::InvalidInput)?;
                    (selected.scope, known)
                };
                let scope = if known {
                    scope
                } else {
                    self.live_channel(&profile, &channel, deadline())?.0
                };
                self.store
                    .lock()
                    .map_err(|_| CoreError::Bridge)?
                    .set_live_favorite(&scope, "", &channel, false)
                    .map_err(|_| CoreError::InvalidInput)?;
            }
            self.live_favorites(profile)
        }
    }
}
