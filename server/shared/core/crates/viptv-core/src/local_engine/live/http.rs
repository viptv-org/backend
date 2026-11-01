use super::*;
impl LocalEngine {
    pub(super) fn live_http(
        &self,
        scope: &Scope,
        url: &str,
        limit: usize,
        deadline: std::time::Instant,
    ) -> Result<(String, String), CoreError> {
        self.live_validate(scope)?;
        #[cfg(test)]
        {
            let fixture = self
                .live_fixture
                .lock()
                .map_err(|_| CoreError::Bridge)?
                .clone();
            if let Some(fixture) = fixture {
                let value = fixture(scope, url)?;
                self.live_validate(scope)?;
                return Ok(value);
            }
        }
        let remaining = deadline
            .checked_duration_since(std::time::Instant::now())
            .ok_or(CoreError::Bridge)?;
        let store = self.store.clone();
        let expected = scope.clone();
        let guard: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
            store
                .lock()
                .ok()
                .and_then(|s| s.profile(&expected).ok().flatten())
                .is_some_and(|p| p.scope.revision == expected.revision)
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| CoreError::Bridge)?;
        let uri = url::Url::parse(url).map_err(|_| CoreError::InvalidInput)?;
        runtime.block_on(async {
            let mut request = viptv_engine::egress::HttpRequest::get(uri);
            request.timeout = Some(remaining);
            request.max_redirects = Some(4);
            let response = tokio::time::timeout(
                remaining,
                viptv_engine::egress::ScopedHttp::new(guard).fetch(request, limit, false),
            )
            .await
            .map_err(|_| CoreError::Bridge)?
            .map_err(|_| CoreError::Bridge)?;
            if !(200..300).contains(&response.status) {
                return Err(CoreError::Bridge);
            }
            Ok((
                response.url.to_string(),
                if limit > 3 * 1024 * 1024 {
                    decode_xmltv_body(&response.body).map_err(|_| CoreError::Bridge)?
                } else {
                    String::from_utf8_lossy(&response.body).into_owned()
                },
            ))
        })
    }

    pub(super) fn live_validate(&self, scope: &Scope) -> Result<(), CoreError> {
        let store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        if store
            .profile(scope)
            .map_err(|_| CoreError::Bridge)?
            .is_some_and(|profile| profile.scope == *scope)
        {
            Ok(())
        } else {
            Err(CoreError::InvalidInput)
        }
    }
    pub(super) fn live_fetch_gate(&self, id: &str) -> Result<Arc<Mutex<()>>, CoreError> {
        let mut locks = self
            .live_fetch_locks
            .lock()
            .map_err(|_| CoreError::Bridge)?;
        locks.retain(|_, lock| Arc::strong_count(lock) > 1);
        if locks.len() >= 64 && !locks.contains_key(id) {
            return Err(CoreError::Bridge);
        }
        Ok(locks.entry(id.into()).or_default().clone())
    }
}

pub(super) fn lock_until(
    lock: &Mutex<()>,
    deadline: std::time::Instant,
) -> Result<std::sync::MutexGuard<'_, ()>, CoreError> {
    loop {
        match lock.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(std::sync::TryLockError::Poisoned(_)) => return Err(CoreError::Bridge),
            Err(std::sync::TryLockError::WouldBlock) => {
                if std::time::Instant::now() >= deadline {
                    return Err(CoreError::Bridge);
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }
}
