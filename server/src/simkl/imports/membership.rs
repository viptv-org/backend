use super::*;
use viptv_engine::library::{Entry, LibraryProvenance, LibraryStatistics};
fn local_identity(
    db: &Connection,
    profile: i64,
    card: &viptv_engine::presentation::Card,
) -> Result<(String, String), ApiError> {
    let old: Option<(String,String)> = db.query_row("SELECT local_id,local_kind FROM simkl_import_entries WHERE profile_id=?1 AND remote_kind=?2 AND remote_id=?3",params![profile,card.identity.kind,card.identity.id],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(db_error)?;
    if let Some(old) = old {
        return Ok(old);
    }
    if let Some(imdb) = card
        .identity
        .ids
        .iter()
        .find(|id| id.namespace == "imdb" && id.value.starts_with("tt"))
    {
        let local: Option<String> = db
            .query_row(
                "SELECT id FROM favorites WHERE profile_id=?1 AND type=?2 AND id=?3",
                params![profile, card.identity.kind, imdb.value],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?;
        if let Some(local) = local {
            return Ok((local, card.identity.kind.clone()));
        }
    }
    Ok((card.identity.id.clone(), card.identity.kind.clone()))
}
pub(super) fn apply(
    db: &Connection,
    profile: i64,
    revision: u64,
    entries: &[Entry],
    now: u64,
) -> Result<usize, ApiError> {
    apply_canonical(db, profile, revision, entries, now, &std::collections::HashMap::new())
}

pub(super) fn apply_canonical(
    db: &Connection,
    profile: i64,
    revision: u64,
    entries: &[Entry],
    now: u64,
    canonical: &std::collections::HashMap<(String, String), viptv_engine::identity::MediaIdentity>,
) -> Result<usize, ApiError> {
    let mut inserted = 0;
    for entry in entries {
        let mut card = entry.card.clone();
        let external = card.identity.clone();
        let show = canonical
            .get(&(external.kind.clone(), external.id.clone()))
            .cloned();
        let (id, kind) = match &show {
            Some(show) => (show.id.clone(), show.kind.clone()),
            None => local_identity(db, profile, &entry.card)?,
        };
        if let Some(show) = show {
            card.identity = show;
        }
        card.identity.id = id.clone();
        card.identity.kind = kind.clone();
        card.progress = None;
        let mut statistics = entry.statistics.clone();
        statistics.identity = card.identity.clone();
        let original:Option<String>=db.query_row("SELECT statistics FROM simkl_import_entries WHERE profile_id=?1 AND remote_kind=?2 AND remote_id=?3",params![profile,external.kind,external.id],|r|r.get(0)).optional().map_err(db_error)?;
        let first = original
            .and_then(|s| serde_json::from_str::<LibraryStatistics>(&s).ok())
            .and_then(|s| s.provenance.first().map(|p| p.imported_at_millis))
            .unwrap_or(now);
        statistics.provenance = vec![LibraryProvenance {
            provider_id: "simkl".into(),
            external_identity: external.clone(),
            imported_at_millis: first,
            source_updated_at_millis: statistics.last_watched_millis,
            link_revision: Some(revision),
        }];
        db.execute("INSERT INTO simkl_import_entries VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(profile_id,remote_kind,remote_id) DO UPDATE SET local_id=excluded.local_id,local_kind=excluded.local_kind,card=excluded.card,statistics=excluded.statistics,link_revision=excluded.link_revision WHERE local_id!=excluded.local_id OR local_kind!=excluded.local_kind OR card!=excluded.card OR statistics!=excluded.statistics OR link_revision!=excluded.link_revision",params![profile,external.id,external.kind,id,kind,serde_json::to_string(&card).map_err(|_|ApiError::from("Invalid import card"))?,serde_json::to_string(&statistics).map_err(|_|ApiError::from("Invalid import statistics"))?,revision]).map_err(db_error)?;
        let removed:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM library_removals WHERE profile_id=?1 AND type=?2 AND id=?3)",params![profile,kind,id],|r|r.get(0)).map_err(db_error)?;
        if removed {
            continue;
        }
        inserted+=db.execute("INSERT OR IGNORE INTO favorites(profile_id,id,type,name,poster,context,added_at_millis) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![profile,id,kind,card.title,card.poster,serde_json::to_string(&card).map_err(|_|ApiError::from("Invalid import card"))?,card.added_at_millis]).map_err(db_error)?;
    }
    Ok(inserted)
}
pub(crate) fn merge(
    db: &Connection,
    profile: i64,
    statistics: &mut LibraryStatistics,
) -> Result<(), ApiError> {
    let mut query=db.prepare("SELECT statistics FROM simkl_import_entries WHERE profile_id=?1 AND local_kind=?2 AND local_id=?3 ORDER BY remote_id").map_err(db_error)?;
    let values = query
        .query_map(
            params![profile, statistics.identity.kind, statistics.identity.id],
            |r| r.get::<_, String>(0),
        )
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    let local = statistics.clone();
    for value in values {
        let Ok(remote) = serde_json::from_str::<LibraryStatistics>(&value) else {
            continue;
        };
        let newer_local = local
            .last_watched_millis
            .is_some_and(|t| remote.last_watched_millis.is_none_or(|r| t > r));
        statistics.provenance.extend(remote.provenance);
        statistics.user_rating = remote.user_rating;
        if !newer_local {
            statistics.status = remote.status;
            statistics.has_unwatched_aired_episodes = remote.has_unwatched_aired_episodes;
            statistics.next_episode = remote.next_episode;
            statistics.next_air_millis = remote.next_air_millis;
        }
        if !newer_local {
            statistics.watched |= remote.watched;
        }
        statistics.last_watched_millis = statistics
            .last_watched_millis
            .into_iter()
            .chain(remote.last_watched_millis)
            .max();
        statistics.times_watched = if newer_local {
            None
        } else {
            remote.times_watched
        };
        statistics.watched_episode_count = statistics
            .watched_episode_count
            .into_iter()
            .chain(remote.watched_episode_count)
            .max();
        statistics.total_episode_count = remote.total_episode_count;
        statistics.aired_episode_count = remote.aired_episode_count;
    }
    Ok(())
}

pub(crate) fn enrich(
    db: &Connection,
    profile: i64,
    card: &mut viptv_engine::presentation::Card,
) -> Result<(), ApiError> {
    let target = card
        .parent_identity
        .as_ref()
        .unwrap_or(&card.identity)
        .clone();
    let mut query=db.prepare("SELECT card FROM simkl_import_entries WHERE profile_id=?1 AND local_kind=?2 AND local_id=?3 ORDER BY remote_id").map_err(db_error)?;
    for value in query
        .query_map(params![profile, target.kind, target.id], |r| {
            r.get::<_, String>(0)
        })
        .map_err(db_error)?
    {
        if let Ok(metadata) =
            serde_json::from_str::<viptv_engine::presentation::Card>(&value.map_err(db_error)?)
        {
            *card = viptv_engine::presentation::enrich_card(card, &metadata);
        }
    }
    Ok(())
}
