use super::*;
impl LocalEngine {
    pub(super) fn live_connections(
        &self,
        profile: &str,
    ) -> Result<(Scope, Vec<IntegrationConnection>), CoreError> {
        let store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        let selected = store
            .profile(&local_scope(profile))
            .map_err(|_| CoreError::Bridge)?
            .ok_or(CoreError::InvalidInput)?;
        let connections = store
            .connections("local")
            .map_err(|_| CoreError::Bridge)?
            .into_iter()
            .filter(|c| {
                c.enabled
                    && c.private_profile_id.as_deref() == Some(profile)
                    && selected
                        .assignments
                        .iter()
                        .any(|a| a.connection_id == c.id && a.enabled.contains(&Capability::LiveTv))
            })
            .collect();
        Ok((selected.scope, connections))
    }
}
