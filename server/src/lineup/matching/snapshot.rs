use super::*;

// A private shared-memory SQLite database keeps snapshot copying in SQLite,
// without copying profile/history/VOD tables or holding the application mutex
// during matching. No snapshot or credentials are written to the filesystem.
pub(super) const SNAPSHOT_TABLES: &[&str] = &[
    "providers",
    "provider_live",
    "live_category_rules",
    "account_pools",
    "provider_pools",
    "family_channels",
    "family_candidates",
    "family_aliases",
    "family_matching_settings",
    "family_match_overrides",
    "family_match_aliases",
    "family_verified_ids",
    "family_provider_groups",
    "family_match_results",
    "candidate_health",
    "health_accounts",
];
struct Snapshot {
    db: Connection,
    uri: String,
    revision: i64,
}
fn snapshot(db: &Connection) -> Result<Snapshot, ApiError> {
    let (streams, channels): (i64, i64) = db
        .query_row(
            "SELECT (SELECT COUNT(*) FROM provider_live),(SELECT COUNT(*) FROM family_channels)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(db_error)?;
    if streams > 500_000 || channels > 1000 {
        return Err("Matching supports up to 500000 imported live entries and 1000 family identities per pass; narrow the selected imports".into());
    }

    let providers = db
        .prepare("SELECT id FROM providers")
        .map_err(db_error)?
        .query_map([], |r| r.get::<_, i64>(0))
        .map_err(db_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error)?;
    for id in providers {
        crate::provider::pools::ensure(db, id)?;
    }
    let uri = format!(
        "file:viptv-family-match-{}?mode=memory&cache=shared",
        uuid::Uuid::new_v4()
    );
    let copy = Connection::open_with_flags(
        &uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
            | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
            | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(db_error)?;
    db.execute("ATTACH DATABASE ?1 AS family_snapshot", [&uri])
        .map_err(db_error)?;
    let result = (|| {
        let tx = db.unchecked_transaction().map_err(db_error)?;
        let revision = tx
            .query_row(
                "SELECT value FROM family_matching_revision WHERE id=1",
                [],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        for table in SNAPSHOT_TABLES {
            let schema: String = tx
                .query_row(
                    "SELECT sql FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |r| r.get(0),
                )
                .map_err(db_error)?;
            let columns = schema
                .find('(')
                .ok_or("Matching snapshot schema unavailable")?;
            tx.execute_batch(&format!(
                "CREATE TABLE family_snapshot.{table} {}",
                &schema[columns..]
            ))
            .map_err(db_error)?;
        }
        for table in SNAPSHOT_TABLES {
            tx.execute_batch(&format!(
                "INSERT INTO family_snapshot.{table} SELECT * FROM main.{table}"
            ))
            .map_err(db_error)?;
        }
        tx.commit().map_err(db_error)?;
        Ok::<_, ApiError>(revision)
    })();
    let detached = db
        .execute_batch("DETACH DATABASE family_snapshot")
        .map_err(db_error);
    let revision = result?;
    detached?;
    Ok(Snapshot {
        db: copy,
        uri,
        revision,
    })
}
pub(super) fn match_owned(a: &App, lease: &ResourceLease) -> Result<Value, ApiError> {
    match_authorized(&a.db, &a.family_matching_gate, |db| owner(lease, db))
}
pub(crate) fn match_catalog(
    db: &Mutex<Connection>,
    gate: &Mutex<()>,
    lease: &crate::automation::CatalogLease,
) -> Result<Value, ApiError> {
    match_authorized(db, gate, |db| lease.validate(db).map_err(ApiError::from))
}
pub(crate) fn match_health(
    a: &App,
    lease: &crate::automation::OwnerAccess,
) -> Result<Value, ApiError> {
    match_authorized(&a.db, &a.family_matching_gate, |db| lease.validate(db))
}
fn match_authorized(
    shared_db: &Mutex<Connection>,
    gate: &Mutex<()>,
    authorize: impl Fn(&Connection) -> Result<(), ApiError>,
) -> Result<Value, ApiError> {
    let _job = gate.try_lock().map_err(|_| {
        ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "Matching is already running; retry to apply the latest settings".into(),
        )
    })?;
    let mut staged = {
        let db = shared_db.lock().unwrap();
        authorize(&db)?;
        snapshot(&db)?
    };
    reconcile(&mut staged.db)?;
    let mut db = shared_db.lock().unwrap();
    authorize(&db)?;
    let revision: i64 = db
        .query_row(
            "SELECT value FROM family_matching_revision WHERE id=1",
            [],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    if revision != staged.revision {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Catalog or matching settings changed during this pass; run matching again".into(),
        ));
    }
    db.execute("ATTACH DATABASE ?1 AS family_snapshot", [&staged.uri])
        .map_err(db_error)?;
    let result = (|| {
        let tx = db.transaction().map_err(db_error)?;
        tx.execute_batch("DELETE FROM main.family_candidates; INSERT INTO main.family_candidates SELECT * FROM family_snapshot.family_candidates;
            DELETE FROM main.family_match_results; INSERT INTO main.family_match_results SELECT * FROM family_snapshot.family_match_results;
            UPDATE main.family_channels SET data=(SELECT data FROM family_snapshot.family_channels s WHERE s.id=family_channels.id);").map_err(db_error)?;
        authorize(&tx)?;
        tx.commit().map_err(db_error)
    })();
    let detached = db
        .execute_batch("DETACH DATABASE family_snapshot")
        .map_err(db_error);
    result?;
    detached?;
    summary(&db, None, 0)
}
