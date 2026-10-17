//! Origin-aware personal records. Legacy rows are never attributed to an installed connector.
use super::*;
use std::collections::BTreeMap;
use viptv_engine::{
    continuation::{ContinuationEntry, ContinuationStatus, NextEpisodeResult},
    library::{Entry, LibraryMembershipSnapshot, LibraryStatistics},
    origin::{
        metadata_assertions_match, shared_identity, verified_storage_identity, ScopedMediaIdentity,
    },
    playback::PlaybackRecord,
    presentation::Card,
};

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct Key(pub String, pub String, pub String);
#[derive(Clone)]
pub(crate) struct Saved<T> {
    pub key: Key,
    pub value: T,
    pub added: Option<u64>,
    pub updated: u64,
    pub verified: bool,
}
fn invalid() -> ApiError {
    "Invalid scoped library record".into()
}
fn conflict() -> ApiError {
    ApiError(
        StatusCode::CONFLICT,
        "Saved metadata is ambiguous. Select the exact saved title.".into(),
    )
}
fn encode<T: serde::Serialize>(v: &T) -> Result<String, ApiError> {
    serde_json::to_string(v).map_err(|_| invalid())
}
fn decode<T: serde::de::DeserializeOwned>(v: &str) -> Result<T, ApiError> {
    serde_json::from_str(v).map_err(|_| invalid())
}
fn now() -> u64 {
    util::now_millis().max(0) as u64
}
pub(crate) fn raw_key(card: &Card) -> Result<Key, ApiError> {
    Ok(Key(
        if shared_identity(&card.identity)
            && (!card.identity.id.starts_with("simkl:")
                || card
                    .metadata_origin
                    .as_ref()
                    .is_some_and(|o| o.provider_id == "simkl"))
        {
            "shared".into()
        } else if let Some(o) = &card.metadata_origin {
            o.key().map_err(|_| invalid())?
        } else {
            "legacy".into()
        },
        card.identity.kind.clone(),
        card.identity.id.clone(),
    ))
}
fn verified_key(card: &Card) -> Result<Key, ApiError> {
    let mut c = card.clone();
    c.identity = verified_storage_identity(card);
    raw_key(&c)
}
// Existing account-owned import receipts retain the legacy canonical alias contract.
fn imported_key(db: &Connection, profile: i64, card: &Card) -> Result<Option<Key>, ApiError> {
    if !card
        .metadata_origin
        .as_ref()
        .is_some_and(|origin| origin.provider_id == "simkl")
    {
        return Ok(None);
    }
    let lookup = card.parent_identity.as_ref().unwrap_or(&card.identity);
    let row:Option<(String,String,String)>=db.query_row("SELECT local_kind,local_id,card FROM simkl_import_entries WHERE profile_id=?1 AND remote_kind=?2 AND remote_id=?3",params![profile,lookup.kind,lookup.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(db_error)?;
    let Some((kind, id, stored)) = row else {
        return Ok(None);
    };
    let mut imported: Card = decode(&stored)?;
    imported.identity.id = lookup.id.clone();
    imported.identity.kind = lookup.kind.clone();
    imported.metadata_origin = card.metadata_origin.clone();
    let mut expected = card.clone();
    expected.identity = lookup.clone();
    expected.parent_identity = None;
    if !metadata_assertions_match(&imported, &expected) {
        return Err(conflict());
    }
    let mut identity = lookup.clone();
    identity.kind = kind;
    identity.id = id;
    if let Some(episode) = &card.identity.episode {
        let season = episode
            .season
            .filter(|_| episode.order == "aired" && episode.episode > 0)
            .ok_or_else(invalid)?;
        identity.id = format!("{}:{season}:{}", identity.id, episode.episode);
        identity.episode = Some(episode.clone());
    }
    if !shared_identity(&identity) {
        return Err(conflict());
    }
    Ok(Some(Key("shared".into(), identity.kind, identity.id)))
}
fn verified_key_db(db: &Connection, profile: i64, card: &Card) -> Result<Key, ApiError> {
    Ok(imported_key(db, profile, card)?.unwrap_or(verified_key(card)?))
}
fn promotable_simkl(card: &Card, key: &Key) -> bool {
    key.0 == "shared"
        && card
            .metadata_origin
            .as_ref()
            .is_some_and(|o| o.provider_id == "simkl")
        && card.identity.id.starts_with("simkl:")
        && shared_identity(&card.identity)
}
pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS engine_scoped_records(profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,record_kind TEXT NOT NULL,identity_scope TEXT NOT NULL,kind TEXT NOT NULL,identity_id TEXT NOT NULL,lookup_origin TEXT NOT NULL,lookup_id TEXT NOT NULL,lookup_identity TEXT NOT NULL,value TEXT NOT NULL,added_at_millis INTEGER,updated_at_millis INTEGER NOT NULL,verified INTEGER NOT NULL,PRIMARY KEY(profile_id,record_kind,identity_scope,kind,identity_id));CREATE INDEX IF NOT EXISTS engine_scoped_lookup ON engine_scoped_records(profile_id,record_kind,kind,lookup_origin,lookup_id);
 CREATE INDEX IF NOT EXISTS engine_scoped_home_favorites ON engine_scoped_records(profile_id,added_at_millis DESC,json_extract(value,'$.title') COLLATE NOCASE,kind,identity_id) WHERE record_kind='favorite';
 CREATE TABLE IF NOT EXISTS engine_verified_lookup_bindings(profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,lookup_origin TEXT NOT NULL,kind TEXT NOT NULL,lookup_id TEXT NOT NULL,identity_scope TEXT NOT NULL,identity_id TEXT NOT NULL,lookup_identity TEXT NOT NULL,card TEXT NOT NULL,verified_at INTEGER NOT NULL,PRIMARY KEY(profile_id,lookup_origin,kind,lookup_id,identity_scope,identity_id));
 CREATE TABLE IF NOT EXISTS engine_scoped_queue_hidden(profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,identity_scope TEXT NOT NULL,kind TEXT NOT NULL,identity_id TEXT NOT NULL,PRIMARY KEY(profile_id,identity_scope,kind,identity_id));
 CREATE TABLE IF NOT EXISTS engine_scoped_next(profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,selector TEXT NOT NULL,configuration TEXT NOT NULL,result TEXT NOT NULL,updated_at_millis INTEGER NOT NULL,PRIMARY KEY(profile_id,selector));")?;
    for table in [
        "engine_scoped_records",
        "engine_scoped_next",
        "engine_scoped_queue_hidden",
    ] {
        for (op, row) in [("INSERT", "NEW"), ("UPDATE", "NEW"), ("DELETE", "OLD")] {
            db.execute_batch(&format!("CREATE TRIGGER IF NOT EXISTS engine_library_{table}_{op} AFTER {op} ON {table} BEGIN INSERT INTO engine_library_revisions(profile_id,revision) SELECT {row}.profile_id,1 WHERE EXISTS(SELECT 1 FROM profiles WHERE id={row}.profile_id) ON CONFLICT(profile_id) DO UPDATE SET revision=revision+1; END;"))?;
        }
    }
    Ok(())
}
fn rows<T: serde::de::DeserializeOwned>(
    db: &Connection,
    profile: i64,
    kind: &str,
    limit: usize,
) -> Result<Vec<Saved<T>>, ApiError> {
    let mut q=db.prepare("SELECT identity_scope,kind,identity_id,value,added_at_millis,updated_at_millis,verified FROM engine_scoped_records WHERE profile_id=?1 AND record_kind=?2 ORDER BY identity_scope,kind,identity_id LIMIT ?3").map_err(db_error)?;
    let values = q
        .query_map(params![profile, kind, limit + 1], |r| {
            Ok((
                Key(r.get(0)?, r.get(1)?, r.get(2)?),
                r.get::<_, String>(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    if values.len() > limit {
        return Err("Library exceeds its bounded item limit".into());
    }
    values
        .into_iter()
        .map(|(key, value, added, updated, verified)| {
            Ok(Saved {
                key,
                value: decode(&value)?,
                added,
                updated,
                verified,
            })
        })
        .collect()
}
fn count_db(db: &Connection, profile: i64, kind: &str) -> Result<usize, ApiError> {
    db.query_row(
        "SELECT count(*) FROM engine_scoped_records WHERE profile_id=?1 AND record_kind=?2",
        params![profile, kind],
        |r| r.get(0),
    )
    .map_err(db_error)
}
fn by_key<T: serde::de::DeserializeOwned>(
    db: &Connection,
    profile: i64,
    kind: &str,
    key: &Key,
) -> Result<Option<Saved<T>>, ApiError> {
    let row:Option<(String,Option<u64>,u64,bool)>=db.query_row("SELECT value,added_at_millis,updated_at_millis,verified FROM engine_scoped_records WHERE profile_id=?1 AND record_kind=?2 AND identity_scope=?3 AND kind=?4 AND identity_id=?5",params![profile,kind,key.0,key.1,key.2],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(db_error)?;
    row.map(|(value, added, updated, verified)| {
        Ok(Saved {
            key: key.clone(),
            value: decode(&value)?,
            added,
            updated,
            verified,
        })
    })
    .transpose()
}
fn legacy_playback_key(
    db: &Connection,
    profile: i64,
    key: &Key,
) -> Result<Option<Saved<PlaybackRecord>>, ApiError> {
    if key.0 != "shared" {
        return Ok(None);
    }
    let raw:Option<(String,Option<u64>)>=db.query_row("SELECT json_object('id',p.id,'type',p.type,'name',p.name,'poster',p.poster,'position',p.position,'duration',p.duration,'updated_at',p.updated_at,'context',p.context,'series_id',p.title_id),w.count FROM progress p LEFT JOIN engine_watch_counts w ON w.profile_id=p.profile_id AND w.type=p.type AND w.id=p.id WHERE p.profile_id=?1 AND p.type=?2 AND p.id=?3",params![profile,key.1,key.2],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(db_error)?;
    raw.map(|(raw, count)| {
        let mut item: Value = decode(&raw)?;
        let context: Value = decode(item["context"].as_str().unwrap_or("{}"))?;
        super::history::restore(&mut item, &context);
        crate::library::add_watch_fields(&mut item, &context);
        if let Ok(context) = matching_context(&context) {
            item.as_object_mut()
                .ok_or_else(invalid)?
                .extend(context.as_object().ok_or_else(invalid)?.clone());
        }
        let mut record = super::history::record(&item).ok_or_else(invalid)?;
        record.times_watched = count;
        Ok(Saved {
            key: key.clone(),
            added: None,
            updated: record.updated_at_millis,
            value: record,
            verified: false,
        })
    })
    .transpose()
}

trait Lookup: serde::de::DeserializeOwned {
    fn card(&self) -> Card;
    fn retarget(self, c: Card) -> Self;
}
impl Lookup for Card {
    fn card(&self) -> Card {
        self.clone()
    }
    fn retarget(self, c: Card) -> Self {
        c
    }
}
impl Lookup for PlaybackRecord {
    fn card(&self) -> Card {
        self.card()
    }
    fn retarget(mut self, c: Card) -> Self {
        self.identity = c.identity.clone();
        self.parent_identity = c.parent_identity.clone();
        self.metadata_origin = c.metadata_origin.clone();
        self.provider_id = c.provider_id.clone();
        self.title = c.title.clone();
        self.poster = c.poster.clone();
        self.landscape = c.landscape.clone();
        self.presentation = Some(c);
        self
    }
}
/// Posted assertions select a stored binding; they never create one or grant an alias.
fn lookup<T: Lookup>(
    db: &Connection,
    profile: i64,
    kind: &str,
    card: &Card,
) -> Result<Option<Saved<T>>, ApiError> {
    let origin = card
        .metadata_origin
        .as_ref()
        .ok_or_else(invalid)?
        .key()
        .map_err(|_| invalid())?;
    let mut q=db.prepare("SELECT identity_scope,kind,identity_id,value,added_at_millis,updated_at_millis,verified,NULL FROM engine_scoped_records WHERE profile_id=?1 AND record_kind=?2 AND kind=?3 AND lookup_origin=?4 AND lookup_id=?5 UNION ALL SELECT r.identity_scope,r.kind,r.identity_id,r.value,r.added_at_millis,r.updated_at_millis,r.verified,b.card FROM engine_scoped_records r JOIN engine_verified_lookup_bindings b ON b.profile_id=r.profile_id AND b.kind=r.kind AND b.identity_scope=r.identity_scope AND b.identity_id=r.identity_id WHERE r.profile_id=?1 AND r.record_kind=?2 AND r.kind=?3 AND b.lookup_origin=?4 AND b.lookup_id=?5 LIMIT 100002").map_err(db_error)?;
    let results = q
        .query_map(
            params![profile, kind, card.identity.kind, origin, card.identity.id],
            |r| {
                Ok((
                    Key(r.get(0)?, r.get(1)?, r.get(2)?),
                    r.get::<_, String>(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get::<_, Option<String>>(7)?,
                ))
            },
        )
        .map_err(db_error)?;
    let mut candidates = BTreeMap::new();
    for result in results {
        let (key, value, added, updated, verified, binding) = result.map_err(db_error)?;
        let mut value: T = decode(&value)?;
        if let Some(binding) = binding {
            value = value.retarget(decode(&binding)?)
        }
        if metadata_assertions_match(card, &value.card()) {
            candidates.insert(
                key.clone(),
                Saved {
                    key,
                    value,
                    added,
                    updated,
                    verified,
                },
            );
        }
    }
    if candidates.len() > 1 {
        return Err(conflict());
    }
    Ok(candidates.into_values().next())
}
fn lookup_for_write<T: Lookup>(
    db: &Connection,
    profile: i64,
    kind: &str,
    card: &Card,
    verified: bool,
) -> Result<Option<Saved<T>>, ApiError> {
    let desired = if verified {
        verified_key_db(db, profile, card)?
    } else {
        raw_key(card)?
    };
    // A fresh host proof may select an existing exact storage key even when
    // old aliases for this raw lookup are deliberately retained alongside it.
    let mut old = if verified {
        by_key::<T>(db, profile, kind, &desired)?
    } else {
        None
    };
    if old.is_none() {
        old = lookup::<T>(db, profile, kind, card)?;
    }
    if verified {
        if let Some(saved) = &old {
            let compatible = (desired.0 == "shared" && saved.key == desired)
                || metadata_assertions_match(&saved.value.card(), card);
            if !compatible {
                if saved.key == desired {
                    return Err(conflict());
                }
                // Loss of alias proof is not permission to copy shared A into a private lookup.
                old = None;
            }
        }
    }
    if old.is_none() && (!verified || desired.0 != "shared") {
        if by_key::<T>(db, profile, kind, &desired)?.is_some() {
            return Err(conflict());
        }
    }
    Ok(old)
}
fn validate_current(db: &Connection, app: &App, profile: i64, card: &Card) -> Result<(), ApiError> {
    app.require_profile(db, profile)?;
    let lease = app.request_lease();
    lease.validate(db)?;
    origins::validate_db(
        db,
        lease.principal.account_id().ok_or_else(auth::forbidden)?,
        card.metadata_origin.as_ref().ok_or_else(invalid)?,
    )?;
    origins::validate_assignment_db(
        db,
        &lease,
        card.metadata_origin.as_ref().ok_or_else(invalid)?,
    )?;
    kids::scoped::require_card_db(db, &lease.principal, card)
}
fn evidence<'a>(
    db: &Connection,
    app: &App,
    profile: i64,
    e: &'a origins::MetadataEvidence,
) -> Result<(&'a Card, bool), ApiError> {
    let (card, verified) = match e {
        origins::MetadataEvidence::Verified(c, p) => {
            p.validate(db, &app.request_lease())?;
            (c, true)
        }
        origins::MetadataEvidence::Unverified(c) => (c, false),
    };
    validate_current(db, app, profile, card)?;
    Ok((card, verified))
}
fn remember(db: &Connection, profile: i64, key: &Key, card: &Card) -> Result<(), ApiError> {
    if key.0 != "shared" {
        return Ok(());
    }
    db.execute("INSERT INTO engine_verified_lookup_bindings VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(profile_id,lookup_origin,kind,lookup_id,identity_scope,identity_id) DO UPDATE SET lookup_identity=excluded.lookup_identity,card=excluded.card,verified_at=excluded.verified_at",params![profile,card.metadata_origin.as_ref().ok_or_else(invalid)?.key().map_err(|_|invalid())?,key.1,card.identity.id,key.0,key.2,encode(&card.identity)?,encode(card)?,now()]).map_err(db_error)?;
    let count: u64 = db
        .query_row(
            "SELECT count(*) FROM engine_verified_lookup_bindings WHERE profile_id=?1",
            [profile],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    if count > 100000 {
        return Err("Library aliases exceed their bounded limit".into());
    }
    Ok(())
}
fn store<T: serde::Serialize>(
    db: &Connection,
    profile: i64,
    kind: &str,
    key: &Key,
    card: &Card,
    value: &T,
    added: Option<u64>,
    verified: bool,
) -> Result<(), ApiError> {
    db.execute("INSERT INTO engine_scoped_records VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12) ON CONFLICT(profile_id,record_kind,identity_scope,kind,identity_id) DO UPDATE SET lookup_origin=excluded.lookup_origin,lookup_id=excluded.lookup_id,lookup_identity=excluded.lookup_identity,value=excluded.value,updated_at_millis=excluded.updated_at_millis,verified=excluded.verified",params![profile,kind,key.0,key.1,key.2,card.metadata_origin.as_ref().ok_or_else(invalid)?.key().map_err(|_|invalid())?,card.identity.id,encode(&card.identity)?,encode(value)?,added,now(),verified]).map_err(db_error)?;
    if verified {
        remember(db, profile, key, card)?
    }
    Ok(())
}
fn delete_key(db: &Connection, profile: i64, kind: &str, key: &Key) -> Result<(), ApiError> {
    db.execute("DELETE FROM engine_scoped_records WHERE profile_id=?1 AND record_kind=?2 AND identity_scope=?3 AND kind=?4 AND identity_id=?5",params![profile,kind,key.0,key.1,key.2]).map_err(db_error)?;
    Ok(())
}
fn clean(mut card: Card) -> Card {
    card.progress = None;
    card.added_at_millis = None;
    card
}
pub(crate) fn save_db(
    db: &Connection,
    app: &App,
    profile: i64,
    e: &origins::MetadataEvidence,
) -> Result<(), ApiError> {
    let (card, verified) = evidence(db, app, profile, e)?;
    card.validate_library().map_err(|_| invalid())?;
    // Fresh metadata can change an opaque lookup's binding A -> B. Do not move A's state.
    let old = lookup_for_write::<Card>(db, profile, "favorite", card, verified)?
        .filter(|old| !verified || old.verified || metadata_assertions_match(&old.value, card));
    let key = if verified {
        verified_key_db(db, profile, card)?
    } else {
        old.as_ref()
            .map(|s| s.key.clone())
            .unwrap_or(raw_key(card)?)
    };
    let mut c = clean(card.clone());
    if !verified {
        if let Some(old) = &old {
            c.identity = old.value.identity.clone();
            c.parent_identity = old.value.parent_identity.clone();
        }
    }
    let prior:Option<Option<u64>>=db.query_row("SELECT added_at_millis FROM engine_scoped_records WHERE profile_id=?1 AND record_kind='favorite' AND identity_scope=?2 AND kind=?3 AND identity_id=?4",params![profile,key.0,key.1,key.2],|r|r.get(0)).optional().map_err(db_error)?;
    let legacy: Option<Option<u64>> = if key.0 == "shared" {
        db.query_row(
            "SELECT added_at_millis FROM favorites WHERE profile_id=?1 AND type=?2 AND id=?3",
            params![profile, key.1, key.2],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?
    } else {
        None
    };
    let added = if let Some(added) = prior.or(legacy) {
        added
    } else if let Some(old) = &old {
        old.added
    } else {
        Some(now())
    };
    if count_db(db, profile, "favorite")? >= 10000 && prior.is_none() && old.is_none() {
        return Err("Library exceeds its bounded item limit".into());
    }
    if let Some(old) = old
        .as_ref()
        .filter(|o| verified && o.key != key && (!o.verified || promotable_simkl(card, &o.key)))
    {
        delete_key(db, profile, "favorite", &old.key)?
    }
    if key.0 == "shared" {
        db.execute(
            "DELETE FROM library_removals WHERE profile_id=?1 AND type=?2 AND id=?3",
            params![profile, key.1, key.2],
        )
        .map_err(db_error)?;
    }
    store(
        db,
        profile,
        "favorite",
        &key,
        &c,
        &c,
        added,
        verified || old.is_some_and(|o| o.verified),
    )
}
/// Exact stored selectors also work after connector removal; no legacy delete fallback.
pub(crate) fn remove_db(
    db: &Connection,
    app: &App,
    profile: i64,
    card: &Card,
) -> Result<(), ApiError> {
    app.require_profile(db, profile)?;
    app.request_lease().validate(db)?;
    card.validate_library().map_err(|_| invalid())?;
    card.metadata_origin
        .as_ref()
        .ok_or_else(invalid)?
        .validate()
        .map_err(|_| invalid())?;
    let saved = lookup::<Card>(db, profile, "favorite", card)?;
    let key = if let Some(saved) = &saved {
        kids::scoped::require_card_db(db, &app.identity(), &saved.value)?;
        saved.key.clone()
    } else {
        let key = raw_key(card)?;
        if key.0 != "shared" {
            return Ok(());
        }
        kids::scoped::require_card_db(db, &app.identity(), card)?;
        imported_key(db, profile, card)?.unwrap_or(key)
    };
    delete_key(db, profile, "favorite", &key)?;
    if key.0 == "shared" {
        db.execute(
            "INSERT OR IGNORE INTO library_removals(profile_id,type,id)VALUES(?1,?2,?3)",
            params![profile, key.1, key.2],
        )
        .map_err(db_error)?;
        // Only an intrinsically shared legacy identity or a verified import alias
        // is the same logical backing row. Ambiguous private legacy rows survive.
        let old:Option<String>=db.query_row("SELECT json_object('id',id,'type',type,'name',name,'poster',poster,'engine_favorite',json(context)) FROM favorites WHERE profile_id=?1 AND type=?2 AND id=?3",params![profile,key.1,key.2],|r|r.get(0)).optional().map_err(db_error)?;
        if let Some(old) = old {
            let item: Value = decode(&old)?;
            let public = super::library::card(&item)
                .is_some_and(|card| raw_key(&card).is_ok_and(|legacy| legacy == key));
            if public {
                db.execute(
                    "DELETE FROM favorites WHERE profile_id=?1 AND type=?2 AND id=?3",
                    params![profile, key.1, key.2],
                )
                .map_err(db_error)?;
            }
        }
    }
    Ok(())
}
pub(crate) fn record_db(
    db: &Connection,
    app: &App,
    profile: i64,
    record: &PlaybackRecord,
    e: &origins::MetadataEvidence,
    ended: bool,
) -> Result<(), ApiError> {
    let (card, verified) = evidence(db, app, profile, e)?;
    let duration = record.duration_millis;
    let position = if ended && duration > 0 {
        duration
    } else {
        record.position_millis
    };
    viptv_engine::continuation::validate_identity(&card.identity).map_err(|_| invalid())?;
    if position > 604800000
        || duration > 604800000
        || (duration > 0 && position > duration.saturating_add(60000))
    {
        return Err(invalid());
    }
    if position == 0 && !ended {
        return Ok(());
    }
    let old = lookup_for_write::<PlaybackRecord>(db, profile, "playback", card, verified)?.filter(
        |old| !verified || old.verified || metadata_assertions_match(&old.value.card(), card),
    );
    let key = if verified {
        verified_key_db(db, profile, card)?
    } else {
        old.as_ref()
            .map(|s| s.key.clone())
            .unwrap_or(raw_key(card)?)
    };
    let prior = by_key::<PlaybackRecord>(db, profile, "playback", &key)?
        .or(legacy_playback_key(db, profile, &key)?)
        .or_else(|| old.clone());
    let prior_completed: Option<bool> = if let Some(prior) = &prior {
        db.query_row("SELECT json_extract(value,'$._current_completed') FROM engine_scoped_records WHERE profile_id=?1 AND record_kind='playback' AND identity_scope=?2 AND kind=?3 AND identity_id=?4",params![profile,prior.key.0,prior.key.1,prior.key.2],|r|r.get(0)).optional().map_err(db_error)?.flatten()
    } else {
        None
    };
    let completed =
        ended || (duration > 0 && position.saturating_mul(100) >= duration.saturating_mul(95));
    let at = now();
    let mut c = clean(card.clone());
    if !verified {
        if let Some(old) = &old {
            c.identity = old.value.identity.clone();
            c.parent_identity = old.value.parent_identity.clone();
        }
    }
    let count = match &prior {
        None => Some(u64::from(completed)),
        Some(old) => old.value.times_watched.map(|n| {
            if completed
                && !prior_completed.unwrap_or(
                    old.value.duration_millis > 0
                        && old.value.position_millis.saturating_mul(100)
                            >= old.value.duration_millis.saturating_mul(95),
                )
            {
                n.saturating_add(1)
            } else {
                n
            }
        }),
    };
    let value = PlaybackRecord {
        identity: c.identity.clone(),
        parent_identity: c.parent_identity.clone(),
        metadata_origin: c.metadata_origin.clone(),
        provider_id: c.provider_id.clone(),
        title: c.title.clone(),
        poster: c.poster.clone(),
        landscape: c.landscape.clone(),
        presentation: Some(c.clone()),
        position_millis: position,
        duration_millis: duration,
        updated_at_millis: at,
        completed_at_millis: if completed {
            Some(at)
        } else {
            prior.as_ref().and_then(|o| o.value.completed_at_millis)
        },
        times_watched: count,
    };
    if prior.is_none() && count_db(db, profile, "playback")? >= 50000 {
        return Err("History exceeds its bounded item limit".into());
    }
    if let Some(old) = old
        .as_ref()
        .filter(|o| verified && o.key != key && (!o.verified || promotable_simkl(card, &o.key)))
    {
        delete_key(db, profile, "playback", &old.key)?
    }
    let mut stored = serde_json::to_value(&value).map_err(|_| invalid())?;
    stored["_current_completed"] = Value::Bool(completed);
    store(
        db,
        profile,
        "playback",
        &key,
        &c,
        &stored,
        None,
        verified || old.is_some_and(|o| o.verified),
    )
}
pub(crate) fn playback_db(
    db: &Connection,
    app: &App,
    profile: i64,
) -> Result<Vec<Saved<PlaybackRecord>>, ApiError> {
    let mut records = rows::<PlaybackRecord>(db, profile, "playback", 50000)?;
    records.retain(|s| kids::scoped::require_card_db(db, &app.identity(), &s.value.card()).is_ok());
    Ok(records)
}
pub(crate) fn favorites_db(
    db: &Connection,
    app: &App,
    profile: i64,
) -> Result<Vec<Saved<Card>>, ApiError> {
    let mut records = rows::<Card>(db, profile, "favorite", 10000)?;
    records.retain(|s| kids::scoped::require_card_db(db, &app.identity(), &s.value).is_ok());
    for s in &mut records {
        s.value.added_at_millis = s.added;
        s.value.progress = None;
    }
    Ok(records)
}
pub(crate) fn membership_db(
    db: &Connection,
    app: &App,
    profile: i64,
    result: &mut LibraryMembershipSnapshot,
    mut logical_keys: std::collections::BTreeSet<Key>,
) -> Result<(), ApiError> {
    use viptv_engine::{identity::MediaIdentity, origin::MetadataOrigin};
    let principal = app.identity();
    let mut seen = std::collections::BTreeSet::new();
    let mut bytes = 0usize;
    // Complete membership must not deserialize titles/artwork/metadata for 10k
    // cards. Only the bounded identity columns and optional parent are needed.
    let mut q=db.prepare("SELECT identity_scope,kind,identity_id,lookup_origin,lookup_identity,json_extract(value,'$.parent_identity') FROM engine_scoped_records WHERE profile_id=?1 AND record_kind='favorite' ORDER BY identity_scope,kind,identity_id LIMIT 10001").map_err(db_error)?;
    let mut count = 0;
    for row in q
        .query_map([profile], |r| {
            Ok((
                Key(r.get(0)?, r.get(1)?, r.get(2)?),
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })
        .map_err(db_error)?
    {
        count += 1;
        if count > 10000 {
            return Err("Library exceeds its bounded item limit".into());
        }
        let (key, raw_origin, raw_identity, raw_parent) = row.map_err(db_error)?;
        let origin: MetadataOrigin = decode(&raw_origin)?;
        let identity: MediaIdentity = decode(&raw_identity)?;
        let parent: Option<MediaIdentity> = raw_parent.as_deref().map(decode).transpose()?;
        bytes += raw_origin.len() + raw_identity.len() + raw_parent.as_ref().map_or(0, String::len);
        if bytes > 16 * 1024 * 1024 {
            return Err(invalid());
        }
        if kids::scoped::require_identity_db(db, &principal, &origin, &identity, parent.as_ref())
            .is_err()
        {
            continue;
        }
        logical_keys.insert(key.clone());
        if key.0 == "shared" {
            let mut shared = identity.clone();
            shared.id = key.2;
            shared.kind = key.1;
            shared.ids.clear();
            result.identities.push(shared);
        }
        seen.insert((raw_origin, raw_identity));
        result.scoped_identities.push(ScopedMediaIdentity {
            metadata_origin: origin,
            identity,
        });
    }
    let mut q=db.prepare("SELECT b.lookup_origin,b.lookup_identity,json_extract(b.card,'$.parent_identity') FROM engine_verified_lookup_bindings b JOIN engine_scoped_records r ON r.profile_id=b.profile_id AND r.record_kind='favorite' AND r.kind=b.kind AND r.identity_scope=b.identity_scope AND r.identity_id=b.identity_id WHERE b.profile_id=?1 ORDER BY b.lookup_origin,b.lookup_id,b.identity_id LIMIT 100001").map_err(db_error)?;
    let mut count = 0;
    for row in q
        .query_map([profile], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(db_error)?
    {
        count += 1;
        if count > 100000 {
            return Err(invalid());
        }
        let (raw_origin, raw_identity, raw_parent) = row.map_err(db_error)?;
        bytes += raw_origin.len() + raw_identity.len() + raw_parent.as_ref().map_or(0, String::len);
        if bytes > 16 * 1024 * 1024 {
            return Err(invalid());
        }
        let origin: MetadataOrigin = decode(&raw_origin)?;
        let identity: MediaIdentity = decode(&raw_identity)?;
        let parent: Option<MediaIdentity> = raw_parent.as_deref().map(decode).transpose()?;
        if kids::scoped::require_identity_db(db, &principal, &origin, &identity, parent.as_ref())
            .is_ok()
            && seen.insert((raw_origin, raw_identity))
        {
            result.scoped_identities.push(ScopedMediaIdentity {
                metadata_origin: origin,
                identity,
            });
        }
        if seen.len() > 100000 {
            return Err(invalid());
        }
    }
    result
        .identities
        .sort_by(|a, b| (&a.kind, &a.id).cmp(&(&b.kind, &b.id)));
    result
        .identities
        .dedup_by(|a, b| a.kind == b.kind && a.id == b.id);
    if logical_keys.len() > 10000 || encode(result)?.len() > 16 * 1024 * 1024 {
        return Err("Library exceeds its bounded item limit".into());
    }
    Ok(())
}
pub(crate) fn entries_db(
    db: &Connection,
    app: &App,
    profile: i64,
) -> Result<Vec<(Key, Entry)>, ApiError> {
    let history = playback_db(db, app, profile)?;
    let mut by_title: BTreeMap<Key, Vec<&Saved<PlaybackRecord>>> = BTreeMap::new();
    for record in &history {
        by_title.entry(record.key.clone()).or_default().push(record);
        if let Some(parent) = parent_key(record)? {
            if parent != record.key {
                by_title.entry(parent).or_default().push(record);
            }
        }
    }
    favorites_db(db, app, profile)?
        .into_iter()
        .map(|s| {
            let mut statistics = LibraryStatistics::empty(s.value.identity.clone());
            statistics.metadata_origin = s.value.metadata_origin.clone();
            let matched = by_title.get(&s.key).map(Vec::as_slice).unwrap_or(&[]);
            statistics.last_watched_millis = matched
                .iter()
                .filter(|p| p.value.position_millis > 0 || p.value.completed_at_millis.is_some())
                .map(|p| p.updated)
                .max();
            statistics.watched = matched
                .iter()
                .any(|p| p.value.completed_at_millis.is_some());
            let episodes = matched.iter().any(|p| p.value.identity.episode.is_some());
            statistics.times_watched = if episodes {
                None
            } else {
                matched.iter().try_fold(0u64, |n, p| {
                    p.value.times_watched.map(|v| n.saturating_add(v))
                })
            };
            if episodes {
                statistics.watched_episode_count = Some(
                    matched
                        .iter()
                        .filter(|p| p.value.completed_at_millis.is_some())
                        .count() as u64,
                )
            }
            Ok((
                s.key,
                Entry {
                    card: s.value,
                    statistics,
                },
            ))
        })
        .collect()
}
fn parent_key(s: &Saved<PlaybackRecord>) -> Result<Option<Key>, ApiError> {
    let Some(parent) = &s.value.parent_identity else {
        return Ok(None);
    };
    if s.key.0 == "shared" {
        let mut identity = s.value.identity.clone();
        identity.id = s.key.2.clone();
        if shared_identity(&identity) && identity.episode.is_some() {
            let parts = identity.id.rsplitn(3, ':').collect::<Vec<_>>();
            if parts.len() == 3 {
                return Ok(Some(Key("shared".into(), "series".into(), parts[2].into())));
            }
        }
    }
    let mut card = s.value.card();
    card.identity = parent.clone();
    card.parent_identity = None;
    Ok(Some(if s.verified {
        verified_key(&card)?
    } else {
        raw_key(&card)?
    }))
}
fn selector(card: &Card) -> Result<String, ApiError> {
    encode(&(
        card.metadata_origin.as_ref().ok_or_else(invalid)?,
        &card.identity,
        &card.parent_identity,
    ))
}
fn config(db: &Connection, profile: i64) -> Result<u64, ApiError> {
    db.query_row(
        "SELECT revision FROM engine_profile_configuration WHERE profile_id=?1",
        [profile],
        |r| r.get(0),
    )
    .optional()
    .map_err(db_error)
    .map(|n| n.unwrap_or(0))
}
fn cache_configuration(
    db: &Connection,
    app: &App,
    profile: i64,
    card: &Card,
) -> Result<String, ApiError> {
    let origin = card.metadata_origin.as_ref().ok_or_else(invalid)?;
    origins::validate_assignment_db(db, &app.request_lease(), origin)?;
    let source = origin
        .connection_id
        .as_deref()
        .map(|id| crate::sources::source_configuration(db, id))
        .transpose()?
        .flatten();
    encode(&(config(db, profile)?, origin, source))
}
pub(crate) fn cache_next_db(
    db: &Connection,
    app: &App,
    profile: i64,
    current: &Card,
    result: &NextEpisodeResult,
) -> Result<(), ApiError> {
    validate_current(db, app, profile, current)?;
    if let Some(c) = &result.item {
        if c.metadata_origin != current.metadata_origin {
            return Err(invalid());
        }
        kids::scoped::require_card_db(db, &app.identity(), c)?;
    }
    db.execute(
        "DELETE FROM engine_scoped_next WHERE profile_id=?1 AND updated_at_millis<?2",
        params![profile, now().saturating_sub(300000)],
    )
    .map_err(db_error)?;
    db.execute("INSERT INTO engine_scoped_next VALUES(?1,?2,?3,?4,?5) ON CONFLICT(profile_id,selector) DO UPDATE SET configuration=excluded.configuration,result=excluded.result,updated_at_millis=excluded.updated_at_millis",params![profile,selector(current)?,cache_configuration(db,app,profile,current)?,encode(result)?,now()]).map_err(db_error)?;
    db.execute("DELETE FROM engine_scoped_next WHERE profile_id=?1 AND rowid NOT IN(SELECT rowid FROM engine_scoped_next WHERE profile_id=?1 ORDER BY updated_at_millis DESC,rowid DESC LIMIT 200)",[profile]).map_err(db_error)?;
    Ok(())
}
pub(crate) fn continuation_db(
    db: &Connection,
    app: &App,
    profile: i64,
) -> Result<Vec<(Key, ContinuationEntry)>, ApiError> {
    let mut groups: BTreeMap<Key, Saved<PlaybackRecord>> = BTreeMap::new();
    for saved in playback_db(db, app, profile)? {
        let key = parent_key(&saved)?.unwrap_or_else(|| saved.key.clone());
        if groups
            .get(&key)
            .is_none_or(|old| old.updated < saved.updated)
        {
            groups.insert(key, saved);
        }
    }
    let mut entries = Vec::new();
    for (key, saved) in groups {
        let hidden:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM engine_scoped_queue_hidden WHERE profile_id=?1 AND identity_scope=?2 AND kind=?3 AND identity_id=?4)",params![profile,key.0,key.1,key.2],|r|r.get(0)).map_err(db_error)?;
        if hidden {
            continue;
        }
        let record = saved.value;
        let mut card = record.card();
        let mut status = if record.can_resume() {
            ContinuationStatus::Resume
        } else if record.identity.episode.is_some() {
            ContinuationStatus::Pending
        } else {
            continue;
        };
        let mut previous_identity = None;
        if status == ContinuationStatus::Pending {
            let configuration = cache_configuration(db, app, profile, &card).unwrap_or_default();
            let cached:Option<String>=db.query_row("SELECT result FROM engine_scoped_next WHERE profile_id=?1 AND selector=?2 AND configuration=?3 AND updated_at_millis>=?4",params![profile,selector(&card)?,configuration,now().saturating_sub(300000)],|r|r.get(0)).optional().map_err(db_error)?;
            if let Some(value) = cached {
                let next: NextEpisodeResult = decode(&value)?;
                status = next.status;
                if let Some(next) = next.item {
                    if next.metadata_origin == card.metadata_origin
                        && kids::scoped::require_card_db(db, &app.identity(), &next).is_ok()
                    {
                        previous_identity = Some(card.identity.clone());
                        card = next;
                    } else {
                        status = ContinuationStatus::Unavailable
                    }
                }
            }
        }
        entries.push((
            key,
            ContinuationEntry {
                card,
                record,
                status,
                previous_identity,
            },
        ));
    }
    Ok(entries)
}

#[cfg(test)]
#[path = "scoped_library_tests.rs"]
mod tests;

/// Visibility is keyed by the saved storage identity; raw legacy queue rows never hide it.
pub(crate) fn visibility_db(
    db: &Connection,
    app: &App,
    profile: i64,
    card: &Card,
    hidden: bool,
) -> Result<viptv_engine::identity::MediaIdentity, ApiError> {
    app.require_profile(db, profile)?;
    app.request_lease().validate(db)?;
    card.metadata_origin
        .as_ref()
        .ok_or_else(invalid)?
        .validate()
        .map_err(|_| invalid())?;
    let mut saved = lookup::<PlaybackRecord>(db, profile, "playback", card)?;
    if saved.is_none() {
        let mut q=db.prepare("SELECT selector,result FROM engine_scoped_next WHERE profile_id=?1 AND configuration=?2 AND updated_at_millis>=?3 ORDER BY updated_at_millis DESC LIMIT 201").map_err(db_error)?;
        for row in q
            .query_map(
                params![
                    profile,
                    cache_configuration(db, app, profile, card)?,
                    now().saturating_sub(300000)
                ],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .map_err(db_error)?
        {
            let (raw, result) = row.map_err(db_error)?;
            let result: NextEpisodeResult = decode(&result)?;
            if result
                .item
                .as_ref()
                .is_some_and(|actual| metadata_assertions_match(card, actual))
            {
                let (origin, identity, parent): (
                    viptv_engine::origin::MetadataOrigin,
                    viptv_engine::identity::MediaIdentity,
                    Option<viptv_engine::identity::MediaIdentity>,
                ) = decode(&raw)?;
                let mut current = card.clone();
                current.metadata_origin = Some(origin);
                current.identity = identity;
                current.parent_identity = parent;
                if let Some(found) = lookup::<PlaybackRecord>(db, profile, "playback", &current)? {
                    if saved
                        .as_ref()
                        .is_some_and(|old: &Saved<PlaybackRecord>| old.key != found.key)
                    {
                        return Err(conflict());
                    }
                    saved = Some(found)
                }
            }
        }
    }
    let saved = saved.ok_or_else(|| {
        ApiError(
            StatusCode::NOT_FOUND,
            "Continuation item is unavailable".into(),
        )
    })?;
    kids::scoped::require_card_db(db, &app.identity(), &saved.value.card())?;
    let key = parent_key(&saved)?.unwrap_or_else(|| saved.key.clone());
    let identity = saved
        .value
        .parent_identity
        .clone()
        .unwrap_or_else(|| saved.value.identity.clone());
    if hidden{db.execute("INSERT OR IGNORE INTO engine_scoped_queue_hidden VALUES(?1,?2,?3,?4)",params![profile,key.0,key.1,key.2])}else{db.execute("DELETE FROM engine_scoped_queue_hidden WHERE profile_id=?1 AND identity_scope=?2 AND kind=?3 AND identity_id=?4",params![profile,key.0,key.1,key.2])}.map_err(db_error)?;
    Ok(identity)
}

/// Caller supplies the card held by a freshly validated host MetadataProof.
/// Exact private lookup records never fall back to raw-ID legacy progress.
pub(crate) fn resume_db(
    db: &Connection,
    app: &App,
    profile: i64,
    card: &Card,
) -> Result<f64, ApiError> {
    validate_current(db, app, profile, card)?;
    let desired = verified_key_db(db, profile, card)?;
    let mut prior = by_key::<PlaybackRecord>(db, profile, "playback", &desired)?;
    if let Some(saved) = &prior {
        if desired.0 != "shared" && !metadata_assertions_match(&saved.value.card(), card) {
            return Err(conflict());
        }
    }
    if prior.is_none() {
        prior = lookup::<PlaybackRecord>(db, profile, "playback", card)?.filter(|saved| {
            saved.key == desired || metadata_assertions_match(&saved.value.card(), card)
        });
    }
    if prior.is_none() && desired.0 == "shared" {
        prior = legacy_playback_key(db, profile, &desired)?;
    }
    Ok(prior
        .filter(|saved| saved.value.can_resume())
        .map_or(0.0, |saved| saved.value.position_millis as f64 / 1000.0))
}

/// Prepared Home favorites need only the first page, not every saved card/statistic.
pub(crate) fn home_favorites_db(
    db: &Connection,
    app: &App,
    profile: i64,
) -> Result<Vec<Card>, ApiError> {
    app.require_profile(db, profile)?;
    app.request_lease().validate(db)?;
    let allowed = kids::sql_allowed("favorites.type", "favorites.id", "?1");
    let mut q=db.prepare(&format!("SELECT id,type,name,poster,context,added_at_millis FROM favorites WHERE profile_id=?1 AND {allowed} ORDER BY added_at_millis DESC,name COLLATE NOCASE,type,id LIMIT 12")).map_err(db_error)?;
    let items=q.query_map([profile],|r|Ok(json!({"id":r.get::<_,String>(0)?,"type":r.get::<_,String>(1)?,"name":r.get::<_,String>(2)?,"poster":r.get::<_,Option<String>>(3)?,"engine_favorite":serde_json::from_str::<Value>(&r.get::<_,String>(4)?).unwrap_or(Value::Null),"added_at_millis":r.get::<_,Option<u64>>(5)?}))).map_err(db_error)?.collect::<Result<Vec<_>,_>>().map_err(db_error)?;
    let mut cards: BTreeMap<Key, Card> = super::library::cards(db, &app.identity(), items)?
        .into_iter()
        .map(|c| Ok((raw_key(&c)?, c)))
        .collect::<Result<_, ApiError>>()?;
    let mut accepted = 0;
    let mut offset = 0;
    while accepted < 12 && offset <= 10000 {
        let mut q=db.prepare("SELECT identity_scope,kind,identity_id,value,added_at_millis FROM engine_scoped_records WHERE profile_id=?1 AND record_kind='favorite' ORDER BY added_at_millis DESC,json_extract(value,'$.title') COLLATE NOCASE,kind,identity_id LIMIT 24 OFFSET ?2").map_err(db_error)?;
        let batch = q
            .query_map(params![profile, offset], |r| {
                Ok((
                    Key(r.get(0)?, r.get(1)?, r.get(2)?),
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<u64>>(4)?,
                ))
            })
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        let count = batch.len();
        for (key, raw, added) in batch {
            let mut card: Card = decode(&raw)?;
            if kids::scoped::require_card_db(db, &app.identity(), &card).is_err() {
                continue;
            }
            card.added_at_millis = added;
            card.progress = None;
            cards.insert(key, card);
            accepted += 1;
            if accepted == 12 {
                break;
            }
        }
        if count < 24 {
            break;
        }
        offset += 24;
    }
    let mut cards = cards.into_iter().collect::<Vec<_>>();
    cards.sort_by(|(ka, a), (kb, b)| {
        b.added_at_millis
            .cmp(&a.added_at_millis)
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
            .then_with(|| ka.cmp(kb))
    });
    Ok(cards.into_iter().take(12).map(|(_, c)| c).collect())
}
