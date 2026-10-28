use super::*;
pub(crate) fn snapshot(
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    offset: u32,
    limit: u32,
) -> Result<ContinuationPage, ApiError> {
    if offset > 10000 || !(1..=100).contains(&limit) {
        return Err("Invalid continuation page".into());
    }
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_profile(&db, profile)?;
    lease.validate(&db)?;
    let configuration_revision = configuration_revision(&db, profile)?;
    let restricted = kids::restricted(&db, &lease.principal)?;
    let queue = crate::continuation::queue_items(&db, profile, 0, 50000, true)?;
    let now = util::now_millis().max(0) as u64;
    let items = kids::filter_items(
        &db,
        &lease.principal,
        queue["items"].as_array().cloned().unwrap_or_default(),
    )?;
    // Each entry with the time that orders it: its latest activity, or the
    // release that made its series new (see `releases`).
    let mut entries = Vec::new();
    for item in items {
        let Some(rendered) = super::super::history::record(&item) else {
            continue;
        };
        let hidden_at = item["queue_hidden_at"]
            .as_u64()
            .map(|seconds| seconds.saturating_mul(1000));
        let previous = item
            .get("previous_episode")
            .and_then(super::super::history::record);
        let status = state(&item, &rendered);
        let mut card = rendered.card();
        let record = if status == ContinuationStatus::Next {
            let Some(previous) = previous.as_ref() else {
                continue;
            };
            previous.clone()
        } else {
            rendered
        };
        if status == ContinuationStatus::Next {
            if let Ok(verified) =
                serde_json::from_value::<Card>(item["engine_presentation"].clone())
            {
                if verified.identity == card.identity
                    && verified.parent_identity == card.parent_identity
                {
                    card = verified;
                }
            }
            card.progress = None;
        } else if status != ContinuationStatus::Resume {
            card.progress = None;
            card.subtitle = None;
        }
        let mut status = status;
        let mut previous_identity = previous.map(|record| record.identity);
        let mut order = record.updated_at_millis;
        let found = match (&record.parent_identity, &status) {
            (Some(series), status) if *status != ContinuationStatus::Resume => {
                super::releases::up_next_db(&db, app, lease, profile, &series.id, &record, now)?
                    .filter(|found| {
                        kids::require_item(&db, &lease.principal, &found.card.identity.kind, &found.card.identity.id)
                            .is_ok()
                    })
            }
            _ => None,
        };
        // A removed title stays removed, through replays too, until an episode
        // is released after its removal; it then returns at that release (and
        // the next Home load un-hides it, see `releases::schedule`).
        if let Some(hidden) = hidden_at {
            match found
                .as_ref()
                .and_then(|found| found.released_at_millis.iter().find(|at| **at > hidden))
            {
                Some(at) => order = *at,
                None => continue,
            }
        } else if let Some(found) = &found {
            // Only a release that came out after the finish is new: it earns the
            // badge and the move up. A backlog episode the profile has not
            // reached yet is up next at the finish's own place in the queue.
            if let Some(at) = found.new_release_at_millis {
                order = order.max(at);
            }
        }
        if let Some(found) = &found {
            let verified = (status == ContinuationStatus::Next).then(|| card.clone());
            card = super::releases::present(found, &record, verified);
            previous_identity = Some(record.identity.clone());
            status = ContinuationStatus::Next;
        } else if status == ContinuationStatus::Next {
            // The stored document cannot project this prepared target; the card
            // `continue/next` stored still names the episode it opened, so the
            // entry keeps its caption (backend#63).
            if let Ok(prepared) =
                serde_json::from_value::<Card>(item["engine_presentation"].clone())
            {
                if prepared.identity.id == card.identity.id
                    && prepared.identity.episode == card.identity.episode
                {
                    card.subtitle =
                        super::releases::caption(&card.identity, &prepared.title, &record.title);
                }
            }
        }
        super::art::fill(&db, lease, profile, restricted, &mut card)?;
        crate::simkl::imports::enrich(&db, profile, &mut card)?;
        super::super::source_integrations::merge_cached_with_db(
            app,
            lease,
            configuration_revision,
            &db,
            std::slice::from_mut(&mut card),
        );
        entries.push((
            order,
            ContinuationEntry {
                card,
                record,
                status,
                previous_identity,
            },
        ));
    }
    // Progress can hold several rows for one title+episode from one provider
    // (different positions or source ids). Emit one card per provider's
    // series+episode, and one per provider's movie, keeping the most recently
    // updated record (and its order). Scoped-library entries merge below and
    // stay separate by scope.
    let mut best: std::collections::HashMap<String, (u64, ContinuationEntry)> =
        std::collections::HashMap::new();
    for (order, entry) in std::mem::take(&mut entries) {
        let key = match &entry.card.parent_identity {
            Some(series) => {
                let episode = entry.card.identity.episode.as_ref();
                format!(
                    "series:{}:{}:{}:{}",
                    entry.card.provider_id,
                    series.id,
                    episode.and_then(|e| e.season).map(|s| s.to_string()).unwrap_or_default(),
                    episode.map(|e| e.episode).unwrap_or(0)
                )
            }
            None => format!("title:{}:{}", entry.card.provider_id, entry.card.identity.id),
        };
        match best.get(&key) {
            Some((_, old)) if old.record.updated_at_millis > entry.record.updated_at_millis => {}
            _ => {
                best.insert(key, (order, entry));
            }
        }
    }
    entries = best.into_values().collect();
    if queue["next_offset"].is_number() {
        return Err("History exceeds its bounded item limit".into());
    }
    for (key, entry) in super::super::scoped_library::continuation_db(&db, app, profile)? {
        entries.retain(|(_, old)| {
            let mut card = old.record.card();
            if let Some(parent) = &old.record.parent_identity {
                card.identity = parent.clone();
                card.parent_identity = None;
            }
            super::super::scoped_library::raw_key(&card).is_ok_and(|old| old != key)
        });
        entries.push((entry.record.updated_at_millis, entry));
    }
    for (_, entry) in &mut entries {
        mark_finished(entry);
    }
    entries.sort_by(|(a_order, a), (b_order, b)| {
        b_order
            .cmp(a_order)
            .then_with(|| a.card.identity.id.cmp(&b.card.identity.id))
    });
    let total = entries.len();
    let next_cursor =
        (offset as usize + (limit as usize) < total).then(|| (offset + limit).to_string());
    let revision =
        configuration_revision.saturating_add(super::super::history::revision(&db, profile)?);
    let mut entries = entries
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .map(|(_, entry)| entry)
        .collect::<Vec<_>>();
    // Attribution reads the add-on store, which decrypts manifests through the
    // vault and the database, so release the queue read first.
    drop(db);
    let serving = super::super::origins::Serving::read(app, lease, profile)?;
    {
        let db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        for entry in &mut entries {
            attribute(&db, &serving, &mut entry.card)?;
        }
    }
    Ok(ContinuationPage {
        scope: super::super::scope(lease, profile, configuration_revision),
        revision,
        entries,
        complete: next_cursor.is_none(),
        next_cursor,
    })
}
/// A row the Stremio import wrote carries no provider provenance, so the app
/// had to load Details before it could start finding sources. Name the scope
/// that serves the row, so the tap starts discovery at once (backend#72). A row
/// nothing serves keeps its shape and the app falls back as before.
fn attribute(
    db: &Connection,
    serving: &super::super::origins::Serving,
    card: &mut Card,
) -> Result<(), ApiError> {
    if card.metadata_origin.is_some() {
        return Ok(());
    }
    let lookup = card.parent_identity.as_ref().unwrap_or(&card.identity);
    let Some(origin) = serving.origin(db, lookup)? else {
        return Ok(());
    };
    card.provider_id = origin
        .connection_id
        .clone()
        .unwrap_or_else(|| origin.provider_id.clone());
    card.metadata_origin = Some(origin);
    Ok(())
}

/// A series whose newest finished episode has no released next one (caught
/// up, upcoming, or not resolved yet) keeps that episode on its card. Without
/// a progress bar it would read as an episode still to watch, so it carries
/// the Watched check. In-progress titles (a movie rewatch included) and the
/// next episode to play never do; any other `Watched` (cached cards) goes.
fn mark_finished(entry: &mut ContinuationEntry) {
    super::super::watched::strip(&mut entry.card);
    if !matches!(entry.status, ContinuationStatus::Resume | ContinuationStatus::Next)
        && entry.card.identity.episode.is_some()
        && entry.card.identity == entry.record.identity
        && entry.card.badges.len() < 32
    {
        entry.card.badges.push(super::super::watched::BADGE.into());
    }
}

pub(crate) async fn get(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    Query(page): Query<Page>,
) -> ApiResult {
    let app = app.with_lease(lease.clone());
    let result = snapshot(&app, &lease, profile, page.offset, page.limit)?;
    Ok(axum::Json(
        serde_json::to_value(result).map_err(|_| ApiError::from("Continuation unavailable"))?,
    ))
}
