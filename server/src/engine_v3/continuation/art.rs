//! Durable 16:9 art for Continue Watching cards that carry none.
//!
//! Stremio-imported history stores no artwork, and Home's metadata
//! enrichment lives only in memory: a restart empties it and its deadline
//! leaves later cards out. A card without a landscape takes, from what the
//! profile already stores: the still of its own episode in the series'
//! release document, then that document's series backdrop, then the backdrop
//! its My List card gained (the Stremio import's Details fill). Nothing is
//! fetched here.
use super::*;

pub(super) fn fill(
    db: &Connection,
    lease: &ResourceLease,
    profile: i64,
    restricted: bool,
    card: &mut Card,
) -> Result<(), ApiError> {
    if card.landscape.is_some() {
        return Ok(());
    }
    if let Some(series) = &card.parent_identity {
        card.landscape =
            super::releases::landscape_db(db, lease, profile, &series.id, &card.identity.id)?;
    }
    // Restricted profiles never read My List's stored cards (see `library::cards`).
    if card.landscape.is_none() && !restricted {
        let title = card.parent_identity.as_ref().unwrap_or(&card.identity);
        card.landscape = my_list(db, profile, &title.kind, &title.id)?;
    }
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
        .and_then(|raw| serde_json::from_str::<Card>(&raw).ok())
        .filter(|card| card.identity.kind == kind && card.identity.id == id)
        .and_then(|card| card.landscape))
}
