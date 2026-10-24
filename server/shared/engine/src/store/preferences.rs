use super::*;
use crate::preferences::{DisplayPreferences, PreferencesSnapshot, ProfilePreferences};
impl Store {
    pub fn preferences(&self, scope: &Scope) -> Result<PreferencesSnapshot, Error> {
        self.profile(scope)?.ok_or(Error::Forbidden)?;
        self.db.execute_batch("CREATE TABLE IF NOT EXISTS engine_profile_preferences(connection TEXT NOT NULL,account TEXT NOT NULL,profile TEXT NOT NULL,revision INTEGER NOT NULL,value TEXT NOT NULL,PRIMARY KEY(connection,account,profile),FOREIGN KEY(connection,account,profile) REFERENCES engine_profiles(connection,account,profile) ON DELETE CASCADE)").map_err(|_|Error::Storage)?;
        let current:Option<(u64,String)>=self.db.query_row("SELECT revision,value FROM engine_profile_preferences WHERE connection=?1 AND account=?2 AND profile=?3",params![scope.connection_id,scope.account_id,scope.profile_id],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(|_|Error::Storage)?;
        match current {
            Some((revision, value)) => Ok(PreferencesSnapshot {
                revision,
                preferences: serde_json::from_str(&value).map_err(|_| Error::Storage)?,
                display: DisplayPreferences::default(),
            }),
            None => Ok(PreferencesSnapshot {
                revision: 0,
                preferences: ProfilePreferences::default(),
                display: DisplayPreferences::default(),
            }),
        }
    }
    pub fn update_preferences(
        &mut self,
        scope: &Scope,
        expected: u64,
        preferences: ProfilePreferences,
    ) -> Result<PreferencesSnapshot, Error> {
        preferences.validate()?;
        let snapshot = self.preferences(scope)?;
        if snapshot.revision != expected {
            return Err(Error::Conflict);
        }
        let revision = expected.checked_add(1).ok_or(Error::Conflict)?;
        let changed=self.db.execute("INSERT INTO engine_profile_preferences VALUES(?1,?2,?3,?4,?5) ON CONFLICT(connection,account,profile) DO UPDATE SET revision=excluded.revision,value=excluded.value WHERE engine_profile_preferences.revision=?6",params![scope.connection_id,scope.account_id,scope.profile_id,revision,serde_json::to_string(&preferences).map_err(|_|Error::Storage)?,expected]).map_err(|_|Error::Storage)?;
        if changed != 1 {
            return Err(Error::Conflict);
        }
        Ok(PreferencesSnapshot {
            revision,
            preferences,
            display: snapshot.display,
        })
    }
}
