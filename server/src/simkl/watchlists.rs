use super::*;
use viptv_engine::library::{Entry, LibraryStatistics};
pub(super) type Cache =
    tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<Option<sync::Snapshot>>>>>;
pub(super) mod enrichment;
pub(in crate::simkl) mod projection;
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
    if cache.is_none() {
        let db=app.db.lock().map_err(|_|ApiError::from("Database unavailable"))?;
        let saved:Option<(String,String)>=db.query_row("SELECT activities,items FROM simkl_import_snapshots WHERE profile_id=?1 AND account_id=?2 AND link_revision=?3 AND configuration_revision=?4",params![profile,app.identity().account_id(),access.revision,access.configuration_revision],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(db_error)?;
        if let Some((activities,items))=saved { if let (Ok(activities),Ok(items))=(serde_json::from_str(&activities),serde_json::from_str(&items)) {
            *cache=Some(sync::Snapshot{checked:Instant::now().checked_sub(Duration::from_secs(61)).unwrap_or_else(Instant::now),activities,items:Arc::new(items),metadata:Default::default()});
        }}
    }
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
        {
            store::validate(app,profile,&access)?;
            let db=app.db.lock().map_err(|_|ApiError::from("Database unavailable"))?;
            imports::schema::validate_db(&db,app,profile,&access,lease)?;
            db.execute("INSERT INTO simkl_import_snapshots VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(profile_id) DO UPDATE SET account_id=excluded.account_id,link_revision=excluded.link_revision,configuration_revision=excluded.configuration_revision,activities=excluded.activities,items=excluded.items",params![profile,app.identity().account_id(),access.revision,access.configuration_revision,snapshot.activities.to_string(),snapshot.items.to_string()]).map_err(db_error)?;
        }
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

pub(crate) async fn cached(app:&App,profile:i64,lease:&ResourceLease)->Result<Option<(Vec<Entry>,usize,usize,bool)>,ApiError>{
    let access=store::credential(app,profile,Capability::Authorization)?;
    let key=format!("{}:{}:{:?}",app.identity().key(),access.configuration_revision,access.revision);
    let Some(owner)=app.simkl.watchlists.lock().await.get(&key).cloned() else{return Ok(None)};
    let (items,state)={let Ok(cache)=owner.try_lock() else{return Ok(None)};let Some(snapshot)=cache.as_ref() else{return Ok(None)};(snapshot.items.clone(),snapshot.metadata.clone())};
    let mut entries=projection::entries(&items)?;
    let allowed=metadata::allowed(app,entries.iter().map(|e|e.card.clone()).collect())?;
    let ids=allowed.iter().map(|c|(c.identity.kind.clone(),c.identity.id.clone())).collect::<std::collections::HashSet<_>>();
    entries.retain(|e|ids.contains(&(e.card.identity.kind.clone(),e.card.identity.id.clone())));
    let (known,total,running)=enrichment::apply(app,profile,&access,lease,owner,state,&mut entries).await;
    store::validate(app,profile,&access)?;Ok(Some((entries,known,total,running)))
}
