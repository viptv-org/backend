use super::*;
use crate::presentation::Card;

/// Configured catalogs can contain personalized data. Never share their cache
/// across accounts, profiles or assignment revisions, or persist request URLs.
impl Store {
    pub fn cached_catalog(
        &self,
        scope: &Scope,
        row: &str,
        now: i64,
    ) -> Result<Option<Vec<Card>>, Error> {
        let current = self.profile(scope)?.ok_or(Error::Forbidden)?;
        if current.scope.revision != scope.revision {
            return Err(Error::Conflict);
        }
        let value: Option<String> = self.db.query_row(
            "SELECT value FROM engine_catalog_cache WHERE connection=?1 AND account=?2 AND profile=?3 AND revision=?4 AND row_id=?5 AND expires_at>?6 AND expires_at<=?6+300",
            params![scope.connection_id, scope.account_id, scope.profile_id, scope.revision, row, now],
            |r| r.get(0),
        ).optional().map_err(|_| Error::Storage)?;
        // A damaged disposable cache is a miss, not a broken Home screen.
        Ok(value.and_then(|raw| serde_json::from_str(&raw).ok()))
    }

    pub fn cache_catalog(
        &mut self,
        scope: &Scope,
        row: &str,
        cards: &[Card],
        now: i64,
    ) -> Result<(), Error> {
        let current = self.profile(scope)?.ok_or(Error::Forbidden)?;
        if current.scope.revision != scope.revision {
            return Err(Error::Conflict);
        }
        if row.is_empty() || row.len() > 512 || now < 0 {
            return Err(Error::InvalidQuery);
        }
        let cards: Vec<_> = cards
            .iter()
            .take(24)
            .cloned()
            .map(|mut card| {
                card.progress = None;
                card
            })
            .collect();
        let value = serde_json::to_string(&cards).map_err(|_| Error::Storage)?;
        if value.len() > 256 * 1024 {
            return Err(Error::InvalidQuery);
        }
        let expires = now.checked_add(300).ok_or(Error::InvalidQuery)?;
        let tx = self.db.transaction().map_err(|_| Error::Storage)?;
        tx.execute(
            "DELETE FROM engine_catalog_cache WHERE connection=?1 AND account=?2 AND profile=?3 AND (revision<>?4 OR expires_at<=?5)",
            params![scope.connection_id, scope.account_id, scope.profile_id, scope.revision, now],
        ).map_err(|_| Error::Storage)?;
        tx.execute(
            "INSERT INTO engine_catalog_cache VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(connection,account,profile,row_id) DO UPDATE SET revision=excluded.revision,expires_at=excluded.expires_at,value=excluded.value",
            params![scope.connection_id, scope.account_id, scope.profile_id, scope.revision, row, expires, value],
        ).map_err(|_| Error::Storage)?;
        tx.execute(
            "DELETE FROM engine_catalog_cache WHERE rowid IN (SELECT rowid FROM engine_catalog_cache WHERE connection=?1 AND account=?2 AND profile=?3 ORDER BY expires_at DESC,rowid DESC LIMIT -1 OFFSET 12)",
            params![scope.connection_id, scope.account_id, scope.profile_id],
        ).map_err(|_| Error::Storage)?;
        tx.commit().map_err(|_| Error::Storage)
    }
}

impl Store {
    /// Related titles come from this profile's currently assigned, already fetched catalogs.
    pub fn related_catalog_cards(
        &self,
        scope: &Scope,
        item: &Card,
        now: i64,
    ) -> Result<Vec<Card>, Error> {
        let current = self.profile(scope)?.ok_or(Error::Forbidden)?;
        if current.scope.revision != scope.revision {
            return Err(Error::Conflict);
        }
        let Some(metadata) = item.metadata.as_ref().filter(|m| !m.genres.is_empty()) else {
            return Ok(vec![]);
        };
        let mut statement=self.db.prepare("SELECT value FROM engine_catalog_cache WHERE connection=?1 AND account=?2 AND profile=?3 AND revision=?4 AND expires_at>?5 AND expires_at<=?5+300 ORDER BY rowid LIMIT 12").map_err(|_|Error::Storage)?;
        let values = statement
            .query_map(
                params![
                    scope.connection_id,
                    scope.account_id,
                    scope.profile_id,
                    scope.revision,
                    now
                ],
                |r| r.get::<_, String>(0),
            )
            .map_err(|_| Error::Storage)?;
        let mut cards = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for value in values {
            let Ok(value) = value else {
                continue;
            };
            let Ok(cached) = serde_json::from_str::<Vec<Card>>(&value) else {
                continue;
            };
            for card in cached {
                if card.identity.id == item.identity.id
                    || card.identity.kind != item.identity.kind
                    || card.identity.episode.is_some()
                    || !seen.insert(card.identity.id.clone())
                {
                    continue;
                }
                let overlap = card.metadata.as_ref().map_or(0, |m| {
                    m.genres
                        .iter()
                        .filter(|g| {
                            metadata
                                .genres
                                .iter()
                                .any(|genre| genre.eq_ignore_ascii_case(g))
                        })
                        .count()
                });
                if overlap > 0 {
                    cards.push((overlap, card));
                }
            }
        }
        cards.sort_by_key(|(overlap, _)| std::cmp::Reverse(*overlap));
        Ok(cards.into_iter().take(24).map(|(_, card)| card).collect())
    }
}
