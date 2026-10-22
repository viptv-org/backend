//! Profile-owned live preferences. They are separate from movie/series My List.
use super::*;
use crate::{capabilities::Capability, registry::Registry};

impl Store {
    pub fn live_favorites(&self, scope: &Scope) -> Result<Vec<String>, Error> {
        let profile = self.profile(scope)?.ok_or(Error::Forbidden)?;
        if profile.scope != *scope {
            return Err(Error::Conflict);
        }
        let connections = self.connections(&scope.account_id)?;
        let eligible =
            Registry::builtin().connections_for(&profile, &connections, Capability::LiveTv)?;
        let mut query = self.db.prepare("SELECT channel_id,source_id,source_revision FROM engine_live_favorites WHERE connection=?1 AND account=?2 AND profile=?3 ORDER BY channel_id").map_err(|_|Error::Storage)?;
        let rows = query
            .query_map(
                params![scope.connection_id, scope.account_id, scope.profile_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, u64>(2)?,
                    ))
                },
            )
            .map_err(|_| Error::Storage)?;
        let mut result = Vec::new();
        for row in rows {
            let (channel, source, revision) = row.map_err(|_| Error::Storage)?;
            if eligible
                .iter()
                .any(|connection| connection.id == source && connection.revision == revision)
            {
                result.push(channel);
            }
        }
        Ok(result)
    }

    pub fn has_live_favorite(&self, scope: &Scope, channel: &str) -> Result<bool, Error> {
        let profile = self.profile(scope)?.ok_or(Error::Forbidden)?;
        if profile.scope != *scope {
            return Err(Error::Conflict);
        }
        self.db.query_row("SELECT EXISTS(SELECT 1 FROM engine_live_favorites WHERE connection=?1 AND account=?2 AND profile=?3 AND channel_id=?4)",params![scope.connection_id,scope.account_id,scope.profile_id,channel],|row|row.get(0)).map_err(|_|Error::Storage)
    }

    /// The host must first resolve the channel from its own current directory.
    pub fn set_live_favorite(
        &mut self,
        scope: &Scope,
        source: &str,
        channel: &str,
        saved: bool,
    ) -> Result<(), Error> {
        let profile = self.profile(scope)?.ok_or(Error::Forbidden)?;
        if profile.scope != *scope {
            return Err(Error::Conflict);
        }
        if channel.is_empty() || channel.len() > 512 || channel.chars().any(char::is_control) {
            return Err(Error::InvalidQuery);
        }
        if !saved {
            self.db.execute("DELETE FROM engine_live_favorites WHERE connection=?1 AND account=?2 AND profile=?3 AND channel_id=?4",params![scope.connection_id,scope.account_id,scope.profile_id,channel]).map_err(|_|Error::Storage)?;
            return Ok(());
        }
        let connections = self.connections(&scope.account_id)?;
        let eligible =
            Registry::builtin().connections_for(&profile, &connections, Capability::LiveTv)?;
        let connection = eligible
            .into_iter()
            .find(|connection| connection.id == source)
            .ok_or(Error::Forbidden)?;
        let tx = self.db.transaction().map_err(|_| Error::Storage)?;
        let exists:bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM engine_live_favorites WHERE connection=?1 AND account=?2 AND profile=?3 AND channel_id=?4)",params![scope.connection_id,scope.account_id,scope.profile_id,channel],|row|row.get(0)).map_err(|_|Error::Storage)?;
        if !exists {
            let count:u64 = tx.query_row("SELECT count(*) FROM engine_live_favorites WHERE connection=?1 AND account=?2 AND profile=?3",params![scope.connection_id,scope.account_id,scope.profile_id],|row|row.get(0)).map_err(|_|Error::Storage)?;
            if count >= 10000 {
                return Err(Error::InvalidQuery);
            }
        }
        tx.execute("INSERT INTO engine_live_favorites(connection,account,profile,channel_id,source_id,source_revision) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(connection,account,profile,channel_id) DO UPDATE SET source_id=excluded.source_id,source_revision=excluded.source_revision",params![scope.connection_id,scope.account_id,scope.profile_id,channel,source,connection.revision]).map_err(|_|Error::Storage)?;
        tx.commit().map_err(|_| Error::Storage)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Store, Scope, IntegrationConnection) {
        let mut store = Store::memory().unwrap();
        for id in ["first", "other"] {
            store
                .save_profile(
                    &ProfileConfiguration::local("local".into(), id.into(), id.into()),
                    None,
                )
                .unwrap();
        }
        let connection = IntegrationConnection {
            id: "m3u:fixture".into(),
            plugin_id: "m3u".into(),
            account_id: "local".into(),
            private_profile_id: Some("first".into()),
            name: "Fixture".into(),
            enabled: true,
            revision: 0,
            capabilities: vec![Capability::LiveTv, Capability::Sources],
        };
        store.install("first", &connection).unwrap();
        let scope = store
            .profile(
                &ProfileConfiguration::local("local".into(), "first".into(), "First".into()).scope,
            )
            .unwrap()
            .unwrap()
            .scope;
        (store, scope, connection)
    }
    fn current(store: &Store, profile: &str) -> Scope {
        store
            .profile(
                &ProfileConfiguration::local("local".into(), profile.into(), profile.into()).scope,
            )
            .unwrap()
            .unwrap()
            .scope
    }

    #[test]
    fn live_preferences_are_profile_scoped_idempotent_and_separate_from_my_list() {
        let (mut store, scope, connection) = fixture();
        store
            .set_live_favorite(&scope, &connection.id, "opaque-b", true)
            .unwrap();
        store
            .set_live_favorite(&scope, &connection.id, "opaque-a", true)
            .unwrap();
        store
            .set_live_favorite(&scope, &connection.id, "opaque-a", true)
            .unwrap();
        assert_eq!(
            store.live_favorites(&scope).unwrap(),
            vec!["opaque-a", "opaque-b"]
        );
        let other = current(&store, "other");
        assert!(store.live_favorites(&other).unwrap().is_empty());
        assert_eq!(
            store.set_live_favorite(&other, &connection.id, "opaque-a", true),
            Err(Error::Forbidden)
        );
        store
            .set_live_favorite(&other, "", "opaque-a", false)
            .unwrap();
        assert_eq!(store.live_favorites(&scope).unwrap().len(), 2);
        assert!(store.favorites(&scope, 0, 100).unwrap().cards.is_empty());
        store
            .set_live_favorite(&scope, "", "opaque-a", false)
            .unwrap();
        assert_eq!(store.live_favorites(&scope).unwrap(), vec!["opaque-b"]);
    }

    #[test]
    fn disabled_reconfigured_removed_connections_and_stale_revisions_cannot_reuse_favorites() {
        let (mut store, scope, mut connection) = fixture();
        store
            .set_live_favorite(&scope, &connection.id, "opaque-a", true)
            .unwrap();
        store
            .set_assignment("first", &connection.id, false)
            .unwrap();
        let disabled = current(&store, "first");
        assert!(store.live_favorites(&disabled).unwrap().is_empty());
        assert_eq!(
            store.set_live_favorite(&scope, &connection.id, "opaque-b", true),
            Err(Error::Conflict)
        );
        store.set_assignment("first", &connection.id, true).unwrap();
        let enabled = current(&store, "first");
        assert_eq!(store.live_favorites(&enabled).unwrap(), vec!["opaque-a"]);
        connection.revision += 1;
        store
            .replace_source_connection("first", enabled.revision, &connection)
            .unwrap();
        let changed = current(&store, "first");
        assert!(store.live_favorites(&changed).unwrap().is_empty());
        store
            .set_live_favorite(&changed, "", "opaque-a", false)
            .unwrap();
        store
            .set_live_favorite(&changed, &connection.id, "opaque-b", true)
            .unwrap();
        store.remove_connection(&connection.id).unwrap();
        let removed = current(&store, "first");
        assert!(store.live_favorites(&removed).unwrap().is_empty());
        let remaining: u64 = store
            .db
            .query_row("SELECT count(*) FROM engine_live_favorites", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(remaining, 0);
    }

    #[test]
    fn favorite_capacity_is_bounded_without_breaking_idempotent_existing_choices() {
        let (mut store, scope, connection) = fixture();
        {
            let tx = store.db.transaction().unwrap();
            {
                let mut insert=tx.prepare("INSERT INTO engine_live_favorites(connection,account,profile,channel_id,source_id,source_revision) VALUES('local','local','first',?1,?2,0)").unwrap();
                for index in 0..10000 {
                    insert
                        .execute(params![format!("opaque-{index:05}"), connection.id])
                        .unwrap();
                }
            }
            tx.commit().unwrap();
        }
        assert_eq!(
            store.set_live_favorite(&scope, &connection.id, "over-cap", true),
            Err(Error::InvalidQuery)
        );
        store
            .set_live_favorite(&scope, &connection.id, "opaque-00000", true)
            .unwrap();
        assert_eq!(store.live_favorites(&scope).unwrap().len(), 10000);
        store
            .set_live_favorite(&scope, "", "opaque-00000", false)
            .unwrap();
        store
            .set_live_favorite(&scope, &connection.id, "new-choice", true)
            .unwrap();
        assert_eq!(store.live_favorites(&scope).unwrap().len(), 10000);
    }
}
