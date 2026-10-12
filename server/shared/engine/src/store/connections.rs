use super::*;

impl Store {
    pub fn connections(&self, account: &str) -> Result<Vec<IntegrationConnection>, Error> {
        let mut query = self
            .db
            .prepare("SELECT value FROM engine_connections WHERE account=?1 ORDER BY id LIMIT 64")
            .map_err(|_| Error::Storage)?;
        let rows = query
            .query_map([account], |r| r.get::<_, String>(0))
            .map_err(|_| Error::Storage)?;
        rows.map(|r| {
            r.map_err(|_| Error::Storage)
                .and_then(|raw| serde_json::from_str(&raw).map_err(|_| Error::Storage))
        })
        .collect()
    }

    pub fn install(
        &mut self,
        profile_id: &str,
        connection: &IntegrationConnection,
    ) -> Result<(), Error> {
        if connection.account_id != "local"
            || !connection.id.starts_with("addon:")
            || connection.plugin_id != "stremio"
        {
            return Err(Error::InvalidQuery);
        }
        if self.connections("local")?.len() >= 32 {
            return Err(Error::InvalidQuery);
        }
        let scope = Scope {
            connection_id: "local".into(),
            account_id: "local".into(),
            profile_id: profile_id.into(),
            revision: 0,
        };
        let mut profile = self.profile(&scope)?.ok_or(Error::Forbidden)?;
        let prior = profile.scope.revision;
        profile.scope.revision = prior.checked_add(1).ok_or(Error::Conflict)?;
        profile.assignments.push(ProfileAssignment {
            connection_id: connection.id.clone(),
            enabled: connection.capabilities.clone(),
        });
        let tx = self.db.transaction().map_err(|_| Error::Storage)?;
        let current:u64=tx.query_row("SELECT revision FROM engine_profiles WHERE connection='local' AND account='local' AND profile=?1",[profile_id],|r|r.get(0)).map_err(|_|Error::Storage)?;
        if current != prior {
            return Err(Error::Conflict);
        }
        tx.execute(
            "INSERT INTO engine_connections(id,account,value) VALUES(?1,'local',?2)",
            params![
                connection.id,
                serde_json::to_string(connection).map_err(|_| Error::Storage)?
            ],
        )
        .map_err(|_| Error::Storage)?;
        tx.execute("UPDATE engine_profiles SET revision=?2,value=?3 WHERE connection='local' AND account='local' AND profile=?1",params![profile_id,profile.scope.revision,serde_json::to_string(&profile).map_err(|_|Error::Storage)?]).map_err(|_|Error::Storage)?;
        tx.commit().map_err(|_| Error::Storage)
    }
}
