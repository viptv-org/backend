use super::*;

impl Store {
    pub fn set_assignment(
        &mut self,
        profile_id: &str,
        connection_id: &str,
        enabled: bool,
    ) -> Result<(), Error> {
        let connection = self
            .connections("local")?
            .into_iter()
            .find(|c| c.id == connection_id)
            .ok_or(Error::Forbidden)?;
        let scope = Scope {
            connection_id: "local".into(),
            account_id: "local".into(),
            profile_id: profile_id.into(),
            revision: 0,
        };
        let mut profile = self.profile(&scope)?.ok_or(Error::Forbidden)?;
        let before = profile.scope.revision;
        let assignment = profile
            .assignments
            .iter_mut()
            .find(|a| a.connection_id == connection_id);
        match assignment {
            Some(value) if enabled => value.enabled = connection.capabilities,
            Some(value) => value.enabled.clear(),
            None if enabled => profile.assignments.push(ProfileAssignment {
                connection_id: connection_id.into(),
                enabled: connection.capabilities,
            }),
            None => return Ok(()),
        }
        profile.scope.revision = before.checked_add(1).ok_or(Error::Conflict)?;
        self.save_profile(&profile, Some(before))
    }

    pub fn remove_connection(&mut self, id: &str) -> Result<(), Error> {
        if !self.connections("local")?.iter().any(|c| c.id == id) {
            return Err(Error::Forbidden);
        }
        let tx = self.db.transaction().map_err(|_| Error::Storage)?;
        let profiles = {
            let mut q=tx.prepare("SELECT profile,revision,value FROM engine_profiles WHERE connection='local' AND account='local'").map_err(|_|Error::Storage)?;
            q.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, u64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(|_| Error::Storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| Error::Storage)?
        };
        for (profile_id, revision, raw) in profiles {
            let mut profile: ProfileConfiguration =
                serde_json::from_str(&raw).map_err(|_| Error::Storage)?;
            let previous = profile.assignments.len();
            profile.assignments.retain(|a| a.connection_id != id);
            if previous == profile.assignments.len() {
                continue;
            }
            profile.scope.revision = revision.checked_add(1).ok_or(Error::Conflict)?;
            tx.execute("UPDATE engine_profiles SET revision=?2,value=?3 WHERE connection='local' AND account='local' AND profile=?1",params![profile_id,profile.scope.revision,serde_json::to_string(&profile).map_err(|_|Error::Storage)?]).map_err(|_|Error::Storage)?;
        }
        tx.execute(
            "DELETE FROM engine_connections WHERE id=?1 AND account='local'",
            [id],
        )
        .map_err(|_| Error::Storage)?;
        tx.commit().map_err(|_| Error::Storage)
    }
    pub fn assign(
        &mut self,
        profile_id: &str,
        connection: &IntegrationConnection,
    ) -> Result<(), Error> {
        let scope = Scope {
            connection_id: "local".into(),
            account_id: "local".into(),
            profile_id: profile_id.into(),
            revision: 0,
        };
        let mut profile = self.profile(&scope)?.ok_or(Error::Forbidden)?;
        if connection.account_id != "local"
            || connection
                .private_profile_id
                .as_deref()
                .is_some_and(|id| id != profile_id)
        {
            return Err(Error::Forbidden);
        }
        if profile
            .assignments
            .iter()
            .any(|a| a.connection_id == connection.id)
        {
            return Ok(());
        }
        let previous = profile.scope.revision;
        profile.scope.revision = previous.checked_add(1).ok_or(Error::Conflict)?;
        profile.assignments.push(ProfileAssignment {
            connection_id: connection.id.clone(),
            enabled: connection.capabilities.clone(),
        });
        self.save_profile(&profile, Some(previous))
    }
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
            || !((connection.id.starts_with("addon:") && connection.plugin_id == "stremio")
                || (connection.id.starts_with("vsources:")
                    && connection.plugin_id == "vsources"
                    && connection.private_profile_id.as_deref() == Some(profile_id)))
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

impl Store {
    /// Replace profile-private source credentials without exposing vault data or losing assignments.
    pub fn replace_source_connection(
        &mut self,
        profile_id: &str,
        expected: u64,
        connection: &IntegrationConnection,
    ) -> Result<(), Error> {
        let existing = self
            .connections("local")?
            .into_iter()
            .find(|c| c.id == connection.id)
            .ok_or(Error::Forbidden)?;
        if existing.plugin_id != "vsources"
            || connection.plugin_id != "vsources"
            || existing.private_profile_id.as_deref() != Some(profile_id)
            || connection.private_profile_id != existing.private_profile_id
            || connection.account_id != "local"
            || connection.revision != existing.revision.checked_add(1).ok_or(Error::Conflict)?
        {
            return Err(Error::Forbidden);
        }
        let scope = Scope {
            connection_id: "local".into(),
            account_id: "local".into(),
            profile_id: profile_id.into(),
            revision: expected,
        };
        let mut profile = self.profile(&scope)?.ok_or(Error::Forbidden)?;
        if profile.scope.revision != expected {
            return Err(Error::Conflict);
        }
        profile.scope.revision = expected.checked_add(1).ok_or(Error::Conflict)?;
        if let Some(assignment) = profile
            .assignments
            .iter_mut()
            .find(|a| a.connection_id == connection.id)
        {
            assignment.enabled = connection.capabilities.clone();
        } else {
            profile.assignments.push(ProfileAssignment {
                connection_id: connection.id.clone(),
                enabled: connection.capabilities.clone(),
            });
        }
        let tx = self.db.transaction().map_err(|_| Error::Storage)?;
        let updated=tx.execute("UPDATE engine_profiles SET revision=?4,value=?5 WHERE connection=?1 AND account=?2 AND profile=?3 AND revision=?6",params![scope.connection_id,scope.account_id,scope.profile_id,profile.scope.revision,serde_json::to_string(&profile).map_err(|_|Error::Storage)?,expected]).map_err(|_|Error::Storage)?;
        if updated != 1 {
            return Err(Error::Conflict);
        }
        tx.execute(
            "UPDATE engine_connections SET value=?2 WHERE id=?1 AND account='local'",
            params![
                connection.id,
                serde_json::to_string(connection).map_err(|_| Error::Storage)?
            ],
        )
        .map_err(|_| Error::Storage)?;
        tx.commit().map_err(|_| Error::Storage)
    }
}
