use super::*;
impl LocalEngine {
    pub(super) fn live_store_xmltv(
        &self,
        connection: &IntegrationConnection,
        body: Arc<str>,
    ) -> Result<(), CoreError> {
        let mut caches = self.live_cache.lock().map_err(|_| CoreError::Bridge)?;
        for cache in caches.values_mut() {
            if cache
                .xmltv
                .as_ref()
                .is_some_and(|(expiry, _)| std::time::Instant::now() >= *expiry)
            {
                cache.xmltv = None;
            }
        }
        if let Some(cache) = caches.get_mut(&connection.id) {
            cache.xmltv = None;
        }
        let budget = 32 * 1024 * 1024;
        while caches
            .values()
            .filter_map(|cache| cache.xmltv.as_ref())
            .map(|(_, doc)| doc.len())
            .sum::<usize>()
            .saturating_add(body.len())
            > budget
        {
            let oldest = caches
                .iter()
                .filter_map(|(id, cache)| {
                    cache
                        .xmltv
                        .as_ref()
                        .map(|(expiry, _)| (id.clone(), *expiry))
                })
                .min_by_key(|(_, expiry)| *expiry)
                .map(|(id, _)| id);
            let Some(oldest) = oldest else {
                return Err(CoreError::Bridge);
            };
            caches.get_mut(&oldest).unwrap().xmltv = None;
        }
        if let Some(cache) = caches
            .get_mut(&connection.id)
            .filter(|cache| cache.revision == connection.revision)
        {
            cache.xmltv = Some((
                std::time::Instant::now() + std::time::Duration::from_secs(60),
                body,
            ));
        }
        Ok(())
    }
}
