use super::*;

pub(super) fn fill(
    db: &Connection,
    lease: &ResourceLease,
    profile: i64,
    restricted: bool,
    card: &mut Card,
) -> Result<(), ApiError> {
    if card.landscape.is_none() {
        if let Some(series) = &card.parent_identity {
            card.landscape =
                super::releases::landscape_db(db, lease, profile, &series.id, &card.identity.id)?;
        }
        if card.landscape.is_none() && !restricted {
            let title = card.parent_identity.as_ref().unwrap_or(&card.identity);
            card.landscape = my_list(db, profile, &title.kind, &title.id)?;
        }
    }
    super::super::art::fill(card);
    Ok(())
}

fn my_list(
    db: &Connection,
    profile: i64,
    kind: &str,
    id: &str,
) -> Result<Option<String>, ApiError> {
    let raw: Option<String> = db
        .query_row(
            "SELECT context FROM favorites WHERE profile_id=?1 AND type=?2 AND id=?3",
            params![profile, kind, id],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?;
    Ok(raw
        .and_then(|raw| super::super::library::hydration::stored(&raw, kind, id))
        .and_then(|card| card.landscape))
}
