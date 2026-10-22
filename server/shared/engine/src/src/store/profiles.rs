use super::*;
use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng},
};
use std::time::{SystemTime, UNIX_EPOCH};

fn valid_pin(pin: &str) -> bool {
    (4..=8).contains(&pin.len()) && pin.bytes().all(|b| b.is_ascii_digit())
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |t| t.as_secs() as i64)
}

impl Store {
    pub fn set_profile_avatar(
        &mut self,
        scope: &Scope,
        avatar_id: &str,
        pin: &str,
    ) -> Result<(), Error> {
        if !matches!(
            avatar_id,
            "" | "vynxc" | "zayne" | "initial-pink" | "initial-blue"
        ) {
            return Err(Error::InvalidQuery);
        }
        if self.has_profile_pin(scope)? && !self.verify_profile_pin(scope, pin)? {
            return Err(Error::Forbidden);
        }
        let mut profile = self.profile(scope)?.ok_or(Error::Forbidden)?;
        let old = profile.scope.revision;
        profile.scope.revision = old.checked_add(1).ok_or(Error::Conflict)?;
        profile.avatar_id = if avatar_id.is_empty() {
            None
        } else {
            Some(avatar_id.into())
        };
        self.save_profile(&profile, Some(old))
    }

    pub fn has_profile_pin(&self, scope: &Scope) -> Result<bool, Error> {
        let found: Option<i64> = self.db.query_row(
            "SELECT 1 FROM engine_profile_locks WHERE connection=?1 AND account=?2 AND profile=?3",
            params![scope.connection_id, scope.account_id, scope.profile_id], |r| r.get(0),
        ).optional().map_err(|_| Error::Storage)?;
        Ok(found.is_some())
    }

