use super::*;
use crate::presentation::{Card, CatalogPage};

impl Store {
    pub(super) fn library_scope(&self, scope: &Scope, mutate: bool) -> Result<(), Error> {
        let profile = self.profile(scope)?.ok_or(Error::Forbidden)?;
        if mutate && profile.scope.revision != scope.revision {
            return Err(Error::Conflict);
        }
        Ok(())
    }
    pub fn library_revision(&self, scope: &Scope) -> Result<u64, Error> {
        self.library_scope(scope, false)?;
        self.db.query_row("SELECT revision FROM engine_library_revisions WHERE connection=?1 AND account=?2 AND profile=?3",params![scope.connection_id,scope.account_id,scope.profile_id],|r|r.get(0)).optional().map_err(|_|Error::Storage).map(|v|v.unwrap_or(0))
    }
    pub fn favorites(&self, scope: &Scope, offset: u32, limit: u32) -> Result<CatalogPage, Error> {
        self.library_scope(scope, false)?;
        let limit = limit.clamp(1, 100);
        let total: u64 = self.db.query_row("SELECT count(*) FROM engine_favorites WHERE connection=?1 AND account=?2 AND profile=?3", params![scope.connection_id,scope.account_id,scope.profile_id], |r|r.get(0)).map_err(|_|Error::Storage)?;
        let mut query = self.db.prepare("SELECT value,added_at_millis FROM engine_favorites WHERE connection=?1 AND account=?2 AND profile=?3 ORDER BY added_at_millis DESC,title COLLATE NOCASE,kind,id LIMIT ?4 OFFSET ?5").map_err(|_|Error::Storage)?;
        let rows = query
            .query_map(
                params![
                    scope.connection_id,
                    scope.account_id,
                    scope.profile_id,
                    limit,
                    offset
                ],
                |r| Ok((r.get::<_,String>(0)?,r.get::<_,Option<u64>>(1)?)),
            )
            .map_err(|_| Error::Storage)?;
        let cards = rows
            .map(|row| {
                row.map_err(|_| Error::Storage)
                    .and_then(|(value,added)|{let mut card:Card=serde_json::from_str(&value).map_err(|_|Error::Storage)?;card.added_at_millis=added;Ok(card)})
            })
            .collect::<Result<Vec<Card>, _>>()?;
        let next = u64::from(offset) + u64::from(limit);
        Ok(CatalogPage {
            cards,
            next_cursor: (next < total).then(|| next.to_string()),
            complete: next >= total,
        })
    }
    pub fn save_favorite(&mut self, scope: &Scope, card: &Card) -> Result<(), Error> {
        self.library_scope(scope, true)?;
        card.validate_library()?;
        let mut card = card.clone();
        card.progress = None;
        card.added_at_millis = None;
        // Provider media authorization must not persist in image URLs.
        card.poster = super::playback::public_art(card.poster.as_deref());
        card.landscape = super::playback::public_art(card.landscape.as_deref());
        card.logo = super::playback::public_art(card.logo.as_deref());
        let value = serde_json::to_string(&card).map_err(|_| Error::Storage)?;
        let tx = self.db.transaction().map_err(|_| Error::Storage)?;
        let changed = tx.execute("INSERT INTO engine_favorites(connection,account,profile,kind,id,title,value,added_at_millis) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(connection,account,profile,kind,id) DO UPDATE SET title=excluded.title,value=excluded.value WHERE engine_favorites.value != excluded.value",params![scope.connection_id,scope.account_id,scope.profile_id,card.identity.kind,card.identity.id,card.title,value,std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|_|Error::Storage)?.as_millis().min(i64::MAX as u128) as i64]).map_err(|_|Error::Storage)?;
        if changed > 0 {
            Self::advance_library(&tx, scope)?;
        }
        tx.commit().map_err(|_| Error::Storage)
    }
    pub fn remove_favorite(&mut self, scope: &Scope, kind: &str, id: &str) -> Result<(), Error> {
        self.library_scope(scope, true)?;
        if !matches!(kind, "movie" | "series")
            || id.is_empty()
            || id.len() > 512
            || id.chars().any(char::is_control)
        {
            return Err(Error::InvalidQuery);
        }
        let tx = self.db.transaction().map_err(|_| Error::Storage)?;
        let changed = tx.execute("DELETE FROM engine_favorites WHERE connection=?1 AND account=?2 AND profile=?3 AND kind=?4 AND id=?5",params![scope.connection_id,scope.account_id,scope.profile_id,kind,id]).map_err(|_|Error::Storage)?;
        if changed > 0 {
            Self::advance_library(&tx, scope)?;
        }
        tx.commit().map_err(|_| Error::Storage)
    }
    fn advance_library(tx: &rusqlite::Transaction<'_>, scope: &Scope) -> Result<(), Error> {
        tx.execute("INSERT INTO engine_library_revisions VALUES(?1,?2,?3,1) ON CONFLICT(connection,account,profile) DO UPDATE SET revision=revision+1",params![scope.connection_id,scope.account_id,scope.profile_id]).map(|_|()).map_err(|_|Error::Storage)
    }
}
