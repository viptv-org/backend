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
