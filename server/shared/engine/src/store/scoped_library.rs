//! Additive private lookup records. Old ambiguous rows remain in legacy tables.
use super::*;
use crate::{
    library::LibraryMembershipSnapshot,
    origin::{
        MetadataOrigin, ScopedMediaIdentity, metadata_assertions_match, shared_identity,
        verified_storage_identity,
    },
    playback::PlaybackRecord,
    presentation::Card,
};
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Key(pub String, pub String, pub String);
pub(super) struct Saved<T> {
    pub key: Key,
    pub value: T,
    pub added: Option<u64>,
    pub updated: u64,
    pub verified: bool,
}

pub(super) fn init(db: &Connection) -> Result<(), Error> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS engine_scoped_records(connection TEXT NOT NULL,account TEXT NOT NULL,profile TEXT NOT NULL,record_kind TEXT NOT NULL,identity_scope TEXT NOT NULL,kind TEXT NOT NULL,identity_id TEXT NOT NULL,lookup_origin TEXT NOT NULL,lookup_id TEXT NOT NULL,lookup_identity TEXT NOT NULL,title TEXT NOT NULL,value TEXT NOT NULL,added_at_millis INTEGER,updated_at_millis INTEGER NOT NULL,verified INTEGER NOT NULL,PRIMARY KEY(connection,account,profile,record_kind,identity_scope,kind,identity_id),FOREIGN KEY(connection,account,profile) REFERENCES engine_profiles(connection,account,profile) ON DELETE CASCADE);
 CREATE INDEX IF NOT EXISTS engine_scoped_lookup ON engine_scoped_records(connection,account,profile,record_kind,kind,lookup_origin,lookup_id);").map_err(|_|Error::Storage)?;
    let has:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('engine_scoped_records')WHERE name='lookup_identity')",[],|row|row.get(0)).map_err(|_|Error::Storage)?;
    if !has {
        db.execute_batch("ALTER TABLE engine_scoped_records ADD COLUMN lookup_identity TEXT NOT NULL DEFAULT '{}';UPDATE engine_scoped_records SET lookup_identity=COALESCE(json_extract(value,'$.identity'),'{}');").map_err(|_|Error::Storage)?;
    }
    db.execute_batch("CREATE TABLE IF NOT EXISTS engine_verified_lookup_bindings(connection TEXT NOT NULL,account TEXT NOT NULL,profile TEXT NOT NULL,lookup_origin TEXT NOT NULL,kind TEXT NOT NULL,lookup_id TEXT NOT NULL,identity_scope TEXT NOT NULL,identity_id TEXT NOT NULL,lookup_identity TEXT NOT NULL,card TEXT NOT NULL,verified_at INTEGER NOT NULL,PRIMARY KEY(connection,account,profile,lookup_origin,kind,lookup_id,identity_scope,identity_id),FOREIGN KEY(connection,account,profile) REFERENCES engine_profiles(connection,account,profile)ON DELETE CASCADE);
    INSERT OR IGNORE INTO engine_verified_lookup_bindings SELECT connection,account,profile,lookup_origin,kind,lookup_id,identity_scope,identity_id,lookup_identity,CASE WHEN record_kind='favorite' THEN value ELSE json_extract(value,'$.presentation')END,updated_at_millis FROM engine_scoped_records WHERE verified=1 AND identity_scope='shared' AND (record_kind='favorite' OR json_type(value,'$.presentation')='object');").map_err(|_|Error::Storage)?;
    Ok(())
}
fn raw_key(card: &Card) -> Result<Key, Error> {
    Ok(Key(
        if shared_identity(&card.identity)
            && (!card.identity.id.starts_with("simkl:")
                || card
                    .metadata_origin
                    .as_ref()
                    .is_some_and(|origin| origin.provider_id == "simkl"))
        {
            "shared".into()
        } else if let Some(origin) = &card.metadata_origin {
            origin.key()?
        } else {
            "legacy".into()
        },
        card.identity.kind.clone(),
        card.identity.id.clone(),
    ))
}
fn verified_key(card: &Card) -> Result<Key, Error> {
    let mut stored = card.clone();
    stored.identity = verified_storage_identity(card);
    raw_key(&stored)
}
fn now() -> Result<u64, Error> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| Error::Storage)?
        .as_millis()
        .min(u64::MAX as u128) as u64)
}

trait LookupRecord: DeserializeOwned {
    fn card(&self) -> Card;
    fn retarget(self, card: Card) -> Self;
}
impl LookupRecord for Card {
    fn card(&self) -> Card {
        self.clone()
    }
    fn retarget(self, card: Card) -> Self {
        card
    }
}
impl LookupRecord for PlaybackRecord {
    fn card(&self) -> Card {
        PlaybackRecord::card(self)
    }
    fn retarget(mut self, card: Card) -> Self {
        self.identity = card.identity.clone();
        self.parent_identity = card.parent_identity.clone();
        self.metadata_origin = card.metadata_origin.clone();
        self.provider_id = card.provider_id.clone();
        self.title = card.title.clone();
        self.poster = card.poster.clone();
        self.landscape = card.landscape.clone();
        self.presentation = Some(card);
        self
    }
}
fn remember_binding(
    tx: &Connection,
    scope: &Scope,
    key: &Key,
    card: &Card,
    at: u64,
) -> Result<(), Error> {
    if key.0 != "shared" {
        return Ok(());
    }
    let origin = card
        .metadata_origin
        .as_ref()
        .ok_or(Error::InvalidQuery)?
        .key()?;
    tx.execute("INSERT INTO engine_verified_lookup_bindings(connection,account,profile,lookup_origin,kind,lookup_id,identity_scope,identity_id,lookup_identity,card,verified_at)VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)ON CONFLICT(connection,account,profile,lookup_origin,kind,lookup_id,identity_scope,identity_id)DO UPDATE SET lookup_identity=excluded.lookup_identity,card=excluded.card,verified_at=excluded.verified_at",params![scope.connection_id,scope.account_id,scope.profile_id,origin,key.1,card.identity.id,key.0,key.2,serde_json::to_string(&card.identity).map_err(|_|Error::Storage)?,serde_json::to_string(card).map_err(|_|Error::Storage)?,at]).map_err(|_|Error::Storage)?;
    let count:u64=tx.query_row("SELECT count(*) FROM engine_verified_lookup_bindings WHERE connection=?1 AND account=?2 AND profile=?3",params![scope.connection_id,scope.account_id,scope.profile_id],|row|row.get(0)).map_err(|_|Error::Storage)?;
    if count > 100000 {
        tx.execute("DELETE FROM engine_verified_lookup_bindings WHERE rowid IN(SELECT b.rowid FROM engine_verified_lookup_bindings b WHERE b.connection=?1 AND b.account=?2 AND b.profile=?3 AND NOT EXISTS(SELECT 1 FROM engine_scoped_records r WHERE r.connection=b.connection AND r.account=b.account AND r.profile=b.profile AND r.lookup_origin=b.lookup_origin AND r.kind=b.kind AND r.lookup_id=b.lookup_id AND r.identity_scope=b.identity_scope AND r.identity_id=b.identity_id)ORDER BY b.verified_at,b.rowid LIMIT ?4)",params![scope.connection_id,scope.account_id,scope.profile_id,count-100000]).map_err(|_|Error::Storage)?;
    }
    Ok(())
}

