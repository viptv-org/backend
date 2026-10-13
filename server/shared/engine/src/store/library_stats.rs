use super::*;
use crate::presentation::Card;

impl Store {
    pub fn library_entries(&self, scope: &Scope) -> Result<Vec<crate::library::Entry>, Error> {
        self.library_scope(scope, false)?;
        let mut query=self.db.prepare("SELECT value,added_at_millis FROM engine_favorites WHERE connection=?1 AND account=?2 AND profile=?3 ORDER BY kind,id LIMIT 10001").map_err(|_|Error::Storage)?;
        let rows = query
            .query_map(
                params![scope.connection_id, scope.account_id, scope.profile_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<u64>>(1)?)),
            )
            .map_err(|_| Error::Storage)?;
        let mut cards = Vec::new();
        for row in rows {
            let (value, added) = row.map_err(|_| Error::Storage)?;
            let mut card: Card = serde_json::from_str(&value).map_err(|_| Error::Storage)?;
            card.added_at_millis = added;
            card.progress = None;
            cards.push(card);
        }
        if cards.len() > 10000 {
            return Err(Error::InvalidQuery);
        }
        let mut history=self.db.prepare("SELECT value FROM engine_playback WHERE connection=?1 AND account=?2 AND profile=?3 LIMIT 50001").map_err(|_|Error::Storage)?;
        let rows = history
            .query_map(
                params![scope.connection_id, scope.account_id, scope.profile_id],
                |r| r.get::<_, String>(0),
            )
            .map_err(|_| Error::Storage)?;
        let records = rows
            .map(|row| {
                row.map_err(|_| Error::Storage).and_then(|value| {
                    serde_json::from_str::<crate::playback::PlaybackRecord>(&value)
                        .map_err(|_| Error::Storage)
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if records.len() > 50000 {
            return Err(Error::InvalidQuery);
        }
        let entries = cards
            .into_iter()
            .map(|card| {
                let mut statistics =
                    crate::library::LibraryStatistics::empty(card.identity.clone());
                let matched = records
                    .iter()
                    .filter(|r| {
                        (r.identity.id == card.identity.id && r.identity.kind == card.identity.kind)
                            || r.parent_identity.as_ref().is_some_and(|p| {
                                p.id == card.identity.id && p.kind == card.identity.kind
                            })
                    })
                    .collect::<Vec<_>>();
                statistics.last_watched_millis = matched
                    .iter()
                    .filter(|r| r.position_millis > 0 || r.completed_at_millis.is_some())
                    .map(|r| r.updated_at_millis)
                    .filter(|n| *n > 0)
                    .max();
                statistics.watched = matched.iter().any(|r| r.completed_at_millis.is_some());
                statistics.times_watched = (!matched.iter().any(|r| r.identity.episode.is_some()))
                    .then(|| matched.iter().try_fold(0u64, |sum, r| r.times_watched.map(|n| sum.saturating_add(n))))
                    .flatten();
                if card.identity.kind == "series" {
                    statistics.watched_episode_count = Some(
                        matched
                            .iter()
                            .filter(|r| {
                                r.identity.episode.is_some() && r.completed_at_millis.is_some()
                            })
                            .count() as u64,
                    );
                }
                crate::library::Entry { card, statistics }
            })
            .collect();
        Ok(entries)
    }
}
