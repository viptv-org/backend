use super::*;
impl LocalEngine {
    pub(super) fn live_directory(
        &self,
        scope: &Scope,
        connection: &IntegrationConnection,
        deadline: std::time::Instant,
    ) -> Result<Cache, CoreError> {
        let gate = self.live_fetch_gate(&connection.id)?;
        let _lock = super::http::lock_until(&gate, deadline)?;
        self.live_validate(scope)?;
        if let Some(cache) = self
            .live_cache
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .get(&connection.id)
            .filter(|c| c.revision == connection.revision && std::time::Instant::now() < c.expires)
            .cloned()
        {
            return Ok(cache);
        }
        let raw = self
            .secrets
            .get(self.source_key(&scope.profile_id, &connection.id))?
            .ok_or(CoreError::Bridge)?;
        let mut groups = Vec::new();
        let channels = if connection.plugin_id == "m3u" {
            let config: M3uConfiguration =
                serde_json::from_str(&raw).map_err(|_| CoreError::Bridge)?;
            let (url, body) =
                self.live_http(scope, &config.playlist_url, 3 * 1024 * 1024, deadline)?;
            let channels = parse_m3u(
                &body,
                &url::Url::parse(&url).map_err(|_| CoreError::Bridge)?,
                &connection.id,
            )
            .map_err(|_| CoreError::Bridge)?;
            let mut seen = std::collections::HashSet::new();
            for channel in &channels {
                for group in &channel.groups {
                    if seen.insert(group.id.clone()) {
                        groups.push(group.clone());
                    }
                }
            }
            channels
        } else {
            let config: XtreamConfiguration =
                serde_json::from_str(&raw).map_err(|_| CoreError::Bridge)?;
            let (_, body) = self.live_http(
                scope,
                xtream_request(&config, "get_live_streams", None)
                    .map_err(|_| CoreError::Bridge)?
                    .as_str(),
                3 * 1024 * 1024,
                deadline,
            )?;
            let mut channels =
                parse_xtream(&body, &connection.id, &config).map_err(|_| CoreError::Bridge)?;
            let (_, body) = self.live_http(
                scope,
                xtream_request(&config, "get_live_categories", None)
                    .map_err(|_| CoreError::Bridge)?
                    .as_str(),
                3 * 1024 * 1024,
                deadline,
            )?;
            groups = xtream_groups(&body).map_err(|_| CoreError::Bridge)?;
            for channel in &mut channels {
                if let Some(group) = groups
                    .iter()
                    .find(|g| channel.channel.group_id.as_ref() == Some(&g.id))
                {
                    channel.channel.group_name = Some(group.label.clone());
                }
            }
            channels
        };
        let store = self.store.lock().map_err(|_|CoreError::Bridge)?;
        if !store.profile(scope).map_err(|_|CoreError::Bridge)?.is_some_and(|profile|profile.scope==*scope) {return Err(CoreError::InvalidInput);}
        let cache = Cache {
            revision: connection.revision,
            expires: std::time::Instant::now() + std::time::Duration::from_secs(300),
            channels,
            groups,
            guides: Default::default(),
            xmltv: None,
        };
        self.live_cache
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .insert(connection.id.clone(), cache.clone());
        Ok(cache)
    }
}
