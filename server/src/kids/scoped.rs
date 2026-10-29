use super::*;
use viptv_engine::{
    origin::{
        metadata_assertions_match, shared_identity, verified_storage_identity, MetadataOrigin,
    },
    presentation::Card,
};
pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS kids_origin_media(account_id INTEGER NOT NULL REFERENCES auth_accounts(id)ON DELETE CASCADE,origin TEXT NOT NULL,kind TEXT NOT NULL,lookup_id TEXT NOT NULL,policy_key TEXT NOT NULL,parent_key TEXT NOT NULL,card TEXT NOT NULL,age INTEGER,conflict INTEGER NOT NULL,current INTEGER NOT NULL DEFAULT 1,updated_at INTEGER NOT NULL,PRIMARY KEY(account_id,origin,kind,lookup_id,policy_key));CREATE INDEX IF NOT EXISTS kids_origin_policy ON kids_origin_media(account_id,policy_key);CREATE TABLE IF NOT EXISTS kids_origin_approvals(profile_id INTEGER NOT NULL REFERENCES profiles(id)ON DELETE CASCADE,policy_key TEXT NOT NULL,card TEXT NOT NULL,PRIMARY KEY(profile_id,policy_key));")?;
    let has: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('kids_origin_media')WHERE name='current')",
        [],
        |r| r.get(0),
    )?;
    if !has {
        db.execute_batch(
            "ALTER TABLE kids_origin_media ADD COLUMN current INTEGER NOT NULL DEFAULT 1",
        )?;
    }
    db.execute_batch("CREATE TABLE IF NOT EXISTS kids_origin_ambiguous(account_id INTEGER NOT NULL REFERENCES auth_accounts(id)ON DELETE CASCADE,origin TEXT NOT NULL,kind TEXT NOT NULL,lookup_id TEXT NOT NULL,PRIMARY KEY(account_id,origin,kind,lookup_id));")?;
    Ok(())
}
fn key(card: &Card) -> Result<String, ApiError> {
    let identity = verified_storage_identity(card);
    let origin = card
        .metadata_origin
        .as_ref()
        .ok_or("Metadata origin is required")?;
    if origin.provider_id == "stremio" && card.identity.id.starts_with("simkl:") {
        return Err(forbidden());
    }
    let scope = if shared_identity(&identity)
        && (!identity.id.starts_with("simkl:") || origin.provider_id == "simkl")
    {
        "shared".to_owned()
    } else {
        origin
            .key()
            .map_err(|_| ApiError::from("Invalid metadata origin"))?
    };
    Ok(json!([scope, identity.kind, identity.id]).to_string())
}
fn origin_key(origin: &MetadataOrigin) -> Result<String, ApiError> {
    origin
        .key()
        .map_err(|_| ApiError::from("Invalid metadata origin"))
}
pub(crate) fn observe(
    db: &Connection,
    account: i64,
    card: &Card,
    parent: Option<&Card>,
    meta: &Value,
) -> Result<(), ApiError> {
    let origin = card
        .metadata_origin
        .as_ref()
        .ok_or("Metadata origin is required")?;
    let policy = key(card)?;
    let parent_key = parent
        .map(key)
        .transpose()?
        .unwrap_or_else(|| policy.clone());
    db.execute("UPDATE kids_origin_media SET current=0 WHERE account_id=?1 AND origin=?2 AND kind=?3 AND lookup_id=?4 AND policy_key<>?5",params![account,origin_key(origin)?,card.identity.kind,card.identity.id,policy]).map_err(db_error)?;
    let prior_parent:Option<String>=db.query_row("SELECT parent_key FROM kids_origin_media WHERE account_id=?1 AND origin=?2 AND kind=?3 AND lookup_id=?4 AND policy_key=?5",params![account,origin_key(origin)?,card.identity.kind,card.identity.id,policy],|r|r.get(0)).optional().map_err(db_error)?;
    if prior_parent.is_some_and(|prior| prior != parent_key) {
        db.execute("INSERT OR IGNORE INTO kids_origin_ambiguous(account_id,origin,kind,lookup_id)VALUES(?1,?2,?3,?4)",params![account,origin_key(origin)?,card.identity.kind,card.identity.id]).map_err(db_error)?;
    }
    let (mut age, mut conflict) = metadata::assessment(meta);
    if parent.is_some() {
        let(parent_age,parent_conflict):(Option<i64>,bool)=db.query_row("SELECT MAX(age),COALESCE(MAX(conflict),0) FROM kids_origin_media WHERE account_id=?1 AND policy_key=?2",params![account,parent_key],|r|Ok((r.get(0)?,r.get(1)?))).map_err(db_error)?;
        age = age.into_iter().chain(parent_age).max();
        conflict |= parent_conflict;
    }
    if let Some(age) = age {
        let disagrees:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM kids_origin_media WHERE account_id=?1 AND policy_key=?2 AND age IS NOT NULL AND age<>?3)",params![account,policy,age],|r|r.get(0)).map_err(db_error)?;
        conflict |= disagrees;
        if disagrees {
            db.execute(
                "UPDATE kids_origin_media SET conflict=1 WHERE account_id=?1 AND policy_key=?2",
                params![account, policy],
            )
            .map_err(db_error)?;
        }
    }
    let old:Option<(Option<i64>,bool)>=db.query_row("SELECT age,conflict FROM kids_origin_media WHERE account_id=?1 AND origin=?2 AND kind=?3 AND lookup_id=?4 AND policy_key=?5",params![account,origin_key(origin)?,card.identity.kind,card.identity.id,policy],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(db_error)?;
    let conflict = conflict
        || old.as_ref().is_some_and(|(previous, bad)| {
            *bad || previous.is_some() && age.is_some() && *previous != age
        });
    let changed = old.as_ref().is_some_and(|(prior_age, prior_conflict)| {
        *prior_age != age || !*prior_conflict && conflict
    });
    let age = age.into_iter().chain(old.and_then(|(age, _)| age)).max();
    if changed {
        db.execute("UPDATE kids_profiles SET revision=revision+1 WHERE enabled=1 AND profile_id IN(SELECT profile_id FROM profile_owners WHERE account_id=?1)",[account]).map_err(db_error)?;
    }
    db.execute("INSERT INTO kids_origin_media(account_id,origin,kind,lookup_id,policy_key,parent_key,card,age,conflict,updated_at)VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT(account_id,origin,kind,lookup_id,policy_key)DO UPDATE SET current=1,parent_key=excluded.parent_key,card=excluded.card,age=excluded.age,conflict=excluded.conflict,updated_at=excluded.updated_at",params![account,origin_key(origin)?,card.identity.kind,card.identity.id,policy,parent_key,serde_json::to_string(card).map_err(|_|ApiError::from("Invalid metadata"))?,age,conflict,util::now()]).map_err(db_error)?;
    Ok(())
}
fn permitted(db: &Connection, p: &auth::Principal, card: &Card) -> Result<bool, ApiError> {
    if !restricted(db, p)? {
        return Ok(true);
    }
    let auth::Principal::Account {
        profile_id: Some(profile),
        ..
    } = p
    else {
        return Ok(false);
    };
    let origin = card.metadata_origin.as_ref().ok_or_else(forbidden)?;
    let policy = key(card)?;
    let ambiguous:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM kids_origin_ambiguous WHERE account_id=?1 AND origin=?2 AND kind=?3 AND lookup_id=?4)",params![p.account_id(),origin_key(origin)?,card.identity.kind,card.identity.id],|r|r.get(0)).map_err(db_error)?;
    if ambiguous {
        return Ok(false);
    }
    let adult:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM kids_origin_media m WHERE m.account_id=?1 AND (m.policy_key=?2 OR m.policy_key IN(SELECT parent_key FROM kids_origin_media WHERE account_id=?1 AND policy_key=?2)) AND m.age>=18)",params![p.account_id(),policy],|r|r.get(0)).map_err(db_error)?;
    if adult {
        return Ok(false);
    }
    let stale:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM kids_origin_media WHERE account_id=?1 AND origin=?2 AND kind=?3 AND lookup_id=?4 AND policy_key=?5 AND current=0)",params![p.account_id(),origin_key(origin)?,card.identity.kind,card.identity.id,policy],|r|r.get(0)).map_err(db_error)?;
    if stale {
        return Ok(false);
    }
    let canonical = verified_storage_identity(card);
    if shared_identity(&canonical) && policy::allow_item(db, p, &canonical.kind, &canonical.id)? {
        return Ok(true);
    }
    db.query_row("SELECT EXISTS(SELECT 1 FROM kids_origin_media m JOIN kids_profiles k ON k.profile_id=?1 JOIN kids_origin_media root ON root.account_id=m.account_id AND root.policy_key=m.parent_key AND root.policy_key=root.parent_key WHERE m.current=1 AND root.current=1 AND m.account_id=?2 AND m.origin=?3 AND m.kind=?4 AND m.lookup_id=?5 AND m.policy_key=?6 AND COALESCE(m.age,0)<18 AND COALESCE(root.age,0)<18 AND NOT EXISTS(SELECT 1 FROM kids_origin_media adult WHERE adult.account_id=m.account_id AND adult.policy_key IN(m.policy_key,m.parent_key) AND adult.age>=18) AND (EXISTS(SELECT 1 FROM kids_origin_approvals a WHERE a.profile_id=?1 AND a.policy_key=m.parent_key) OR (m.conflict=0 AND root.conflict=0 AND m.age<=k.max_age AND root.age<=k.max_age)))",params![profile,p.account_id(),origin_key(origin)?,card.identity.kind,card.identity.id,policy],|r|r.get(0)).map_err(db_error)
}
pub(crate) fn require_card_db(
    db: &Connection,
    p: &auth::Principal,
    card: &Card,
) -> Result<(), ApiError> {
    if permitted(db, p, card)? {
        Ok(())
    } else {
        Err(forbidden())
    }
}
pub(crate) fn lookup_db(
    db: &Connection,
    p: &auth::Principal,
    origin: &MetadataOrigin,
    kind: &str,
    id: &str,
    expected: Option<&Card>,
) -> Result<Card, ApiError> {
    let mut query=db.prepare("SELECT card FROM kids_origin_media WHERE current=1 AND account_id=?1 AND origin=?2 AND kind=?3 AND lookup_id=?4 ORDER BY policy_key LIMIT 32").map_err(db_error)?;
    let rows = query
        .query_map(
            params![p.account_id(), origin_key(origin)?, kind, id],
            |r| r.get::<_, String>(0),
        )
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    let mut matching = rows
        .into_iter()
        .filter_map(|raw| serde_json::from_str::<Card>(&raw).ok())
        .filter(|card| expected.is_none_or(|expected| metadata_assertions_match(expected, card)));
    let card = matching.next().ok_or_else(forbidden)?;
    if matching.next().is_some() {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Title identity is ambiguous. Select the saved title again.".into(),
        ));
    }
    require_card_db(db, p, &card)?;
    Ok(card)
}
pub(crate) fn require(
    app: &App,
    origin: &MetadataOrigin,
    kind: &str,
    id: &str,
    expected: Option<&Card>,
) -> Result<Option<Card>, ApiError> {
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.request_lease().validate(&db)?;
    crate::engine_v3::origins::validate_db(
        &db,
        app.identity().account_id().ok_or_else(auth::forbidden)?,
        origin,
    )?;
    if !restricted(&db, &app.identity())? {
        return Ok(None);
    }
    if titles::canonical(&origin.provider_id, kind, id)
        && policy::allow_item(&db, &app.identity(), kind, id)?
    {
        return Ok(None);
    }
    lookup_db(&db, &app.identity(), origin, kind, id, expected).map(Some)
}
pub(crate) fn approve_db(
    db: &Connection,
    app: &App,
    profile: i64,
    card: &Card,
    approved: bool,
) -> Result<Value, ApiError> {
    manager(db, &app.identity(), Some(profile))?;
    let policy = key(card)?;
    if approved {
        let adult:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM kids_origin_media WHERE account_id=?1 AND policy_key=?2 AND age>=18)",params![app.identity().account_id(),policy],|r|r.get(0)).map_err(db_error)?;
        if adult {
            return Err("Adult titles cannot be approved for kids profiles".into());
        }
        let count:i64=db.query_row("SELECT(SELECT count(*)FROM kids_origin_approvals WHERE profile_id=?1)+(SELECT count(*)FROM kids_approvals WHERE profile_id=?1)",[profile],|r|r.get(0)).map_err(db_error)?;
        let exists:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM kids_origin_approvals WHERE profile_id=?1 AND policy_key=?2)",params![profile,policy],|r|r.get(0)).map_err(db_error)?;
        if count >= 500 && !exists {
            return Err("A profile can have at most 500 approved titles".into());
        }
        db.execute("INSERT INTO kids_origin_approvals(profile_id,policy_key,card)VALUES(?1,?2,?3)ON CONFLICT(profile_id,policy_key)DO UPDATE SET card=excluded.card",params![profile,policy,serde_json::to_string(card).map_err(|_|ApiError::from("Invalid title approval"))?]).map_err(db_error)?;
    } else {
        let stored: Option<String> = db
            .query_row(
                "SELECT card FROM kids_origin_approvals WHERE profile_id=?1 AND policy_key=?2",
                params![profile, policy],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?;
        if let Some(stored) = stored {
            let stored: Card = serde_json::from_str(&stored)
                .map_err(|_| ApiError::from("Approval unavailable"))?;
            if !metadata_assertions_match(card, &stored) {
                return Err(ApiError(
                    StatusCode::CONFLICT,
                    "Approval identity changed".into(),
                ));
            }
            db.execute(
                "DELETE FROM kids_origin_approvals WHERE profile_id=?1 AND policy_key=?2",
                params![profile, policy],
            )
            .map_err(db_error)?;
        }
    }
    db.execute("INSERT INTO kids_profiles(profile_id,revision)VALUES(?1,1)ON CONFLICT(profile_id)DO UPDATE SET revision=revision+1",[profile]).map_err(db_error)?;
    Ok(json!({"approved":approved}))
}
pub(crate) fn list_db(db: &Connection, profile: i64) -> Result<Vec<Value>, ApiError> {
    let mut query=db.prepare("SELECT card FROM kids_origin_approvals WHERE profile_id=?1 ORDER BY policy_key LIMIT 501").map_err(db_error)?;
    let result=query.query_map([profile],|r|r.get::<_,String>(0)).map_err(db_error)?.map(|raw|{
        let card:Card=serde_json::from_str(&raw.map_err(db_error)?).map_err(|_|ApiError::from("Approval unavailable"))?;
        Ok(json!({"id":card.identity.id,"type":card.identity.kind,"name":card.title,"poster":card.poster,"metadata_provider_id":card.metadata_origin.as_ref().map(|o|o.provider_id.clone()),"metadata_addon_id":card.metadata_origin.as_ref().and_then(|o|o.connection_id.as_deref()).and_then(|id|id.strip_prefix("addon:")).and_then(|id|id.parse::<i64>().ok()),"metadata_origin":card.metadata_origin,"card":card,"approval_supported":true}))
    }).collect();
    result
}

