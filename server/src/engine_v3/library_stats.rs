//! Counts only observed completion transitions; old watch totals remain unknown.
use super::*;
use viptv_engine::library::{Entry, LibraryStatistics};
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
    let allowed = kids::sql_allowed("favorites.type", "favorites.id", "?1");
    let mut query=db.prepare(&format!("SELECT id,type,name,poster,context,added_at_millis FROM favorites WHERE profile_id=?1 AND type IN ('movie','series') AND {allowed} ORDER BY type,id LIMIT 10001")).map_err(db_error)?;
    let items=query.query_map([profile],|r|Ok(json!({"id":r.get::<_,String>(0)?,"type":r.get::<_,String>(1)?,"name":r.get::<_,String>(2)?,"poster":r.get::<_,Option<String>>(3)?,"added_at_millis":r.get::<_,Option<u64>>(5)?,"engine_favorite":serde_json::from_str::<Value>(&r.get::<_,String>(4)?).unwrap_or(Value::Null)}))).map_err(db_error)?.collect::<Result<Vec<_>,_>>().map_err(db_error)?;
    if items.len() > 10000 {
        return Err("Library exceeds its bounded item limit".into());
    }
    let mut cards = super::library::cards(&db, &app.identity(), items)?;
    super::source_integrations::merge_cached(app, lease, revision, &mut cards);
    let mut progress=db.prepare("SELECT p.id,p.type,p.position,p.duration,p.updated_at,p.context,p.title_id,w.count FROM progress p LEFT JOIN engine_watch_counts w ON w.profile_id=p.profile_id AND w.type=p.type AND w.id=p.id WHERE p.profile_id=?1 AND p.type IN ('movie','series') LIMIT 50001").map_err(db_error)?;
    let history = progress
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
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    if history.len() > 50000 {
        return Err("History exceeds its bounded item limit".into());
    }
    Ok(cards
        .into_iter()
        .map(|card| {
            let mut statistics = LibraryStatistics::empty(card.identity.clone());
            let matched = history
                .iter()
                .filter(|(id, kind, _, _, _, _, parent, _)| {
                    kind == &card.identity.kind
                        && (id == &card.identity.id || parent.as_ref() == Some(&card.identity.id))
                })
                .collect::<Vec<_>>();
            statistics.last_watched_millis = matched
                .iter()
                .filter(|(_, _, position, duration, _, context, _, _)| {
                    let context = serde_json::from_str::<Value>(context).unwrap_or(Value::Null);
                    context["stremio_watch_date_unknown"] != true
                        && (*position > 0.0
                            || crate::library::watched(
                                &json!({"position":position,"duration":duration}),
                                &context,
                            ))
                })
                .map(|(_, _, _, _, updated, _, _, _)| updated.saturating_mul(1000))
                .filter(|n| *n > 0)
                .max();
            statistics.watched =
                matched
                    .iter()
                    .any(|(_, _, position, duration, _, context, _, _)| {
                        crate::library::watched(
                            &json!({"position":position,"duration":duration}),
                            &serde_json::from_str::<Value>(context).unwrap_or(Value::Null),
                        )
                    });
            let episodes = matched
                .iter()
                .filter(|(_, _, _, _, _, context, _, _)| {
                    serde_json::from_str::<Value>(context)
                        .ok()
                        .and_then(|c| {
                            serde_json::from_value::<viptv_engine::identity::MediaIdentity>(
                                c["engine_identity"].clone(),
                            )
                            .ok()
                        })
                        .is_some_and(|identity| identity.episode.is_some())
                })
                .collect::<Vec<_>>();
            statistics.times_watched = if episodes.is_empty() {
                matched
                    .iter()
                    .try_fold(0u64, |sum, (_, _, _, _, _, _, _, count)| {
                        count.map(|count| sum.saturating_add(count))
                    })
            } else {
                None
            };
            if !episodes.is_empty() {
                statistics.watched_episode_count = Some(
                    episodes
                        .iter()
                        .filter(|(_, _, position, duration, _, context, _, _)| {
                            crate::library::watched(
                                &json!({"position":position,"duration":duration}),
                                &serde_json::from_str::<Value>(context).unwrap_or(Value::Null),
                            )
                        })
                        .count() as u64,
                );
            }
            Entry { card, statistics }
        })
        .collect())
}
