use super::*;
pub(super) async fn run(
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    access: &store::Credential,
) -> Result<Status, ApiError> {
    use futures::StreamExt;
    let (entries, _, _, _) = watchlists::load(app, profile, lease).await?;
    let owner = app
        .simkl
        .watchlists
        .lock()
        .await
        .get(&key(app, access))
        .cloned()
        .ok_or_else(|| ApiError::from("Import snapshot unavailable"))?;
    let items = owner
        .lock()
        .await
        .as_ref()
        .map(|s| s.items.clone())
        .ok_or_else(|| ApiError::from("Import snapshot unavailable"))?;
    store::validate(app, profile, access)?;
    let mut status = Status::default();
    {
        let mut db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        schema::validate_db(&db, app, profile, access, lease)?;
        let tx = db.transaction().map_err(db_error)?;
        status.added = membership::apply(
            &tx,
            profile,
            access.revision.unwrap_or(0),
            &entries,
            util::now_millis().max(0) as u64,
        )?;
        let counts =
            history::original(&tx, profile, access.revision.unwrap_or(0), &items, &entries)?;
        status.history = counts.inserted;
        status.preserved = counts.skipped;
        status.unresolved = counts.unresolved;
        tx.commit().map_err(db_error)?;
    }
    let playback = match store::private_read(
        app,
        profile,
        access,
        "/sync/playback?limit=10000&hide_watched=true",
    )
    .await
    {
        Ok(playback) => playback,
        Err(_) => {
            status.error = true;
            return Ok(status);
        }
    };
    let playback = playback
        .as_array()
        .filter(|p| p.len() <= 10000)
        .ok_or_else(|| ApiError::from("Invalid paused playback"))?;
    // Bound the active metadata work; unresolved entries remain explicit, never guessed.
    status.unresolved += playback.len().saturating_sub(100);
    let mut reads = futures::stream::iter(playback.iter().take(100).cloned().collect::<Vec<_>>())
        .map(|value| {
            let entries = &entries;
            async move { history::pause(app, profile, access, &value, entries).await }
        })
        .buffer_unordered(3);
    while let Some(pause) = reads.next().await {
        let Some(pause) = pause else {
            status.unresolved += 1;
            continue;
        };
        store::validate(app, profile, access)?;
        let mut db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        schema::validate_db(&db, app, profile, access, lease)?;
        let tx = db.transaction().map_err(db_error)?;
        let mut statistics =
            viptv_engine::library::LibraryStatistics::empty(pause.library_card.identity.clone());
        statistics.last_watched_millis = Some(pause.updated);
        // Already imported watchlist facts remain authoritative; a pause is not a new status/rating.
        let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM simkl_import_entries WHERE profile_id=?1 AND remote_kind=?2 AND remote_id=?3)",params![profile,pause.library_card.identity.kind,pause.library_card.identity.id],|r|r.get(0)).map_err(db_error)?;
        if !exists {
            status.added += membership::apply(
                &tx,
                profile,
                access.revision.unwrap_or(0),
                &[viptv_engine::library::Entry {
                    card: pause.library_card.clone(),
                    statistics,
                }],
                util::now_millis().max(0) as u64,
            )?;
        }
        if progress::apply(
            &tx,
            profile,
            &pause.card,
            pause.parent.as_ref(),
            pause.position,
            pause.duration,
            pause.updated,
            false,
            access.revision.unwrap_or(0),
        )? {
            status.history += 1;
        } else {
            status.preserved += 1;
        }
        tx.commit().map_err(db_error)?;
    }
    status.unresolved += next::batch(app, lease, profile, access, &entries).await;
    Ok(status)
}
