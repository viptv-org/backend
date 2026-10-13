use super::*;

type Entry = tokio::sync::Mutex<Option<(Instant, Arc<Value>)>>;
pub(super) type Cache = tokio::sync::Mutex<HashMap<String, Arc<Entry>>>;

impl Service {
    pub(super) async fn metadata_document(
        &self,
        path: &str,
        check: impl FnOnce() -> Result<(), ApiError>,
    ) -> Result<Arc<Value>, ApiError> {
        let entry = {
            let mut cache = self.metadata.lock().await;
            if cache.len() >= 256 && !cache.contains_key(path) {
                // Keep active requests coalesced; idle public records can be fetched again.
                cache.retain(|_, entry| Arc::strong_count(entry) > 1);
                if cache.len() >= 256 {
                    return Err(ApiError(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Simkl metadata is busy".into(),
                    ));
                }
            }
            cache.entry(path.to_owned()).or_default().clone()
        };
        let mut cached = entry.lock().await;
        if let Some((_, value)) = cached
            .as_ref()
            .filter(|(created, _)| created.elapsed() < Duration::from_secs(3600))
        {
            return Ok(value.clone());
        }
        let value = Arc::new(self.request_checked(path, None, None, check).await?);
        *cached = Some((Instant::now(), value.clone()));
        Ok(value)
    }
}

pub(super) async fn metadata(
    app: &App,
    profile: i64,
    access: &store::Credential,
    path: &str,
) -> Result<Arc<Value>, ApiError> {
    store::validate(app, profile, access)?;
    let value = app
        .simkl
        .metadata_document(path, || store::validate(app, profile, access))
        .await;
    let value = match value {
        Err(error) if error.1 == "Simkl authorization is required" && access.token.is_some() => {
            // OAuth-backed responses never enter the shared public metadata cache.
            Arc::new(store::request(app, profile, access, path, None).await?)
        }
        result => result?,
    };
    store::validate(app, profile, access)?;
    Ok(value)
}

/// Public detail snapshots can seed a new private projection without egress.
pub(super) async fn cached_title(
    app: &App,
    category: &str,
    id: u64,
) -> Option<viptv_engine::presentation::Card> {
    let endpoint = if category == "movie" {
        "movies"
    } else {
        category
    };
    let entry = app
        .simkl
        .metadata
        .lock()
        .await
        .get(&format!("/{endpoint}/{id}?extended=full"))?
        .clone();
    let cached = entry.try_lock().ok()?;
    let (_, value) = cached
        .as_ref()
        .filter(|(created, _)| created.elapsed() < Duration::from_secs(3600))?;
    if viptv_engine::simkl::id(value) != Some(id) {
        return None;
    }
    viptv_engine::simkl::card(value, category).ok()
}
