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
    let queue = crate::continuation::queue(&db, profile, offset, limit)?;
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
        super::super::source_integrations::merge_cached(
            app,
            lease,
            configuration_revision,
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
    let next_cursor = queue["next_offset"].as_u64().map(|n| n.to_string());
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