    pub fn verify_profile_pin(&mut self, scope: &Scope, pin: &str) -> Result<bool, Error> {
        if !valid_pin(pin) || self.profile(scope)?.is_none() {
            return Ok(false);
        }
        let row: Option<(String, i64, i64)> = self.db.query_row(
            "SELECT pin_hash,failures,blocked_until FROM engine_profile_locks WHERE connection=?1 AND account=?2 AND profile=?3",
            params![scope.connection_id, scope.account_id, scope.profile_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).optional().map_err(|_| Error::Storage)?;
        let Some((hash, failures, blocked_until)) = row else {
            return Ok(false);
        };
        if blocked_until > now() {
            return Ok(false);
        }
        let verified = PasswordHash::new(&hash).ok().is_some_and(|parsed| {
            Argon2::default()
                .verify_password(pin.as_bytes(), &parsed)
                .is_ok()
        });
        let next_failures = if verified {
            0
        } else {
            failures.saturating_add(1)
        };
        self.db.execute(
            "UPDATE engine_profile_locks SET failures=?4,blocked_until=?5 WHERE connection=?1 AND account=?2 AND profile=?3",
            params![scope.connection_id, scope.account_id, scope.profile_id, next_failures,
                if next_failures >= 5 { now() + 300 } else { 0 }],
        ).map_err(|_| Error::Storage)?;
        Ok(verified)
    }

    pub fn set_profile_pin(
        &mut self,
        scope: &Scope,
        old_pin: &str,
        new_pin: &str,
    ) -> Result<(), Error> {
        if self.profile(scope)?.is_none() {
            return Err(Error::Forbidden);
        }
        if self.has_profile_pin(scope)? && !self.verify_profile_pin(scope, old_pin)? {
            return Err(Error::Forbidden);
        }
        if new_pin.is_empty() {
            self.db.execute("DELETE FROM engine_profile_locks WHERE connection=?1 AND account=?2 AND profile=?3",
                params![scope.connection_id, scope.account_id, scope.profile_id]).map_err(|_| Error::Storage)?;
            return Ok(());
        }
        if !valid_pin(new_pin) {
            return Err(Error::InvalidQuery);
        }
        let hash = Argon2::default()
            .hash_password(new_pin.as_bytes(), &SaltString::generate(&mut OsRng))
            .map_err(|_| Error::Storage)?
            .to_string();
        self.db.execute("INSERT INTO engine_profile_locks(connection,account,profile,pin_hash) VALUES(?1,?2,?3,?4) ON CONFLICT(connection,account,profile) DO UPDATE SET pin_hash=excluded.pin_hash,failures=0,blocked_until=0",
            params![scope.connection_id, scope.account_id, scope.profile_id, hash]).map_err(|_| Error::Storage)?;
        Ok(())
    }

    pub fn rename_profile(&mut self, scope: &Scope, name: &str, pin: &str) -> Result<(), Error> {
        if name.trim().is_empty() || name.chars().count() > 64 {
            return Err(Error::InvalidQuery);
        }
        if self.has_profile_pin(scope)? && !self.verify_profile_pin(scope, pin)? {
            return Err(Error::Forbidden);
        }
        let mut profile = self.profile(scope)?.ok_or(Error::Forbidden)?;
        let old = profile.scope.revision;
        profile.scope.revision = old.checked_add(1).ok_or(Error::Conflict)?;
        profile.name = name.trim().into();
        self.save_profile(&profile, Some(old))
    }

    pub fn delete_profile(&mut self, scope: &Scope, pin: &str) -> Result<(), Error> {
        self.delete_profile_with_private_connections(scope, pin)
            .map(|_| ())
    }
    /// Return only the private connection records removed by this transaction so
    /// the native host can prune their separate vault entries after commit.
    pub fn delete_profile_with_private_connections(
        &mut self,
        scope: &Scope,
        pin: &str,
    ) -> Result<Vec<IntegrationConnection>, Error> {
        let current = self.profile(scope)?.ok_or(Error::Forbidden)?;
        if self
            .profiles(&scope.connection_id, &scope.account_id)?
            .len()
            <= 1
        {
            return Err(Error::InvalidQuery);
        }
        if self.has_profile_pin(scope)? && !self.verify_profile_pin(scope, pin)? {
            return Err(Error::Forbidden);
        }
        let tx = self.db.transaction().map_err(|_| Error::Storage)?;
        tx.execute("INSERT INTO engine_deleted_profile_revisions(connection,account,profile,revision) VALUES(?1,?2,?3,?4) ON CONFLICT(connection,account,profile) DO UPDATE SET revision=MAX(revision,excluded.revision)",params![scope.connection_id,scope.account_id,scope.profile_id,current.scope.revision]).map_err(|_|Error::Storage)?;
        let private = {
            let mut query = tx
                .prepare("SELECT value FROM engine_connections WHERE account=?1")
                .map_err(|_| Error::Storage)?;
            let rows = query
                .query_map([&scope.account_id], |row| row.get::<_, String>(0))
                .map_err(|_| Error::Storage)?;
            let mut private = Vec::new();
            for row in rows {
                let connection: IntegrationConnection =
                    serde_json::from_str(&row.map_err(|_| Error::Storage)?)
                        .map_err(|_| Error::Storage)?;
                if connection.account_id == scope.account_id
                    && connection.private_profile_id.as_deref() == Some(&scope.profile_id)
                {
                    private.push(connection);
                }
            }
            private
        };
        for connection in &private {
            tx.execute(
                "DELETE FROM engine_connections WHERE account=?1 AND id=?2",
                params![scope.account_id, connection.id],
            )
            .map_err(|_| Error::Storage)?;
        }
        for table in [
            "engine_profiles",
            "engine_observations",
            "engine_playback",
            "engine_profile_locks",
            "engine_catalog_cache",
        ] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE connection=?1 AND account=?2 AND profile=?3"),
                params![scope.connection_id, scope.account_id, scope.profile_id],
            )
            .map_err(|_| Error::Storage)?;
        }
        tx.commit().map_err(|_| Error::Storage)?;
        Ok(private)
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    #[test]
    fn recreated_ids_keep_monotonic_authority_and_overflow_is_rejected() {
        let mut store = Store::memory().unwrap();
        for id in ["first", "other"] {
            store
                .save_profile(
                    &ProfileConfiguration::local("local".into(), id.into(), id.into()),
                    None,
                )
                .unwrap();
        }
        let scope =
            ProfileConfiguration::local("local".into(), "first".into(), "First".into()).scope;
        store.delete_profile(&scope, "").unwrap();
        assert_eq!(store.new_profile_revision(&scope).unwrap(), 1);
        let mut recreated =
            ProfileConfiguration::local("local".into(), "first".into(), "Fresh".into());
        assert_eq!(store.save_profile(&recreated, None), Err(Error::Conflict));
        recreated.scope.revision = 1;
        store.save_profile(&recreated, None).unwrap();
        store.delete_profile(&recreated.scope, "").unwrap();
        assert_eq!(store.new_profile_revision(&scope).unwrap(), 2);
        assert_eq!(
            store
                .new_profile_revision(
                    &ProfileConfiguration::local("local".into(), "new".into(), "New".into()).scope
                )
                .unwrap(),
            0
        );
        store
            .db
            .execute(
                "UPDATE engine_deleted_profile_revisions SET revision=?1 WHERE profile='first'",
                [i64::MAX],
            )
            .unwrap();
        assert_eq!(store.new_profile_revision(&scope), Err(Error::Storage));
        assert!(store.profile(&scope).unwrap().is_none());
    }
}
