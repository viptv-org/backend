//! Read-only linked-profile watchlists; no memos, PRO claims, state writes or public-cache sharing.
use super::*;
use viptv_engine::library::{Entry, LibraryStatistics};
pub(super) type Cache =
    tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<Option<sync::Snapshot>>>>>;
pub(super) mod enrichment;
mod projection;
pub(super) mod sync;
pub(crate) async fn load(
    app: &App,
    profile: i64,
    lease: &ResourceLease,
) -> Result<(Vec<Entry>, usize, usize, bool), ApiError> {
    let access = store::credential(app, profile, Capability::Authorization)?;
    if access.token.is_none() {
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "Link Simkl for this profile".into(),
        ));
    }
    let key = format!(
        "{}:{}:{:?}",
        app.identity().key(),
        access.configuration_revision,
        access.revision
    );
    let entry = {
        let mut cache = app.simkl.watchlists.lock().await;
        if cache.len() >= 16 && !cache.contains_key(&key) {
            cache.retain(|_, value| Arc::strong_count(value) > 1);
            if cache.len() >= 16 {
                return Err(ApiError(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Watchlists are busy".into(),
                ));
            }
        }
        cache.entry(key).or_default().clone()
    };
    let mut cache = entry.lock().await;
    store::validate(app, profile, &access)?;
    let value = if let Some(snapshot) = cache
        .as_ref()
        .filter(|snapshot| snapshot.checked.elapsed() < Duration::from_secs(60))
    {
        snapshot.items.clone()
    } else {
        let snapshot = tokio::time::timeout(
            Duration::from_secs(12),
            sync::read(app, profile, &access, cache.as_ref()),
        )
        .await
        .map_err(|_| {
            ApiError(
                StatusCode::GATEWAY_TIMEOUT,
                "Watchlist read exceeded its deadline".into(),
            )
        })??;
        let value = snapshot.items.clone();
        *cache = Some(snapshot);
        value
    };
    let metadata = cache
        .as_ref()
        .ok_or_else(|| ApiError::from("Watchlist cache unavailable"))?
        .metadata
        .clone();
    drop(cache);
    let mut entries = projection::entries(&value)?;
    store::validate(app, profile, &access)?;
    let allowed = metadata::allowed(app, entries.iter().map(|e| e.card.clone()).collect())?;
    let ids = allowed
        .iter()
        .map(|c| (c.identity.kind.clone(), c.identity.id.clone()))
        .collect::<std::collections::HashSet<_>>();
    entries.retain(|e| ids.contains(&(e.card.identity.kind.clone(), e.card.identity.id.clone())));
    let (known, total, running) =
        enrichment::apply(app, profile, &access, lease, entry, metadata, &mut entries).await;
    store::validate(app, profile, &access)?;
    Ok((entries, known, total, running))
}
