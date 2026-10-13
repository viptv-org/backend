use crate::{Error, profile::*};
use rusqlite::{Connection, OptionalExtension, params};

pub struct Store {
    db: Connection,
}
mod catalog;
mod connections;
mod library;
mod playback;
mod profiles;

impl Store {
    pub fn open(path: &std::path::Path) -> Result<Self, Error> {
        Self::from_connection(Connection::open(path).map_err(|_| Error::Storage)?)
    }
    pub fn memory() -> Result<Self, Error> {
        Self::from_connection(Connection::open_in_memory().map_err(|_| Error::Storage)?)
    }
    fn from_connection(db: Connection) -> Result<Self, Error> {
        db.execute_batch("PRAGMA foreign_keys=ON;
          CREATE TABLE IF NOT EXISTS engine_profiles(connection TEXT NOT NULL, account TEXT NOT NULL, profile TEXT NOT NULL, revision INTEGER NOT NULL, value TEXT NOT NULL, PRIMARY KEY(connection,account,profile));
          CREATE TABLE IF NOT EXISTS engine_observations(connection TEXT NOT NULL, account TEXT NOT NULL, profile TEXT NOT NULL, event TEXT NOT NULL, value TEXT NOT NULL, PRIMARY KEY(connection,account,profile,event));
          CREATE TABLE IF NOT EXISTS engine_connections(id TEXT PRIMARY KEY, account TEXT NOT NULL, value TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS engine_catalog_cache(connection TEXT NOT NULL, account TEXT NOT NULL, profile TEXT NOT NULL, revision INTEGER NOT NULL, row_id TEXT NOT NULL, expires_at INTEGER NOT NULL, value TEXT NOT NULL, PRIMARY KEY(connection,account,profile,row_id));
          CREATE TABLE IF NOT EXISTS engine_playback(connection TEXT NOT NULL, account TEXT NOT NULL, profile TEXT NOT NULL, kind TEXT NOT NULL, id TEXT NOT NULL, updated_at_millis INTEGER NOT NULL, value TEXT NOT NULL, PRIMARY KEY(connection,account,profile,kind,id));
          CREATE TABLE IF NOT EXISTS engine_favorites(connection TEXT NOT NULL, account TEXT NOT NULL, profile TEXT NOT NULL, kind TEXT NOT NULL, id TEXT NOT NULL, title TEXT NOT NULL, value TEXT NOT NULL, PRIMARY KEY(connection,account,profile,kind,id), FOREIGN KEY(connection,account,profile) REFERENCES engine_profiles(connection,account,profile) ON DELETE CASCADE);
          CREATE TABLE IF NOT EXISTS engine_library_revisions(connection TEXT NOT NULL, account TEXT NOT NULL, profile TEXT NOT NULL, revision INTEGER NOT NULL, PRIMARY KEY(connection,account,profile), FOREIGN KEY(connection,account,profile) REFERENCES engine_profiles(connection,account,profile) ON DELETE CASCADE);
          CREATE TABLE IF NOT EXISTS engine_profile_locks(connection TEXT NOT NULL, account TEXT NOT NULL, profile TEXT NOT NULL, pin_hash TEXT NOT NULL, failures INTEGER NOT NULL DEFAULT 0, blocked_until INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(connection,account,profile));
          CREATE INDEX IF NOT EXISTS engine_playback_recent ON engine_playback(connection,account,profile,updated_at_millis DESC);").map_err(|_| Error::Storage)?;
        Ok(Self { db })
    }
    pub fn profile(&self, scope: &Scope) -> Result<Option<ProfileConfiguration>, Error> {
        let value: Option<String> = self.db.query_row("SELECT value FROM engine_profiles WHERE connection=?1 AND account=?2 AND profile=?3", params![scope.connection_id, scope.account_id, scope.profile_id], |r| r.get(0)).optional().map_err(|_| Error::Storage)?;
        value
            .map(|v| serde_json::from_str(&v).map_err(|_| Error::Storage))
            .transpose()
    }
    pub fn profiles(
        &self,
        connection: &str,
        account: &str,
    ) -> Result<Vec<ProfileConfiguration>, Error> {
        let mut q=self.db.prepare("SELECT value FROM engine_profiles WHERE connection=?1 AND account=?2 ORDER BY profile LIMIT 12").map_err(|_|Error::Storage)?;
        let rows = q
            .query_map(params![connection, account], |r| r.get::<_, String>(0))
            .map_err(|_| Error::Storage)?;
        rows.map(|r| {
            r.map_err(|_| Error::Storage)
                .and_then(|v| serde_json::from_str(&v).map_err(|_| Error::Storage))
        })
        .collect()
    }
    /// Compare-and-swap prevents a late settings request overwriting newer choices.
    pub fn save_profile(
        &mut self,
        profile: &ProfileConfiguration,
        expected_revision: Option<u64>,
    ) -> Result<(), Error> {
        let tx = self.db.transaction().map_err(|_| Error::Storage)?;
        let s = &profile.scope;
        let current: Option<u64> = tx.query_row("SELECT revision FROM engine_profiles WHERE connection=?1 AND account=?2 AND profile=?3", params![s.connection_id,s.account_id,s.profile_id], |r| r.get(0)).optional().map_err(|_| Error::Storage)?;
        if current != expected_revision
            || current.is_some_and(|r| s.revision != r + 1)
            || (current.is_none() && s.revision != 0)
        {
            return Err(Error::Conflict);
        }
        let json = serde_json::to_string(profile).map_err(|_| Error::Storage)?;
        tx.execute("INSERT INTO engine_profiles VALUES(?1,?2,?3,?4,?5) ON CONFLICT(connection,account,profile) DO UPDATE SET revision=excluded.revision,value=excluded.value",params![s.connection_id,s.account_id,s.profile_id,s.revision,json]).map_err(|_| Error::Storage)?;
        tx.commit().map_err(|_| Error::Storage)
    }
    pub fn observe(&self, scope: &Scope, event_id: &str, value: &str) -> Result<bool, Error> {
        if event_id.is_empty() || event_id.len() > 128 || value.len() > 65536 {
            return Err(Error::InvalidQuery);
        }
        let profile = self.profile(scope)?.ok_or(Error::Forbidden)?;
        if profile.scope.revision != scope.revision {
            return Err(Error::Conflict);
        }
        self.db
            .execute(
                "INSERT OR IGNORE INTO engine_observations VALUES(?1,?2,?3,?4,?5)",
                params![
                    scope.connection_id,
                    scope.account_id,
                    scope.profile_id,
                    event_id,
                    value
                ],
            )
            .map(|n| n == 1)
            .map_err(|_| Error::Storage)
    }
}
