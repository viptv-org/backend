use super::*;
#[uniffi::export]
impl LocalEngine {
    pub fn live_guide(&self, profile: String, channel_id: String) -> Result<String, CoreError> {
        #[cfg(not(feature = "live-runtime"))]
        {
            let _ = profile;
            return serde_json::to_string(&LiveGuide {
                channel_id,
                programs: vec![],
                availability: Availability::Unsupported,
                reason: Some("Live runtime unavailable".into()),
            })
            .map_err(|_| CoreError::Bridge);
        }
        #[cfg(feature = "live-runtime")]
        {
            match self.load_live_guide(&profile, &channel_id) {
                Ok(guide) => serde_json::to_string(&guide).map_err(|_| CoreError::Bridge),
                Err(CoreError::Bridge) => serde_json::to_string(&LiveGuide {
                    channel_id,
                    programs: vec![],
                    availability: Availability::TemporarilyUnavailable,
                    reason: Some("Guide temporarily unavailable".into()),
                })
                .map_err(|_| CoreError::Bridge),
                Err(error) => Err(error),
            }
        }
    }
}

#[cfg(feature = "live-runtime")]
impl LocalEngine {
    fn load_live_guide(&self, profile: &str, channel_id: &str) -> Result<LiveGuide, CoreError> {
        let deadline = deadline();
        let (scope, connection, channel, cache) =
            self.live_channel(profile, channel_id, deadline)?;
        if let Some((expiry, guide)) = cache
            .guides
            .get(channel_id)
            .filter(|(expiry, _)| std::time::Instant::now() < *expiry)
        {
            let _ = expiry;
            self.live_validate(&scope)?;
            return Ok(guide.clone());
        }
        let gate = self.live_fetch_gate(&connection.id)?;
        let _lock = super::http::lock_until(&gate, deadline)?;
        self.live_validate(&scope)?;
        let value = self
            .secrets
            .get(self.source_key(&profile, &connection.id))?
            .ok_or(CoreError::Bridge)?;
        if let Some((expiry, guide)) = self
            .live_cache
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .get(&connection.id)
            .and_then(|cache| cache.guides.get(channel_id))
            .filter(|(expiry, _)| std::time::Instant::now() < *expiry)
        {
            let _ = expiry;
            self.live_validate(&scope)?;
            return Ok(guide.clone());
        }
        let mut guide = if connection.plugin_id == "m3u" {
            let config: M3uConfiguration =
                serde_json::from_str(&value).map_err(|_| CoreError::Bridge)?;
            if let Some((url, epg)) = config.epg_url.filter(|s| !s.is_empty()).zip(channel.epg_id) {
                let raw = self
                    .live_cache
                    .lock()
                    .map_err(|_| CoreError::Bridge)?
                    .get(&connection.id)
                    .filter(|cache| cache.revision == connection.revision)
                    .and_then(|cache| cache.xmltv.as_ref())
                    .filter(|(expiry, _)| std::time::Instant::now() < *expiry)
                    .map(|(_, body)| body.clone());
                let body = if let Some(body) = raw {
                    body
                } else {
                    let response = self.live_http(&scope, &url, 16 * 1024 * 1024, deadline)?;
                    let body: Arc<str> = response.1.into();
                    self.live_store_xmltv(&connection, body.clone())?;
                    body
                };
                LiveGuide {
                    channel_id: channel_id.to_owned(),
                    programs: parse_xmltv(&body, &epg, &channel_id)
                        .map_err(|_| CoreError::Bridge)?,
                    availability: Availability::Available,
                    reason: None,
                }
            } else {
                LiveGuide {
                    channel_id: channel_id.to_owned(),
                    programs: vec![],
                    availability: Availability::Unsupported,
                    reason: Some("No XMLTV guide configured for this channel".into()),
                }
            }
        } else {
            let config: XtreamConfiguration =
                serde_json::from_str(&value).map_err(|_| CoreError::Bridge)?;
            let uri = xtream_request(&config, "get_short_epg", channel.stream_id.as_deref())
                .map_err(|_| CoreError::Bridge)?;
            let response = self.live_http(&scope, uri.as_str(), 3 * 1024 * 1024, deadline)?;
            LiveGuide {
                channel_id: channel_id.to_owned(),
                programs: parse_xtream_guide(
                    &serde_json::from_str(&response.1).map_err(|_| CoreError::Bridge)?,
                    &channel_id,
                ),
                availability: Availability::Available,
                reason: None,
            }
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        guide.programs.retain(|p| {
            p.end_millis > now.saturating_sub(24 * 60 * 60 * 1000)
                && p.start_millis < now + 7 * 24 * 60 * 60 * 1000
        });
        guide.programs.truncate(100);
        self.live_validate(&scope)?;
        if let Some(cache) = self
            .live_cache
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .get_mut(&connection.id)
        {
            cache.guides.insert(
                channel_id.to_owned(),
                (
                    std::time::Instant::now() + std::time::Duration::from_secs(60),
                    guide.clone(),
                ),
            );
        }
        Ok(guide)
    }
}
