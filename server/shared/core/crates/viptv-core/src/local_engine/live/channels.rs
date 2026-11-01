use super::*;
#[uniffi::export]
impl LocalEngine {
    pub fn live_channels(&self, profile: String, query_json: String) -> Result<String, CoreError> {
        let query: LiveQuery =
            serde_json::from_str(&query_json).map_err(|_| CoreError::InvalidInput)?;
        query.validate().map_err(|_| CoreError::InvalidInput)?;
        #[cfg(not(feature = "live-runtime"))]
        {
            let _ = profile;
            return serde_json::to_string(&LivePage {
                channels: vec![],
                groups: vec![],
                connections: vec![],
                next_cursor: None,
                complete: true,
                availability: Availability::Unsupported,
                reason: Some("Live runtime unavailable".into()),
            })
            .map_err(|_| CoreError::Bridge);
        }
        #[cfg(feature = "live-runtime")]
        {
            let (scope, connections) = self.live_connections(&profile)?;
            let selected = connections
                .iter()
                .find(|c| query.connection_id.as_ref().map_or(true, |id| id == &c.id));
            let Some(connection) = selected else {
                return serde_json::to_string(&LivePage {
                    channels: vec![],
                    groups: vec![],
                    connections,
                    next_cursor: None,
                    complete: true,
                    availability: Availability::NeedsConfiguration,
                    reason: Some("Assign a live TV connection".into()),
                })
                .map_err(|_| CoreError::Bridge);
            };
            let cache = match self.live_directory(&scope, connection, deadline()) {
                Ok(cache) => cache,
                Err(CoreError::Bridge) => {
                    self.live_validate(&scope)?;
                    return serde_json::to_string(&LivePage {
                        channels: vec![],
                        groups: vec![],
                        connections,
                        next_cursor: None,
                        complete: true,
                        availability: Availability::TemporarilyUnavailable,
                        reason: Some("Live TV temporarily unavailable".into()),
                    })
                    .map_err(|_| CoreError::Bridge);
                }
                Err(error) => return Err(error),
            };
            let filtered = cache
                .channels
                .iter()
                .filter(|c| {
                    query
                        .group_id
                        .as_ref()
                        .is_none_or(|g| c.channel.in_group(g))
                        && query.search.as_ref().is_none_or(|search| {
                            c.channel
                                .card
                                .title
                                .to_lowercase()
                                .contains(&search.to_lowercase())
                        })
                })
                .collect::<Vec<_>>();
            let limit = query.limit.unwrap_or(100) as usize;
            let start = (query.offset as usize).min(filtered.len());
            let end = start.saturating_add(limit).min(filtered.len());
            let page = LivePage {
                channels: filtered
                    .iter()
                    .skip(start)
                    .take(limit)
                    .map(|c| c.channel.clone())
                    .collect(),
                groups: cache.groups,
                connections,
                next_cursor: (end < filtered.len()).then(|| end.to_string()),
                complete: end >= filtered.len(),
                availability: Availability::Available,
                reason: None,
            };
            self.live_validate(&scope)?;
            serde_json::to_string(&page).map_err(|_| CoreError::Bridge)
        }
    }
}
