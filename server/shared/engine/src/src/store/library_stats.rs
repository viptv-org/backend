use super::*;

impl Store {
    pub fn library_entries(&self, scope: &Scope) -> Result<Vec<crate::library::Entry>, Error> {
        self.library_scope(scope, false)?;
        let favorites = self.favorite_records(scope)?;
        let records = self.playback_records(scope)?;
        let mut by_key = std::collections::BTreeMap::new();
        for saved in &records {
            by_key
                .entry(saved.key.clone())
                .or_insert_with(Vec::new)
                .push(&saved.value);
            if let Some(parent) = super::scoped_library::parent_key(saved)? {
                if parent != saved.key {
                    by_key
                        .entry(parent)
                        .or_insert_with(Vec::new)
                        .push(&saved.value);
                }
            }
        }
        let entries = favorites
            .into_iter()
            .map(|saved| {
                let card = saved.value;
                let mut statistics =
                    crate::library::LibraryStatistics::empty(card.identity.clone());
                statistics.metadata_origin = card.metadata_origin.clone();
                let matched = by_key.get(&saved.key).cloned().unwrap_or_default();
                statistics.last_watched_millis = matched
                    .iter()
                    .filter(|r| r.position_millis > 0 || r.completed_at_millis.is_some())
                    .map(|r| r.updated_at_millis)
                    .filter(|n| *n > 0)
                    .max();
                statistics.watched = matched.iter().any(|r| r.completed_at_millis.is_some());
                statistics.times_watched = (!matched.iter().any(|r| r.identity.episode.is_some()))
                    .then(|| {
                        matched.iter().try_fold(0u64, |sum, r| {
                            r.times_watched.map(|n| sum.saturating_add(n))
                        })
                    })
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
