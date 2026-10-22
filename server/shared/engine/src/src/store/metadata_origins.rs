use super::*;
use crate::origin::MetadataOrigin;

pub(super) fn init(db: &Connection) -> Result<(), Error> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS engine_connection_incarnations(id TEXT PRIMARY KEY REFERENCES engine_connections(id) ON DELETE CASCADE,incarnation TEXT NOT NULL UNIQUE);
    INSERT OR IGNORE INTO engine_connection_incarnations(id,incarnation) SELECT id,lower(hex(randomblob(16))) FROM engine_connections;
    CREATE TRIGGER IF NOT EXISTS engine_connection_incarnation_insert AFTER INSERT ON engine_connections BEGIN INSERT INTO engine_connection_incarnations(id,incarnation) VALUES(NEW.id,lower(hex(randomblob(16))));END;")
        .map_err(|_|Error::Storage)
}
impl Store {
    /// Current owned lookup provenance. Producer-specific capability checks stay
    /// in the host; catalog-only connectors may still issue private identities.
    pub fn metadata_origin(&self, scope: &Scope, id: &str) -> Result<MetadataOrigin, Error> {
        let profile = self.profile(scope)?.ok_or(Error::Forbidden)?;
        let connection = self
            .connections(&scope.account_id)?
            .into_iter()
            .find(|connection| {
                connection.id == id
                    && connection.plugin_id == "stremio"
                    && connection.enabled
                    && connection
                        .private_profile_id
                        .as_ref()
                        .is_none_or(|owner| owner == &scope.profile_id)
            })
            .ok_or(Error::Forbidden)?;
        if !profile.assignments.iter().any(|assignment| {
            assignment.connection_id == connection.id && !assignment.enabled.is_empty()
        }) {
            return Err(Error::Forbidden);
        }
        let incarnation = self
            .db
            .query_row(
                "SELECT incarnation FROM engine_connection_incarnations WHERE id=?1",
                [id],
                |row| row.get(0),
            )
            .map_err(|_| Error::Storage)?;
        Ok(MetadataOrigin {
            provider_id: "stremio".into(),
            connection_id: Some(id.into()),
            incarnation: Some(incarnation),
        })
    }
    pub fn validate_metadata_origin(
        &self,
        scope: &Scope,
        origin: &MetadataOrigin,
    ) -> Result<(), Error> {
        origin.validate()?;
        let id = origin.connection_id.as_deref().ok_or(Error::Forbidden)?;
        if &self.metadata_origin(scope, id)? != origin {
            return Err(Error::Forbidden);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incarnation_survives_normal_changes_and_rejects_deleted_id_recreation() {
        let mut store = Store::memory().unwrap();
        let profile = ProfileConfiguration::local("local".into(), "p".into(), "Profile".into());
        store.save_profile(&profile, None).unwrap();
        let mut connection = IntegrationConnection {
            id: "addon:one".into(),
            plugin_id: "stremio".into(),
            account_id: "local".into(),
            private_profile_id: None,
            name: "Fixture".into(),
            enabled: true,
            revision: 1,
            capabilities: vec![crate::capabilities::Capability::Search],
        };
        store.install("p", &connection).unwrap();
        let original = store
            .metadata_origin(&profile.scope, &connection.id)
            .unwrap();
        connection.revision = 2;
        store
            .db
            .execute(
                "UPDATE engine_connections SET value=?2 WHERE id=?1",
                params![connection.id, serde_json::to_string(&connection).unwrap()],
            )
            .unwrap();
        assert_eq!(
            store
                .metadata_origin(&profile.scope, &connection.id)
                .unwrap(),
            original
        );
        store.set_assignment("p", &connection.id, false).unwrap();
        assert!(
            store
                .metadata_origin(&profile.scope, &connection.id)
                .is_err()
        );
        store.set_assignment("p", &connection.id, true).unwrap();
        assert_eq!(
            store
                .metadata_origin(&profile.scope, &connection.id)
                .unwrap(),
            original
        );
        store.remove_connection(&connection.id).unwrap();
        store.install("p", &connection).unwrap();
        assert_ne!(
            store
                .metadata_origin(&profile.scope, &connection.id)
                .unwrap(),
            original
        );
        assert!(
            store
                .validate_metadata_origin(&profile.scope, &original)
                .is_err()
        );
    }
}
