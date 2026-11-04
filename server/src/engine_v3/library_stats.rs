use super::*;
use viptv_engine::library::{Entry, LibraryStatistics};

struct HistoryRow {
    updated_millis: u64,
    watched: bool,
    counts_last: bool,
    has_episode: bool,
    count: Option<u64>,
}
pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS engine_watch_counts(profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,type TEXT NOT NULL,id TEXT NOT NULL,count INTEGER,PRIMARY KEY(profile_id,type,id));CREATE TRIGGER IF NOT EXISTS engine_watch_counts_delete AFTER DELETE ON progress BEGIN DELETE FROM engine_watch_counts WHERE profile_id=OLD.profile_id AND type=OLD.type AND id=OLD.id;END;")
}
pub(crate) fn observe(
    db: &Connection,
    profile: i64,
    kind: &str,
    id: &str,
    completed: bool,
) -> Result<(), ApiError> {
    let previous:Option<(f64,f64,String)>=db.query_row("SELECT position,duration,context FROM progress WHERE profile_id=?1 AND type=?2 AND id=?3",params![profile,kind,id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(db_error)?;
    let was_completed = previous
        .as_ref()
        .is_some_and(|(position, duration, context)| {
            crate::library::watched(
                &json!({"position":position,"duration":duration}),
                &serde_json::from_str::<Value>(context).unwrap_or(Value::Null),
            )
        });
    let count: Option<Option<u64>> = db
        .query_row(
            "SELECT count FROM engine_watch_counts WHERE profile_id=?1 AND type=?2 AND id=?3",
            params![profile, kind, id],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?;
    let count = count
        .unwrap_or_else(|| previous.as_ref().map(|_| None).unwrap_or(Some(0)))
        .map(|count| {
            if completed && !was_completed {
                count.saturating_add(1)
            } else {
                count
            }
        });
    db.execute("INSERT INTO engine_watch_counts VALUES(?1,?2,?3,?4) ON CONFLICT(profile_id,type,id) DO UPDATE SET count=excluded.count",params![profile,kind,id,count]).map_err(db_error)?;
    Ok(())
}
pub(crate) fn entries(
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    revision: u64,
) -> Result<Vec<Entry>, ApiError> {
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_profile(&db, profile)?;
    if !kids::restricted(&db, &app.identity())? {
        super::art::backfill(&db, profile, super::art::READ_BATCH)?;
    }
    let allowed = kids::sql_allowed("favorites.type", "favorites.id", "?1");
    let mut query=db.prepare(&format!("SELECT id,type,name,poster,context,added_at_millis FROM favorites WHERE profile_id=?1 AND type IN ('movie','series') AND {allowed} ORDER BY type,id LIMIT 10001")).map_err(db_error)?;
    let items=query.query_map([profile],|r|Ok(json!({"id":r.get::<_,String>(0)?,"type":r.get::<_,String>(1)?,"name":r.get::<_,String>(2)?,"poster":r.get::<_,Option<String>>(3)?,"added_at_millis":r.get::<_,Option<u64>>(5)?,"engine_favorite":serde_json::from_str::<Value>(&r.get::<_,String>(4)?).unwrap_or(Value::Null)}))).map_err(db_error)?.collect::<Result<Vec<_>,_>>().map_err(db_error)?;
    if items.len() > 10000 {
        return Err("Library exceeds its bounded item limit".into());
    }
    let mut cards = super::library::cards(&db, &app.identity(), items)?;
    super::source_integrations::merge_cached_with_db(app, lease, revision, &db, &mut cards);
    let imports = crate::simkl::imports::Imports::load(&db, profile)?;
    let mut history = Vec::<HistoryRow>::new();
    let mut by_title = std::collections::HashMap::<(String, String), Vec<usize>>::new();
    {
        let mut progress = db
            .prepare(
                "SELECT p.id,p.type,p.position,p.duration,p.updated_at,p.context,p.title_id,w.count FROM progress p LEFT JOIN engine_watch_counts w ON w.profile_id=p.profile_id AND w.type=p.type AND w.id=p.id WHERE p.profile_id=?1 AND p.type IN ('movie','series') LIMIT 50001",
            )
            .map_err(db_error)?;
        for row in progress
            .query_map([profile], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, f64>(2)?,
                    r.get::<_, f64>(3)?,
                    r.get::<_, u64>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, Option<u64>>(7)?,
                ))
            })
            .map_err(db_error)?
        {
            let (id, kind, position, duration, updated, context_raw, parent, count) =
                row.map_err(db_error)?;
            let item = json!({ "position": position, "duration": duration });
            let context: Value = serde_json::from_str(&context_raw).unwrap_or(Value::Null);
            let watched = crate::library::watched(&item, &context);
            let idx = history.len();
            history.push(HistoryRow {
                updated_millis: updated.saturating_mul(1000),
                watched,
                counts_last: context["stremio_watch_date_unknown"] != true
                    && (position > 0.0 || watched),
                has_episode: serde_json::from_value::<viptv_engine::identity::MediaIdentity>(
                    context["engine_identity"].clone(),
                )
                .ok()
                .is_some_and(|identity| identity.episode.is_some()),
                count,
            });
            let media_id = crate::simkl::key::media(&kind, &id).into_owned();
            by_title
                .entry((kind.clone(), media_id))
                .or_default()
                .push(idx);
            if let Some(parent) = parent {
                let parent = crate::simkl::key::media("series", &parent).into_owned();
                by_title.entry((kind, parent)).or_default().push(idx);
            }
        }
        if history.len() > 50000 {
            return Err("History exceeds its bounded item limit".into());
        }
    }
    let mut entries: Vec<Entry> = cards
        .into_iter()
        .map(|mut card| {
            imports.enrich(&mut card);
            crate::simkl::key::card(&mut card);
            let mut statistics = LibraryStatistics::empty(card.identity.clone());
            let matched = by_title
                .get(&(card.identity.kind.clone(), card.identity.id.clone()))
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            statistics.last_watched_millis = matched
                .iter()
                .filter(|&&i| history[i].counts_last)
                .map(|&i| history[i].updated_millis)
                .filter(|n| *n > 0)
                .max();
            statistics.watched = matched.iter().any(|&i| history[i].watched);
            let episodes: Vec<usize> = matched
                .iter()
                .copied()
                .filter(|&i| history[i].has_episode)
                .collect();
            statistics.times_watched = if episodes.is_empty() {
                matched
                    .iter()
                    .try_fold(0u64, |sum, &i| history[i].count.map(|c| sum.saturating_add(c)))
            } else {
                None
            };
            if !episodes.is_empty() {
                statistics.watched_episode_count = Some(
                    episodes
                        .iter()
                        .filter(|&&i| history[i].watched)
                        .count() as u64,
                );
            }
            imports.merge(&mut statistics);
            Ok(Entry { card, statistics })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    for (mut key, mut entry) in super::scoped_library::entries_db(&db, app, profile)? {
        let observed = entry.statistics.clone();
        let before = entry.card.identity.id.clone();
        crate::simkl::key::card(&mut entry.card);
        crate::simkl::key::identity(&mut entry.statistics.identity);
        if entry.card.identity.id != before {
            key = super::scoped_library::raw_key(&entry.card)?;
        }
        if let Some(old) = entries
            .iter()
            .find(|old| super::scoped_library::raw_key(&old.card).is_ok_and(|old| old == key))
        {
            if entry.statistics.last_watched_millis.is_none() {
                entry.statistics = old.statistics.clone();
                entry.statistics.identity = entry.card.identity.clone();
                entry.statistics.metadata_origin = entry.card.metadata_origin.clone();
            }
        }
        if key.0 == "shared" {
            entry.statistics.identity.id = key.2.clone();
            entry.statistics.identity.kind = key.1.clone();
            imports.merge(&mut entry.statistics);
            entry.statistics.identity = entry.card.identity.clone();
            entry.statistics.metadata_origin = entry.card.metadata_origin.clone();
            if observed.last_watched_millis.is_some() {
                entry.statistics.watched = observed.watched;
                entry.statistics.last_watched_millis = observed.last_watched_millis;
            }
            let mut seen = std::collections::BTreeSet::new();
            let mut provenance = Vec::new();
            for source in std::mem::take(&mut entry.statistics.provenance) {
                let identity = serde_json::to_string(&source)
                    .map_err(|_| ApiError::from("Invalid import statistics"))?;
                if seen.insert(identity) {
                    provenance.push(source)
                }
            }
            entry.statistics.provenance = provenance;
        }
        entries.retain(|old| super::scoped_library::raw_key(&old.card).is_ok_and(|old| old != key));
        entries.push(entry);
    }
    if entries.len() > 10000 {
        return Err("Library exceeds its bounded item limit".into());
    }
    Ok(entries)
}

#[cfg(test)]
#[path = "library_stats_tests.rs"]
mod tests;
