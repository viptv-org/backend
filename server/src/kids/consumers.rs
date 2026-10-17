//! Narrow child-consumer checks; private-origin policy migration remains separate.
use super::*;
pub(crate) const UNSUPPORTED: &str =
    "This provider's private title IDs do not support restricted playback yet";

pub(crate) fn require(
    app: &App,
    kind: &str,
    id: &str,
    provider: &str,
    canonical_only: bool,
) -> Result<(), ApiError> {
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_media(&db)?;
    if !restricted(&db, &app.identity())? || !matches!(kind, "movie" | "series") {
        return Ok(());
    }
    require_item(&db, &app.identity(), kind, id)?;
    let parent: String = db
        .query_row(
            "SELECT parent_id FROM kids_media WHERE account_id=?1 AND kind=?2 AND id=?3",
            params![app.identity().account_id(), kind, id],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_error)?
        .ok_or_else(forbidden)?;
    // A public namespace cannot become an add-on-private title just by changing
    // a query parameter. Episode authority comes from the observed parent edge.
    if (id.starts_with("simkl:") || parent.starts_with("simkl:")) && provider != "simkl" {
        return Err(forbidden());
    }
    if canonical_only
        && !(titles::canonical("simkl", kind, &parent) && provider == "simkl"
            || titles::canonical("stremio", kind, &parent))
    {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            UNSUPPORTED.into(),
        ));
    }
    Ok(())
}
