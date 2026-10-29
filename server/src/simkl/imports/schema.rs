use super::*;
pub(in crate::simkl) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS simkl_import_entries(profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,remote_id TEXT NOT NULL,remote_kind TEXT NOT NULL,local_id TEXT NOT NULL,local_kind TEXT NOT NULL,card TEXT NOT NULL,statistics TEXT NOT NULL,link_revision INTEGER NOT NULL,PRIMARY KEY(profile_id,remote_kind,remote_id));
    CREATE TRIGGER IF NOT EXISTS simkl_import_entries_revision_insert AFTER INSERT ON simkl_import_entries BEGIN INSERT INTO engine_library_revisions(profile_id,revision) SELECT NEW.profile_id,1 WHERE EXISTS(SELECT 1 FROM profiles WHERE id=NEW.profile_id) ON CONFLICT(profile_id) DO UPDATE SET revision=revision+1;END;
    CREATE TRIGGER IF NOT EXISTS simkl_import_entries_revision_update AFTER UPDATE ON simkl_import_entries BEGIN INSERT INTO engine_library_revisions(profile_id,revision) SELECT NEW.profile_id,1 WHERE EXISTS(SELECT 1 FROM profiles WHERE id=NEW.profile_id) ON CONFLICT(profile_id) DO UPDATE SET revision=revision+1;END;
    CREATE TABLE IF NOT EXISTS library_removals(profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,type TEXT NOT NULL,id TEXT NOT NULL,PRIMARY KEY(profile_id,type,id));
    CREATE TRIGGER IF NOT EXISTS library_removal_record AFTER DELETE ON favorites BEGIN INSERT OR IGNORE INTO library_removals SELECT OLD.profile_id,OLD.type,OLD.id WHERE EXISTS(SELECT 1 FROM profiles WHERE id=OLD.profile_id);END;
    CREATE TRIGGER IF NOT EXISTS library_removal_clear AFTER INSERT ON favorites BEGIN DELETE FROM library_removals WHERE profile_id=NEW.profile_id AND type=NEW.type AND id=NEW.id;END;
    CREATE TABLE IF NOT EXISTS library_history_removals(profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,type TEXT NOT NULL,id TEXT NOT NULL,PRIMARY KEY(profile_id,type,id));
    CREATE TRIGGER IF NOT EXISTS library_history_removal_record AFTER DELETE ON progress BEGIN INSERT OR IGNORE INTO library_history_removals SELECT OLD.profile_id,OLD.type,OLD.id WHERE EXISTS(SELECT 1 FROM profiles WHERE id=OLD.profile_id);END;
    CREATE TRIGGER IF NOT EXISTS library_history_removal_clear AFTER INSERT ON progress BEGIN DELETE FROM library_history_removals WHERE profile_id=NEW.profile_id AND type=NEW.type AND id=NEW.id;END;
    CREATE TABLE IF NOT EXISTS simkl_import_history_floors(profile_id INTEGER PRIMARY KEY REFERENCES profiles(id) ON DELETE CASCADE,cutoff_millis INTEGER NOT NULL);
    CREATE TABLE IF NOT EXISTS simkl_import_progress(profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,type TEXT NOT NULL,id TEXT NOT NULL,receipt TEXT NOT NULL,source_updated_at_millis INTEGER NOT NULL,PRIMARY KEY(profile_id,type,id));
    CREATE TABLE IF NOT EXISTS simkl_import_snapshots(profile_id INTEGER PRIMARY KEY REFERENCES profiles(id) ON DELETE CASCADE,account_id INTEGER NOT NULL,link_revision INTEGER NOT NULL,configuration_revision INTEGER NOT NULL,activities TEXT NOT NULL,items TEXT NOT NULL);")
}
pub(in crate::simkl) fn validate_db(
    db: &Connection,
    app: &App,
    profile: i64,
    access: &store::Credential,
    lease: &ResourceLease,
) -> Result<(), ApiError> {
    lease.validate(db)?;
    app.require_profile(db, profile)?;
    let revision: Option<u64> = db.query_row("SELECT revision FROM simkl_links WHERE profile_id=?1 AND account_id=?2 AND credential IS NOT NULL", params![profile,app.identity().account_id()], |r|r.get(0)).optional().map_err(db_error)?;
    let config: u64 = db
        .query_row(
            "SELECT revision FROM engine_profile_configuration WHERE profile_id=?1",
            [profile],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?
        .unwrap_or(0);
    if revision != access.revision || config != access.configuration_revision {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Simkl import authority changed".into(),
        ));
    }
    Ok(())
}
