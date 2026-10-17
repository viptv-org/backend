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
    let queue = crate::continuation::queue(&db, profile, 0, 50000)?;
    let items = kids::filter_items(
        &db,
        &lease.principal,
        queue["items"].as_array().cloned().unwrap_or_default(),
    )?;
    let mut entries = Vec::new();
    for item in items {
        let Some(rendered) = super::super::history::record(&item) else {
            continue;
        };
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
        crate::simkl::imports::enrich(&db, profile, &mut card)?;
        super::super::source_integrations::merge_cached_with_db(
            app,
            lease,
            configuration_revision,
            &db,
            std::slice::from_mut(&mut card),
        );
        let previous_identity = previous.map(|record| record.identity);
        entries.push(ContinuationEntry {
            card,
            record,
            status,
            previous_identity,
        });
    }
    if queue["next_offset"].is_number() {
        return Err("History exceeds its bounded item limit".into());
    }
    for (key, entry) in super::super::scoped_library::continuation_db(&db, app, profile)? {
        entries.retain(|old| {
            let mut card = old.record.card();
            if let Some(parent) = &old.record.parent_identity {
                card.identity = parent.clone();
                card.parent_identity = None;
            }
            super::super::scoped_library::raw_key(&card).is_ok_and(|old| old != key)
        });
        entries.push(entry);
    }
    entries.sort_by(|a, b| {
        b.record
            .updated_at_millis
            .cmp(&a.record.updated_at_millis)
            .then_with(|| a.card.identity.id.cmp(&b.card.identity.id))
    });
    let total = entries.len();
    let next_cursor =
        (offset as usize + (limit as usize) < total).then(|| (offset + limit).to_string());
    let entries = entries
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .collect();
    let revision =
        configuration_revision.saturating_add(super::super::history::revision(&db, profile)?);
    Ok(ContinuationPage {
        scope: super::super::scope(lease, profile, configuration_revision),
        revision,
        entries,
        complete: next_cursor.is_none(),
        next_cursor,
    })
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