impl Store {
    fn scoped_rows<T: DeserializeOwned>(
        &self,
        scope: &Scope,
        record_kind: &str,
        limit: usize,
    ) -> Result<Vec<Saved<T>>, Error> {
        let mut query=self.db.prepare("SELECT identity_scope,kind,identity_id,value,added_at_millis,updated_at_millis,verified FROM engine_scoped_records WHERE connection=?1 AND account=?2 AND profile=?3 AND record_kind=?4 LIMIT ?5").map_err(|_|Error::Storage)?;
        let rows = query
            .query_map(
                params![
                    scope.connection_id,
                    scope.account_id,
                    scope.profile_id,
                    record_kind,
                    limit as u64 + 1
                ],
                |row| {
                    Ok((
                        Key(row.get(0)?, row.get(1)?, row.get(2)?),
                        row.get::<_, String>(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .map_err(|_| Error::Storage)?;
        let result = rows
            .map(|row| {
                let (key, value, added, updated, verified) = row.map_err(|_| Error::Storage)?;
                Ok(Saved {
                    key,
                    value: serde_json::from_str(&value).map_err(|_| Error::Storage)?,
                    added,
                    updated,
                    verified,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        if result.len() > limit {
            return Err(Error::InvalidQuery);
        }
        Ok(result)
    }
    pub(super) fn favorite_records(&self, scope: &Scope) -> Result<Vec<Saved<Card>>, Error> {
        self.library_scope(scope, false)?;
        let mut query=self.db.prepare("SELECT value,added_at_millis FROM engine_favorites WHERE connection=?1 AND account=?2 AND profile=?3 LIMIT 10001").map_err(|_|Error::Storage)?;
        let rows = query
            .query_map(
                params![scope.connection_id, scope.account_id, scope.profile_id],
                |row| Ok((row.get::<_, String>(0)?, row.get(1)?)),
            )
            .map_err(|_| Error::Storage)?;
        let mut records = BTreeMap::new();
        for row in rows {
            let (value, added) = row.map_err(|_| Error::Storage)?;
            let mut card: Card = serde_json::from_str(&value).map_err(|_| Error::Storage)?;
            // Existing data is explicitly legacy; never assign it a current incarnation.
            card.metadata_origin = None;
            let key = raw_key(&card)?;
            card.added_at_millis = added;
            card.progress = None;
            records.insert(
                key.clone(),
                Saved {
                    key,
                    value: card,
                    added,
                    updated: 0,
                    verified: false,
                },
            );
        }
        for mut saved in self.scoped_rows::<Card>(scope, "favorite", 10000)? {
            saved.value.added_at_millis = saved.added;
            saved.value.progress = None;
            records.insert(saved.key.clone(), saved);
        }
        if records.len() > 10000 {
            return Err(Error::InvalidQuery);
        }
        Ok(records.into_values().collect())
    }
    pub(super) fn playback_records(
        &self,
        scope: &Scope,
    ) -> Result<Vec<Saved<PlaybackRecord>>, Error> {
        self.library_scope(scope, false)?;
        let mut query=self.db.prepare("SELECT value FROM engine_playback WHERE connection=?1 AND account=?2 AND profile=?3 LIMIT 50001").map_err(|_|Error::Storage)?;
        let rows = query
            .query_map(
                params![scope.connection_id, scope.account_id, scope.profile_id],
                |row| row.get::<_, String>(0),
            )
            .map_err(|_| Error::Storage)?;
        let mut records = BTreeMap::new();
        for row in rows {
            let mut value: PlaybackRecord = serde_json::from_str(&row.map_err(|_| Error::Storage)?)
                .map_err(|_| Error::Storage)?;
            value.metadata_origin = None;
            if let Some(card) = &mut value.presentation {
                card.metadata_origin = None;
            }
            let key = raw_key(&value.card())?;
            records.insert(
                key.clone(),
                Saved {
                    key,
                    updated: value.updated_at_millis,
                    value,
                    added: None,
                    verified: false,
                },
            );
        }
        for saved in self.scoped_rows::<PlaybackRecord>(scope, "playback", 50000)? {
            if records
                .get(&saved.key)
                .is_none_or(|old| old.updated <= saved.updated)
            {
                records.insert(saved.key.clone(), saved);
            }
        }
        if records.len() > 50000 {
            return Err(Error::InvalidQuery);
        }
        Ok(records.into_values().collect())
    }
    fn exact_scoped_key<T: LookupRecord>(
        &self,
        scope: &Scope,
        record_kind: &str,
        key: &Key,
    ) -> Result<Option<Saved<T>>, Error> {
        let row:Option<(String,Option<u64>,u64,bool)>=self.db.query_row("SELECT value,added_at_millis,updated_at_millis,verified FROM engine_scoped_records WHERE connection=?1 AND account=?2 AND profile=?3 AND record_kind=?4 AND identity_scope=?5 AND kind=?6 AND identity_id=?7",params![scope.connection_id,scope.account_id,scope.profile_id,record_kind,key.0,key.1,key.2],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).optional().map_err(|_|Error::Storage)?;
        row.map(|(value, added, updated, verified)| {
            Ok(Saved {
                key: key.clone(),
                value: serde_json::from_str(&value).map_err(|_| Error::Storage)?,
                added,
                updated,
                verified,
            })
        })
        .transpose()
    }
    fn stored_lookup<T: LookupRecord>(
        &self,
        scope: &Scope,
        record_kind: &str,
        card: &Card,
    ) -> Result<Option<Saved<T>>, Error> {
        self.library_scope(scope, false)?;
        let origin = card.metadata_origin.as_ref().ok_or(Error::InvalidQuery)?;
        let origin_key = origin.key()?;
        let mut query=self.db.prepare("SELECT identity_scope,kind,identity_id,value,added_at_millis,updated_at_millis,verified,NULL FROM engine_scoped_records WHERE connection=?1 AND account=?2 AND profile=?3 AND record_kind=?4 AND kind=?5 AND lookup_origin=?6 AND lookup_id=?7 UNION SELECT r.identity_scope,r.kind,r.identity_id,r.value,r.added_at_millis,r.updated_at_millis,r.verified,b.card FROM engine_scoped_records r JOIN engine_verified_lookup_bindings b ON b.connection=r.connection AND b.account=r.account AND b.profile=r.profile AND b.kind=r.kind AND b.identity_scope=r.identity_scope AND b.identity_id=r.identity_id WHERE r.connection=?1 AND r.account=?2 AND r.profile=?3 AND r.record_kind=?4 AND r.kind=?5 AND b.lookup_origin=?6 AND b.lookup_id=?7 LIMIT 50001").map_err(|_|Error::Storage)?;
        let rows = query
            .query_map(
                params![
                    scope.connection_id,
                    scope.account_id,
                    scope.profile_id,
                    record_kind,
                    card.identity.kind,
                    origin_key,
                    card.identity.id
                ],
                |row| {
                    Ok((
                        Key(row.get(0)?, row.get(1)?, row.get(2)?),
                        row.get::<_, String>(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get::<_, bool>(6)?,
                        row.get::<_, Option<String>>(7)?,
                    ))
                },
            )
            .map_err(|_| Error::Storage)?;
        let desired = verified_key(card)?;
        let mut candidates = BTreeMap::new();
        for row in rows {
            let (key, value, added, updated, verified, binding) =
                row.map_err(|_| Error::Storage)?;
            let mut value: T = serde_json::from_str(&value).map_err(|_| Error::Storage)?;
            if let Some(binding) = binding {
                value = value.retarget(serde_json::from_str(&binding).map_err(|_| Error::Storage)?);
            }
            let saved = Saved {
                key: key.clone(),
                value,
                added,
                updated,
                verified,
            };
            if !metadata_assertions_match(card, &saved.value.card()) {
                continue;
            }
            if key == desired {
                return Ok(Some(saved));
            }
            if !verified || desired.0 != "shared" {
                candidates.insert(key, saved);
            }
        }
        if candidates.len() > 1 {
            return Err(Error::Conflict);
        }
        Ok(candidates.into_values().next())
    }
    pub fn favorite_for_origin(&self, scope: &Scope, card: &Card) -> Result<Option<Card>, Error> {
        Ok(self
            .stored_lookup::<Card>(scope, "favorite", card)?
            .map(|saved| saved.value))
    }
    pub fn playback_for_origin(
        &self,
        scope: &Scope,
        card: &Card,
    ) -> Result<Option<PlaybackRecord>, Error> {
        Ok(self
            .stored_lookup::<PlaybackRecord>(scope, "playback", card)?
            .map(|saved| saved.value))
    }
    pub(super) fn favorite_count(&self, scope: &Scope) -> Result<u64, Error> {
        self.db.query_row(&format!("SELECT (SELECT count(*) FROM engine_favorites WHERE connection=?1 AND account=?2 AND profile=?3)+(SELECT count(*) FROM engine_scoped_records WHERE connection=?1 AND account=?2 AND profile=?3 AND record_kind='favorite')-(SELECT count(*) FROM engine_favorites f JOIN engine_scoped_records s ON s.connection=f.connection AND s.account=f.account AND s.profile=f.profile AND s.record_kind='favorite' AND s.identity_scope='shared' AND s.kind=f.kind AND s.identity_id=f.id WHERE f.connection=?1 AND f.account=?2 AND f.profile=?3 AND {})",legacy_shared_sql("f")),params![scope.connection_id,scope.account_id,scope.profile_id],|row|row.get(0)).map_err(|_|Error::Storage)
    }
    pub(super) fn playback_key(
        &self,
        scope: &Scope,
        key: &Key,
    ) -> Result<Option<PlaybackRecord>, Error> {
        let scoped:Option<String>=self.db.query_row("SELECT value FROM engine_scoped_records WHERE connection=?1 AND account=?2 AND profile=?3 AND record_kind='playback' AND identity_scope=?4 AND kind=?5 AND identity_id=?6",params![scope.connection_id,scope.account_id,scope.profile_id,key.0,key.1,key.2],|row|row.get(0)).optional().map_err(|_|Error::Storage)?;
        let mut result = scoped
            .map(|value| serde_json::from_str::<PlaybackRecord>(&value).map_err(|_| Error::Storage))
            .transpose()?;
        if key.0 == "shared" || key.0 == "legacy" {
            let legacy:Option<String>=self.db.query_row("SELECT value FROM engine_playback WHERE connection=?1 AND account=?2 AND profile=?3 AND kind=?4 AND id=?5",params![scope.connection_id,scope.account_id,scope.profile_id,key.1,key.2],|row|row.get(0)).optional().map_err(|_|Error::Storage)?;
            if let Some(value) = legacy {
                let mut record: PlaybackRecord =
                    serde_json::from_str(&value).map_err(|_| Error::Storage)?;
                record.metadata_origin = None;
                if let Some(card) = &mut record.presentation {
                    card.metadata_origin = None;
                }
                if raw_key(&record.card())? == *key
                    && result
                        .as_ref()
                        .is_none_or(|old| old.updated_at_millis < record.updated_at_millis)
                {
                    result = Some(record);
                }
            }
        }
        Ok(result)
    }
    pub fn library_membership(&self, scope: &Scope) -> Result<LibraryMembershipSnapshot, Error> {
        self.library_scope(scope, false)?;
        if self.favorite_count(scope)? > 10000 {
            return Err(Error::InvalidQuery);
        }
        let mut identities = Vec::new();
        let mut scoped_identities = Vec::new();
        let mut legacy_identities = Vec::new();
        let mut membership_bytes = 0usize;
        let sql = format!(
            "SELECT f.kind,f.id,NULL,json_extract(f.value,'$.identity'),0 FROM engine_favorites f WHERE f.connection=?1 AND f.account=?2 AND f.profile=?3 AND NOT EXISTS(SELECT 1 FROM engine_scoped_records s WHERE s.connection=f.connection AND s.account=f.account AND s.profile=f.profile AND s.record_kind='favorite' AND s.identity_scope='shared' AND s.kind=f.kind AND s.identity_id=f.id AND {}) UNION ALL SELECT kind,identity_id,lookup_origin,lookup_identity,identity_scope='shared' FROM engine_scoped_records WHERE connection=?1 AND account=?2 AND profile=?3 AND record_kind='favorite' ORDER BY 1,2,3 LIMIT 10001",
            legacy_shared_sql("f")
        );
        let mut query = self.db.prepare(&sql).map_err(|_| Error::Storage)?;
        let rows = query
            .query_map(
                params![scope.connection_id, scope.account_id, scope.profile_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, bool>(4)?,
                    ))
                },
            )
            .map_err(|_| Error::Storage)?;
        for row in rows {
            let (kind, id, origin, lookup, shared) = row.map_err(|_| Error::Storage)?;
            membership_bytes = membership_bytes.saturating_add(
                kind.len()
                    + id.len()
                    + lookup.len()
                    + origin.as_ref().map_or(0, |origin| origin.len()),
            );
            if membership_bytes > 16 * 1024 * 1024 {
                return Err(Error::InvalidQuery);
            }
            let lookup: crate::identity::MediaIdentity =
                serde_json::from_str(&lookup).map_err(|_| Error::Storage)?;
            if let Some(origin) = origin {
                if shared {
                    identities.push(crate::identity::MediaIdentity {
                        id,
                        kind,
                        ids: vec![],
                        episode: None,
                        edition: None,
                    });
                }
                scoped_identities.push(ScopedMediaIdentity {
                    identity: lookup,
                    metadata_origin: serde_json::from_str(&origin).map_err(|_| Error::Storage)?,
                });
            } else if shared_identity(&lookup) && !lookup.id.starts_with("simkl:") {
                identities.push(lookup);
            } else {
                legacy_identities.push(lookup);
            }
        }
        let mut aliases=self.db.prepare("SELECT DISTINCT b.lookup_origin,b.lookup_identity FROM engine_verified_lookup_bindings b JOIN engine_scoped_records r ON r.connection=b.connection AND r.account=b.account AND r.profile=b.profile AND r.record_kind='favorite' AND r.kind=b.kind AND r.identity_scope=b.identity_scope AND r.identity_id=b.identity_id WHERE b.connection=?1 AND b.account=?2 AND b.profile=?3 ORDER BY b.lookup_origin,b.lookup_id,b.identity_id LIMIT 100001").map_err(|_|Error::Storage)?;
        let rows = aliases
            .query_map(
                params![scope.connection_id, scope.account_id, scope.profile_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .map_err(|_| Error::Storage)?;
        let mut seen = scoped_identities
            .iter()
            .map(|entry| {
                Ok((
                    entry.metadata_origin.key()?,
                    serde_json::to_string(&entry.identity).map_err(|_| Error::Storage)?,
                ))
            })
            .collect::<Result<std::collections::BTreeSet<_>, Error>>()?;

        for row in rows {
            let (origin, identity) = row.map_err(|_| Error::Storage)?;
            if seen.insert((origin.clone(), identity.clone())) {
                membership_bytes = membership_bytes.saturating_add(origin.len() + identity.len());
                if membership_bytes > 16 * 1024 * 1024 || seen.len() > 100000 {
                    return Err(Error::InvalidQuery);
                }
                scoped_identities.push(ScopedMediaIdentity {
                    metadata_origin: serde_json::from_str(&origin).map_err(|_| Error::Storage)?,
                    identity: serde_json::from_str(&identity).map_err(|_| Error::Storage)?,
                });
            }
        }
        Ok(LibraryMembershipSnapshot {
            identities,
            scoped_identities,
            legacy_identities,
            revision: self.library_revision(scope)?,
            complete: true,
        })
    }
    pub fn save_favorite_verified(&mut self, scope: &Scope, card: &Card) -> Result<(), Error> {
        if card.metadata_origin.is_none() {
            return self.save_favorite(scope, card);
        }
        self.save_scoped_favorite(scope, card, true)
    }
    pub(super) fn save_scoped_favorite(
        &mut self,
        scope: &Scope,
        card: &Card,
        verified: bool,
    ) -> Result<(), Error> {
        self.library_scope(scope, true)?;
        card.validate_library()?;
        let origin = card.metadata_origin.as_ref().ok_or(Error::InvalidQuery)?;
        self.validate_metadata_origin(scope, origin)?;
        let desired = verified_key(card)?;
        let mut existing = self
            .stored_lookup::<Card>(scope, "favorite", card)?
            .filter(|saved| {
                !verified || saved.key == desired || metadata_assertions_match(&saved.value, card)
            });
        if existing.is_none() && !verified {
            existing = self.exact_scoped_key::<Card>(scope, "favorite", &raw_key(card)?)?;
        }
        if verified
            && desired.0 != "shared"
            && self
                .exact_scoped_key::<Card>(scope, "favorite", &desired)?
                .is_some_and(|old| !metadata_assertions_match(&old.value, card))
        {
            return Err(Error::Conflict);
        }
        let key = if verified {
            verified_key(card)?
        } else {
            existing
                .as_ref()
                .map(|saved| saved.key.clone())
                .unwrap_or(raw_key(card)?)
        };
        let mut card = card.clone();
        if !verified {
            if let Some(old) = existing.as_ref() {
                card.identity = old.value.identity.clone();
                card.parent_identity = old.value.parent_identity.clone();
            }
        }
        card.progress = None;
        card.added_at_millis = None;
        card.poster = super::playback::public_art(card.poster.as_deref());
        card.landscape = super::playback::public_art(card.landscape.as_deref());
        card.logo = super::playback::public_art(card.logo.as_deref());
        let prior:Option<Option<u64>>=self.db.query_row("SELECT added_at_millis FROM engine_scoped_records WHERE connection=?1 AND account=?2 AND profile=?3 AND record_kind='favorite' AND identity_scope=?4 AND kind=?5 AND identity_id=?6",params![scope.connection_id,scope.account_id,scope.profile_id,key.0,key.1,key.2],|row|row.get(0)).optional().map_err(|_|Error::Storage)?;
        let legacy: Option<Option<u64>> = if key.0 == "shared" {
            self.db.query_row("SELECT added_at_millis FROM engine_favorites WHERE connection=?1 AND account=?2 AND profile=?3 AND kind=?4 AND id=?5",params![scope.connection_id,scope.account_id,scope.profile_id,key.1,key.2],|row|row.get(0)).optional().map_err(|_|Error::Storage)?
        } else {
            None
        };
        if self.favorite_count(scope)? >= 10000
            && prior.is_none()
            && legacy.is_none()
            && existing.is_none()
        {
            return Err(Error::InvalidQuery);
        }
        let added = if let Some(prior) = prior.or(legacy) {
            prior
        } else if let Some(old) = &existing {
            old.added
        } else {
            Some(now()?)
        };
        let value = serde_json::to_string(&card).map_err(|_| Error::Storage)?;
        let lookup_origin = origin.key()?;
        let retain_verified = verified || existing.as_ref().is_some_and(|saved| saved.verified);
        let tx = self.db.transaction().map_err(|_| Error::Storage)?;
        let deleted = if let Some(old) = existing
            .as_ref()
            .filter(|old| verified && !old.verified && old.key != key)
        {
            tx.execute("DELETE FROM engine_scoped_records WHERE connection=?1 AND account=?2 AND profile=?3 AND record_kind='favorite' AND kind=?4 AND identity_scope=?5 AND identity_id=?6 AND verified=0",params![scope.connection_id,scope.account_id,scope.profile_id,old.key.1,old.key.0,old.key.2]).map_err(|_|Error::Storage)?
        } else {
            0
        };
        let changed=tx.execute("INSERT INTO engine_scoped_records(connection,account,profile,record_kind,identity_scope,kind,identity_id,lookup_origin,lookup_id,lookup_identity,title,value,added_at_millis,updated_at_millis,verified)VALUES(?1,?2,?3,'favorite',?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)ON CONFLICT(connection,account,profile,record_kind,identity_scope,kind,identity_id)DO UPDATE SET lookup_origin=excluded.lookup_origin,lookup_id=excluded.lookup_id,lookup_identity=excluded.lookup_identity,title=excluded.title,value=excluded.value,verified=excluded.verified WHERE value!=excluded.value OR verified!=excluded.verified",params![scope.connection_id,scope.account_id,scope.profile_id,key.0,key.1,key.2,lookup_origin,card.identity.id,serde_json::to_string(&card.identity).map_err(|_|Error::Storage)?,card.title,value,added,now()?,retain_verified]).map_err(|_|Error::Storage)?;
        if verified {
            remember_binding(&tx, scope, &key, &card, now()?)?;
        }
        if changed + deleted > 0 {
            Self::advance_library(&tx, scope)?;
        }
        tx.commit().map_err(|_| Error::Storage)
    }
    pub fn remove_favorite_for_origin(
        &mut self,
        scope: &Scope,
        kind: &str,
        id: &str,
        origin: &MetadataOrigin,
    ) -> Result<(), Error> {
        let card:Card=serde_json::from_value(serde_json::json!({"identity":{"kind":kind,"id":id,"ids":[]},"metadata_origin":origin,"provider_id":"stremio","title":"Stored title","badges":[]})).map_err(|_|Error::InvalidQuery)?;
        self.remove_scoped_favorite(scope, &card, false)
    }
    pub fn remove_favorite_stored(&mut self, scope: &Scope, card: &Card) -> Result<(), Error> {
        if card.metadata_origin.is_none() {
            return self.remove_favorite(scope, &card.identity.kind, &card.identity.id);
        }
        self.remove_scoped_favorite(scope, card, false)
    }
    pub fn remove_favorite_verified(&mut self, scope: &Scope, card: &Card) -> Result<(), Error> {
        if card.metadata_origin.is_none() {
            return self.remove_favorite(scope, &card.identity.kind, &card.identity.id);
        }
        self.validate_metadata_origin(
            scope,
            card.metadata_origin.as_ref().ok_or(Error::InvalidQuery)?,
        )?;
        self.remove_scoped_favorite(scope, card, true)
    }
    fn remove_scoped_favorite(
        &mut self,
        scope: &Scope,
        card: &Card,
        verified: bool,
    ) -> Result<(), Error> {
        self.library_scope(scope, true)?;
        let origin = card.metadata_origin.as_ref().ok_or(Error::InvalidQuery)?;
        origin.validate()?;
        let existing = self.stored_lookup::<Card>(scope, "favorite", card)?;
        let key = if verified {
            verified_key(card)?
        } else if let Some(saved) = existing {
            saved.key
        } else if shared_identity(&card.identity) {
            raw_key(card)?
        } else {
            return Ok(());
        };
        let tx = self.db.transaction().map_err(|_| Error::Storage)?;
        let mut changed=tx.execute("DELETE FROM engine_scoped_records WHERE connection=?1 AND account=?2 AND profile=?3 AND record_kind='favorite' AND identity_scope=?4 AND kind=?5 AND identity_id=?6",params![scope.connection_id,scope.account_id,scope.profile_id,key.0,key.1,key.2]).map_err(|_|Error::Storage)?;
        if key.0 == "shared" {
            changed+=tx.execute("DELETE FROM engine_favorites WHERE connection=?1 AND account=?2 AND profile=?3 AND kind=?4 AND id=?5",params![scope.connection_id,scope.account_id,scope.profile_id,key.1,key.2]).map_err(|_|Error::Storage)?;
        }
        if changed > 0 {
            Self::advance_library(&tx, scope)?;
        }
        tx.commit().map_err(|_| Error::Storage)
    }
    pub fn record_playback_verified(
        &mut self,
        scope: &Scope,
        card: &Card,
        position: u64,
        duration: u64,
        at: u64,
    ) -> Result<(), Error> {
        if card.metadata_origin.is_none() {
            return self.record_playback(scope, card, position, duration, at);
        }
        self.record_scoped_playback(scope, card, position, duration, at, true)
    }
    pub(super) fn record_scoped_playback(
        &mut self,
        scope: &Scope,
        card: &Card,
        position: u64,
        duration: u64,
        at: u64,
        verified: bool,
    ) -> Result<(), Error> {
        self.library_scope(scope, true)?;
        if !matches!(card.identity.kind.as_str(), "movie" | "series")
            || card.identity.id.is_empty()
            || card.identity.id.len() > 512
            || card.title.chars().count() > 512
            || duration > 7 * 24 * 3600 * 1000
            || position > 7 * 24 * 3600 * 1000
            || (duration > 0 && position > duration.saturating_add(60_000))
            || at == 0
        {
            return Err(Error::InvalidQuery);
        }
        if position == 0 {
            return Ok(());
        }
        let origin = card.metadata_origin.as_ref().ok_or(Error::InvalidQuery)?;
        self.validate_metadata_origin(scope, origin)?;
        let desired = verified_key(card)?;
        let mut old = self
            .stored_lookup::<PlaybackRecord>(scope, "playback", card)?
            .filter(|saved| {
                !verified
                    || saved.key == desired
                    || metadata_assertions_match(&saved.value.card(), card)
            });
        if old.is_none() && !verified {
            old = self.exact_scoped_key::<PlaybackRecord>(scope, "playback", &raw_key(card)?)?;
        }
        if verified
            && desired.0 != "shared"
            && self
                .exact_scoped_key::<PlaybackRecord>(scope, "playback", &desired)?
                .is_some_and(|old| !metadata_assertions_match(&old.value.card(), card))
        {
            return Err(Error::Conflict);
        }
        let key = if verified {
            verified_key(card)?
        } else {
            old.as_ref()
                .map(|old| old.key.clone())
                .unwrap_or(raw_key(card)?)
        };
        let previous = self
            .playback_key(scope, &key)?
            .or_else(|| old.as_ref().map(|saved| saved.value.clone()));
        let completed = duration > 0 && position.saturating_mul(100) >= duration.saturating_mul(95);
        let mut presentation = card.clone();
        if !verified {
            if let Some(saved) = old.as_ref() {
                presentation.identity = saved.value.identity.clone();
                presentation.parent_identity = saved.value.parent_identity.clone();
            }
        }
        presentation.poster = super::playback::public_art(presentation.poster.as_deref());
        presentation.landscape = super::playback::public_art(presentation.landscape.as_deref());
        presentation.logo = super::playback::public_art(presentation.logo.as_deref());
        presentation.progress = None;
        presentation.added_at_millis = None;
        let count = match &previous {
            None => Some(u64::from(completed)),
            Some(old) => old.times_watched.map(|count| {
                if completed
                    && !(old.duration_millis > 0
                        && old.position_millis.saturating_mul(100)
                            >= old.duration_millis.saturating_mul(95))
                {
                    count.saturating_add(1)
                } else {
                    count
                }
            }),
        };
        let record = PlaybackRecord {
            metadata_origin: presentation.metadata_origin.clone(),
            times_watched: count,
            identity: presentation.identity.clone(),
            parent_identity: presentation.parent_identity.clone(),
            provider_id: presentation.provider_id.clone(),
            title: presentation.title.clone(),
            poster: presentation.poster.clone(),
            landscape: presentation.landscape.clone(),
            presentation: Some(presentation),
            position_millis: position,
            duration_millis: duration,
            updated_at_millis: at,
            completed_at_millis: if completed {
                Some(at)
            } else {
                previous.and_then(|old| old.completed_at_millis)
            },
        };
        let tx = self.db.transaction().map_err(|_| Error::Storage)?;
        let lookup_origin = origin.key()?;
        if let Some(old) = old
            .as_ref()
            .filter(|old| verified && !old.verified && old.key != key)
        {
            tx.execute("DELETE FROM engine_scoped_records WHERE connection=?1 AND account=?2 AND profile=?3 AND record_kind='playback' AND kind=?4 AND identity_scope=?5 AND identity_id=?6 AND verified=0",params![scope.connection_id,scope.account_id,scope.profile_id,old.key.1,old.key.0,old.key.2]).map_err(|_|Error::Storage)?;
        }
        tx.execute("INSERT INTO engine_scoped_records(connection,account,profile,record_kind,identity_scope,kind,identity_id,lookup_origin,lookup_id,lookup_identity,title,value,updated_at_millis,verified)VALUES(?1,?2,?3,'playback',?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)ON CONFLICT(connection,account,profile,record_kind,identity_scope,kind,identity_id)DO UPDATE SET lookup_origin=excluded.lookup_origin,lookup_id=excluded.lookup_id,lookup_identity=excluded.lookup_identity,title=excluded.title,value=excluded.value,updated_at_millis=excluded.updated_at_millis,verified=excluded.verified",params![scope.connection_id,scope.account_id,scope.profile_id,key.0,key.1,key.2,lookup_origin,card.identity.id,serde_json::to_string(&record.identity).map_err(|_|Error::Storage)?,card.title,serde_json::to_string(&record).map_err(|_|Error::Storage)?,at,verified||old.as_ref().is_some_and(|old|old.verified)]).map_err(|_|Error::Storage)?;
        if verified {
            remember_binding(
                &tx,
                scope,
                &key,
                record.presentation.as_ref().ok_or(Error::Storage)?,
                at,
            )?;
        }
        tx.commit().map_err(|_| Error::Storage)
    }
}

pub(super) fn parent_key(saved: &Saved<PlaybackRecord>) -> Result<Option<Key>, Error> {
    let Some(parent) = &saved.value.parent_identity else {
        return Ok(None);
    };
    let mut card = saved.value.card();
    card.identity = parent.clone();
    card.parent_identity = None;
    Ok(Some(if saved.verified {
        verified_key(&card)?
    } else {
        raw_key(&card)?
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (Store, Scope, MetadataOrigin, MetadataOrigin) {
        let mut store = Store::memory().unwrap();
        let profile = ProfileConfiguration::local("local".into(), "p".into(), "Profile".into());
        store.save_profile(&profile, None).unwrap();
        for id in ["addon:a", "addon:b"] {
            store
                .install(
                    "p",
                    &IntegrationConnection {
                        id: id.into(),
                        plugin_id: "stremio".into(),
                        account_id: "local".into(),
                        private_profile_id: None,
                        name: id.into(),
                        enabled: true,
                        revision: 1,
                        capabilities: vec![
                            crate::capabilities::Capability::Search,
                            crate::capabilities::Capability::Metadata,
                        ],
                    },
                )
                .unwrap();
        }
        let scope = store.profile(&profile.scope).unwrap().unwrap().scope;
        let a = store.metadata_origin(&scope, "addon:a").unwrap();
        let b = store.metadata_origin(&scope, "addon:b").unwrap();
        (store, scope, a, b)
    }
    fn card(origin: Option<&MetadataOrigin>, id: &str, title: &str, imdb: Option<&str>) -> Card {
        serde_json::from_value(serde_json::json!({"identity":{"kind":"movie","id":id,"ids":imdb.map(|id|vec![serde_json::json!({"namespace":"imdb","value":id})]).unwrap_or_default()},"provider_id":"stremio","metadata_origin":origin,"title":title,"badges":[]})).unwrap()
    }
    #[test]
    fn colliding_private_ids_keep_favorites_progress_membership_and_statistics_separate() {
        let (mut store, scope, a, b) = setup();
        let ca = card(Some(&a), "same", "Alpha", None);
        let cb = card(Some(&b), "same", "Beta", None);
        let legacy = card(None, "same", "Legacy", None);
        for c in [&ca, &cb, &legacy] {
            store.save_favorite(&scope, c).unwrap();
        }
        assert_eq!(store.favorites(&scope, 0, 100).unwrap().cards.len(), 3);
        let membership = store.library_membership(&scope).unwrap();
        assert!(membership.identities.is_empty());
        assert_eq!(membership.legacy_identities.len(), 1);
        assert_eq!(membership.scoped_identities.len(), 2);
        store.record_playback(&scope, &ca, 20, 100, 10).unwrap();
        store.record_playback(&scope, &cb, 100, 100, 20).unwrap();
        assert_eq!(
            store
                .playback_for_origin(&scope, &ca)
                .unwrap()
                .unwrap()
                .position_millis,
            20
        );
        assert_eq!(
            store
                .playback_for_origin(&scope, &cb)
                .unwrap()
                .unwrap()
                .position_millis,
            100
        );
        assert!(store.playback(&scope, "movie", "same").unwrap().is_none());
        let entries = store.library_entries(&scope).unwrap();
        for entry in &entries {
            assert_eq!(entry.statistics.metadata_origin, entry.card.metadata_origin);
        }
        assert!(
            !entries
                .iter()
                .find(|e| e.card.title == "Alpha")
                .unwrap()
                .statistics
                .watched
        );
        assert!(
            entries
                .iter()
                .find(|e| e.card.title == "Beta")
                .unwrap()
                .statistics
                .watched
        );
        assert!(
            !entries
                .iter()
                .find(|e| e.card.title == "Legacy")
                .unwrap()
                .statistics
                .watched
        );
        store.remove_favorite(&scope, "movie", "same").unwrap();
        assert_eq!(
            store
                .library_membership(&scope)
                .unwrap()
                .scoped_identities
                .len(),
            2
        );
    }
    #[test]
    fn external_aliases_need_verified_metadata_and_keep_issuing_lookup_origin() {
        let (mut store, scope, a, b) = setup();
        let ca = card(Some(&a), "private-a", "Alpha", Some("tt1234567"));
        let cb = card(Some(&b), "private-b", "Beta", Some("tt1234567"));
        store.save_favorite(&scope, &ca).unwrap();
        assert_eq!(
            store
                .library_membership(&scope)
                .unwrap()
                .scoped_identities
                .len(),
            1
        );
        assert!(
            store
                .library_membership(&scope)
                .unwrap()
                .identities
                .is_empty()
        );
        store.save_favorite_verified(&scope, &ca).unwrap();
        store.save_favorite_verified(&scope, &cb).unwrap();
        let membership = store.library_membership(&scope).unwrap();
        assert_eq!(membership.identities.len(), 1);
        assert_eq!(membership.identities[0].id, "tt1234567");
        assert_eq!(membership.scoped_identities.len(), 2);
        assert!(
            membership
                .scoped_identities
                .iter()
                .any(|entry| entry.identity.id == "private-b")
        );
        assert!(
            membership
                .scoped_identities
                .iter()
                .any(|entry| entry.identity.id == "private-a")
        );
        let saved = store.favorites(&scope, 0, 100).unwrap().cards;
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].identity.id, "private-b");
        assert_eq!(saved[0].metadata_origin.as_ref(), Some(&b));
        store
            .record_playback_verified(&scope, &ca, 10, 100, 10)
            .unwrap();
        store
            .record_playback_verified(&scope, &ca, 100, 100, 20)
            .unwrap();
        store
            .record_playback_verified(&scope, &cb, 10, 100, 30)
            .unwrap();
        store
            .record_playback_verified(&scope, &cb, 100, 100, 40)
            .unwrap();
        assert_eq!(store.history(&scope, 100).unwrap().len(), 1);
        let restored_a = store.playback_for_origin(&scope, &ca).unwrap().unwrap();
        assert_eq!(restored_a.identity.id, "private-a");
        assert_eq!(restored_a.metadata_origin.as_ref(), Some(&a));
        assert_eq!(restored_a.times_watched, Some(2));
        assert_eq!(
            store
                .favorite_for_origin(&scope, &ca)
                .unwrap()
                .unwrap()
                .identity
                .id,
            "private-a"
        );
        assert_eq!(
            store.history(&scope, 100).unwrap()[0].times_watched,
            Some(2)
        );
        store.remove_favorite_verified(&scope, &ca).unwrap();
        assert!(store.favorites(&scope, 0, 100).unwrap().cards.is_empty());
    }
    #[test]
    fn source_updates_do_not_hide_saved_data_and_id_recreation_cannot_inherit_it() {
        let (mut store, mut scope, a, b) = setup();
        let ca = card(Some(&a), "same", "Alpha", None);
        let cb = card(Some(&b), "same", "Beta", None);
        store.save_favorite(&scope, &ca).unwrap();
        store.record_playback(&scope, &ca, 20, 100, 10).unwrap();
        let mut connection = store
            .connections("local")
            .unwrap()
            .into_iter()
            .find(|c| c.id == "addon:a")
            .unwrap();
        connection.enabled = false;
        connection.revision = 2;
        store
            .db
            .execute(
                "UPDATE engine_connections SET value=?2 WHERE id=?1",
                params![connection.id, serde_json::to_string(&connection).unwrap()],
            )
            .unwrap();
        assert_eq!(store.favorites(&scope, 0, 100).unwrap().cards.len(), 1);
        assert_eq!(store.history(&scope, 100).unwrap().len(), 1);
        assert!(store.save_favorite(&scope, &ca).is_err());
        store.remove_favorite_stored(&scope, &ca).unwrap();
        assert!(store.favorites(&scope, 0, 100).unwrap().cards.is_empty());
        store.remove_connection("addon:a").unwrap();
        scope = store.profile(&scope).unwrap().unwrap().scope;
        assert!(store.playback_for_origin(&scope, &ca).unwrap().is_some());
        connection.enabled = true;
        store.install("p", &connection).unwrap();
        scope = store.profile(&scope).unwrap().unwrap().scope;
        let fresh = store.metadata_origin(&scope, "addon:a").unwrap();
        assert_ne!(fresh, a);
        assert!(store.record_playback(&scope, &ca, 30, 100, 20).is_err());
        let fresh_card = card(Some(&fresh), "same", "Replacement", None);
        store.save_favorite(&scope, &fresh_card).unwrap();
        store
            .record_playback(&scope, &fresh_card, 50, 100, 30)
            .unwrap();
        store.save_favorite(&scope, &cb).unwrap();
        assert_eq!(store.history(&scope, 100).unwrap().len(), 2);
        assert_eq!(
            store
                .playback_for_origin(&scope, &ca)
                .unwrap()
                .unwrap()
                .position_millis,
            20
        );
        assert_eq!(
            store
                .playback_for_origin(&scope, &fresh_card)
                .unwrap()
                .unwrap()
                .position_millis,
            50
        );
    }
    #[test]
    fn membership_does_not_deserialize_provider_cards() {
        let (mut store, scope, a, _) = setup();
        let ca = card(Some(&a), "same", "Alpha", None);
        store.save_favorite(&scope, &ca).unwrap();
        store
            .db
            .execute(
                "UPDATE engine_scoped_records SET value='not-json' WHERE record_kind='favorite'",
                [],
            )
            .unwrap();
        let membership = store.library_membership(&scope).unwrap();
        assert_eq!(membership.scoped_identities.len(), 1);
        assert_eq!(membership.scoped_identities[0].identity.id, "same");
    }
    #[test]
    fn disappearing_alias_does_not_move_shared_history_or_overwrite_private_constraints() {
        let (mut store, scope, a, _) = setup();
        let old = card(Some(&a), "same", "Aliased", Some("tt1234567"));
        store.save_favorite_verified(&scope, &old).unwrap();
        store
            .record_playback_verified(&scope, &old, 100, 100, 10)
            .unwrap();
        let fresh = card(Some(&a), "same", "Now private", None);
        store.save_favorite_verified(&scope, &fresh).unwrap();
        store
            .record_playback_verified(&scope, &fresh, 20, 100, 20)
            .unwrap();
        assert_eq!(store.favorites(&scope, 0, 100).unwrap().cards.len(), 2);
        assert_eq!(store.history(&scope, 100).unwrap().len(), 2);
        assert_eq!(
            store
                .playback_for_origin(&scope, &old)
                .unwrap()
                .unwrap()
                .times_watched,
            Some(1)
        );
        assert_eq!(
            store
                .playback_for_origin(&scope, &fresh)
                .unwrap()
                .unwrap()
                .times_watched,
            Some(0)
        );
        let mut constrained = card(Some(&a), "private-only", "Constrained", None);
        constrained.identity.ids.push(crate::identity::ExternalId {
            namespace: "tmdb".into(),
            value: "1".into(),
        });
        store.save_favorite(&scope, &constrained).unwrap();
        store
            .record_playback(&scope, &constrained, 90, 100, 30)
            .unwrap();
        let mut conflicting = constrained.clone();
        conflicting.identity.ids[0].value = "2".into();
        assert!(matches!(
            store.save_favorite_verified(&scope, &conflicting),
            Err(Error::Conflict)
        ));
        assert!(matches!(
            store.record_playback_verified(&scope, &conflicting, 10, 100, 40),
            Err(Error::Conflict)
        ));
        assert!(
            store
                .favorite_for_origin(&scope, &conflicting)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn fresh_remapping_never_promotes_a_saved_record_with_a_different_expected_identity() {
        let (mut store, scope, a, _) = setup();
        let old = card(Some(&a), "same", "Original", Some("tt1234567"));
        let fresh = card(Some(&a), "same", "Replacement", Some("tt7654321"));
        store.save_favorite(&scope, &old).unwrap();
        store.record_playback(&scope, &old, 100, 100, 10).unwrap();
        store.save_favorite_verified(&scope, &fresh).unwrap();
        store
            .record_playback_verified(&scope, &fresh, 20, 100, 20)
            .unwrap();
        assert_eq!(store.favorites(&scope, 0, 100).unwrap().cards.len(), 2);
        assert_eq!(store.history(&scope, 100).unwrap().len(), 2);
        assert_eq!(
            store
                .playback_for_origin(&scope, &old)
                .unwrap()
                .unwrap()
                .times_watched,
            Some(1)
        );
        assert_eq!(
            store
                .playback_for_origin(&scope, &fresh)
                .unwrap()
                .unwrap()
                .times_watched,
            Some(0)
        );
        assert_eq!(
            store
                .favorite_for_origin(&scope, &old)
                .unwrap()
                .unwrap()
                .title,
            "Original"
        );
        store.remove_favorite_stored(&scope, &old).unwrap();
        assert_eq!(
            store.favorites(&scope, 0, 100).unwrap().cards[0].title,
            "Replacement"
        );
    }
    #[test]
    fn private_incarnation_and_scoped_records_reopen_without_guessing_legacy_origins() {
        let path = std::env::temp_dir().join(format!(
            "viptv-origin-store-{}-{}.sqlite",
            std::process::id(),
            now().unwrap()
        ));
        let mut store = Store::open(&path).unwrap();
        let profile = ProfileConfiguration::local("local".into(), "p".into(), "Profile".into());
        store.save_profile(&profile, None).unwrap();
        let connection = IntegrationConnection {
            id: "addon:a".into(),
            plugin_id: "stremio".into(),
            account_id: "local".into(),
            private_profile_id: None,
            name: "Fixture".into(),
            enabled: true,
            revision: 1,
            capabilities: vec![crate::capabilities::Capability::Metadata],
        };
        store.install("p", &connection).unwrap();
        let scope = store.profile(&profile.scope).unwrap().unwrap().scope;
        let origin = store.metadata_origin(&scope, "addon:a").unwrap();
        let current = card(Some(&origin), "same", "Current", None);
        let legacy = card(None, "same", "Legacy", Some("tt1234567"));
        store.save_favorite(&scope, &current).unwrap();
        store.save_favorite(&scope, &legacy).unwrap();
        store
            .record_playback(&scope, &current, 20, 100, 10)
            .unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.metadata_origin(&scope, "addon:a").unwrap(), origin);
        let membership = store.library_membership(&scope).unwrap();
        assert_eq!(membership.legacy_identities[0].id, "same");
        assert_eq!(membership.scoped_identities[0].metadata_origin, origin);
        assert_eq!(store.favorites(&scope, 0, 100).unwrap().cards.len(), 2);
        assert!(
            store
                .playback_for_origin(&scope, &current)
                .unwrap()
                .is_some()
        );
        drop(store);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn reserved_namespaces_and_noncanonical_episode_spellings_stay_scoped() {
        let (mut store, scope, a, _) = setup();
        let fake = card(Some(&a), "simkl:movie:10", "Private reserved claim", None);
        store.save_favorite_verified(&scope, &fake).unwrap();
        let snapshot = store.library_membership(&scope).unwrap();
        assert!(snapshot.identities.is_empty());
        assert_eq!(snapshot.scoped_identities.len(), 1);
        let legacy = card(None, "simkl:movie:10", "Ambiguous legacy", None);
        store.save_favorite(&scope, &legacy).unwrap();
        let snapshot = store.library_membership(&scope).unwrap();
        assert!(snapshot.identities.is_empty());
        assert_eq!(snapshot.legacy_identities.len(), 1);
        assert_eq!(store.favorites(&scope, 0, 100).unwrap().cards.len(), 2);
        let mut padded = card(Some(&a), "tt1234567:01:02", "Padded episode", None);
        padded.identity.kind = "series".into();
        padded.identity.episode = Some(crate::identity::EpisodeCoordinates {
            order: "aired".into(),
            season: Some(1),
            episode: 2,
        });
        assert!(!shared_identity(&padded.identity));
        store.save_favorite_verified(&scope, &padded).unwrap();
        assert!(
            store
                .library_membership(&scope)
                .unwrap()
                .identities
                .is_empty()
        );
    }
    #[test]
    fn same_lookup_remapped_to_two_verified_titles_has_two_assertion_specific_memberships() {
        let (mut store, scope, a, _) = setup();
        let first = card(Some(&a), "same", "First", Some("tt1234567"));
        let second = card(Some(&a), "same", "Second", Some("tt7654321"));
        store.save_favorite_verified(&scope, &first).unwrap();
        store.save_favorite_verified(&scope, &second).unwrap();
        let membership = store.library_membership(&scope).unwrap();
        assert_eq!(membership.identities.len(), 2);
        assert_eq!(membership.scoped_identities.len(), 2);
        assert!(
            membership
                .scoped_identities
                .iter()
                .any(|entry| entry.identity.ids == first.identity.ids)
        );
        assert!(
            membership
                .scoped_identities
                .iter()
                .any(|entry| entry.identity.ids == second.identity.ids)
        );
        let unknown = card(Some(&a), "same", "Unknown", None);
        assert!(matches!(
            store.favorite_for_origin(&scope, &unknown),
            Err(Error::Conflict)
        ));
        assert_eq!(
            store
                .favorite_for_origin(&scope, &first)
                .unwrap()
                .unwrap()
                .title,
            "First"
        );
        assert_eq!(
            store
                .favorite_for_origin(&scope, &second)
                .unwrap()
                .unwrap()
                .title,
            "Second"
        );
        store.remove_connection("addon:a").unwrap();
        let scope = store.profile(&scope).unwrap().unwrap().scope;
        store.remove_favorite_stored(&scope, &first).unwrap();
        assert_eq!(store.favorites(&scope, 0, 100).unwrap().cards.len(), 1);
        assert_eq!(
            store.favorites(&scope, 0, 100).unwrap().cards[0].title,
            "Second"
        );
    }
}

// Alias is a compile-time table identifier, never request input.
pub(super) fn legacy_shared_sql(alias: &str) -> String {
    format!(
        "({alias}.id NOT LIKE 'simkl:%' AND (instr({alias}.id,':')=0 OR ({alias}.kind='series' AND json_extract({alias}.value,'$.identity.episode.order')='aired' AND {alias}.id=substr({alias}.id,1,instr({alias}.id,':')-1)||':'||json_extract({alias}.value,'$.identity.episode.season')||':'||json_extract({alias}.value,'$.identity.episode.episode'))))"
    )
}