pub(crate) fn observe_document(
    db: &Connection,
    account: i64,
    parent: &Card,
    episodes: &[Card],
    meta: &Value,
) -> Result<(), ApiError> {
    observe(db, account, parent, None, meta)?;
    let origin = parent
        .metadata_origin
        .as_ref()
        .ok_or("Metadata origin is required")?;
    db.execute("UPDATE kids_origin_media SET current=0 WHERE account_id=?1 AND origin=?2 AND parent_key=?3 AND policy_key<>parent_key",params![account,origin_key(origin)?,key(parent)?]).map_err(db_error)?;
    let rows = meta["videos"].as_array();
    for episode in episodes {
        let raw = rows
            .into_iter()
            .flatten()
            .find(|raw| raw["id"].as_str() == Some(episode.identity.id.as_str()))
            .ok_or("Episode metadata unavailable")?;
        observe(db, account, episode, Some(parent), raw)?;
    }
    db.execute("DELETE FROM kids_origin_media WHERE account_id=?1 AND rowid NOT IN(SELECT rowid FROM kids_origin_media WHERE account_id=?1 ORDER BY updated_at DESC,rowid DESC LIMIT 10000) AND NOT EXISTS(SELECT 1 FROM kids_origin_approvals a JOIN profile_owners o ON o.profile_id=a.profile_id WHERE o.account_id=?1 AND a.policy_key IN(kids_origin_media.policy_key,kids_origin_media.parent_key) AND (kids_origin_media.current=1 OR kids_origin_media.policy_key=kids_origin_media.parent_key))",[account]).map_err(db_error)?;
    Ok(())
}

