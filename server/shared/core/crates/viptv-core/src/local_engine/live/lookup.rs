use super::*;
impl LocalEngine {
    pub(super) fn live_channel(
        &self,
        profile: &str,
        id: &str,
        deadline: std::time::Instant,
    ) -> Result<(Scope, IntegrationConnection, PrivateChannel, Cache), CoreError> {
        let (scope, connections) = self.live_connections(profile)?;
        let owner = {
            let cache = self.live_cache.lock().map_err(|_| CoreError::Bridge)?;
            connections
                .iter()
                .find(|connection| {
                    cache.get(&connection.id).is_some_and(|entry| {
                        entry
                            .channels
                            .iter()
                            .any(|channel| channel.channel.id == id)
                    })
                })
                .cloned()
        };
        if let Some(connection) = owner {
            let cache = self.live_directory(&scope, &connection, deadline)?;
            let channel = cache
                .channels
                .iter()
                .find(|c| c.channel.id == id)
                .cloned()
                .ok_or(CoreError::InvalidInput)?;
            return Ok((scope, connection, channel, cache));
        }
        Err(CoreError::InvalidInput)
    }
}