pub(crate) fn invalidate_mismatch(
    db: &Connection,
    account: i64,
    actual: &Card,
) -> Result<(), ApiError> {
    let Some(origin) = actual.metadata_origin.as_ref() else {
        return Ok(());
    };
    let mut query=db.prepare("SELECT policy_key,card FROM kids_origin_media WHERE account_id=?1 AND origin=?2 AND kind=?3 AND lookup_id=?4 AND current=1").map_err(db_error)?;
    let rows = query
        .query_map(
            params![
                account,
                origin_key(origin)?,
                actual.identity.kind,
                actual.identity.id
            ],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    for (key, stored) in rows {
        let stored: Card =
            serde_json::from_str(&stored).map_err(|_| ApiError::from("Metadata unavailable"))?;
        if !metadata_assertions_match(&stored, actual) {
            db.execute("UPDATE kids_origin_media SET current=0 WHERE account_id=?1 AND origin=?2 AND kind=?3 AND lookup_id=?4 AND policy_key=?5",params![account,origin_key(origin)?,actual.identity.kind,actual.identity.id,key]).map_err(db_error)?;
        }
    }
    Ok(())
}

pub(crate) fn require_identity_db(
    db: &Connection,
    p: &auth::Principal,
    origin: &viptv_engine::origin::MetadataOrigin,
    identity: &viptv_engine::identity::MediaIdentity,
    parent: Option<&viptv_engine::identity::MediaIdentity>,
) -> Result<(), ApiError> {
    if !restricted(db, p)? {
        return Ok(());
    }
    let card:Card=serde_json::from_value(json!({"identity":identity,"parent_identity":parent,"metadata_origin":origin,"provider_id":origin.provider_id,"title":"Stored title","badges":[]})).map_err(|_|ApiError::from("Stored identity unavailable"))?;
    require_card_db(db, p, &card)
}
