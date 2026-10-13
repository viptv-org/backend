use super::*;
use crate::{playback::PlaybackRecord, presentation::Card};

pub(super) fn public_art(url: Option<&str>) -> Option<String> {
    let value = url?.trim();
    if value.len() > 2048 {
        return None;
    }
    let parsed = url::Url::parse(value).ok()?;
    if !matches!(parsed.scheme(), "http" | "https")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return None;
    }
    Some(value.into())
}

impl Store {
    pub fn clear_history(&mut self, scope: &Scope) -> Result<(), Error> {
        if self.profile(scope)?.is_none() {
            return Err(Error::Forbidden);
        }
        self.db
            .execute(
                "DELETE FROM engine_playback WHERE connection=?1 AND account=?2 AND profile=?3",
                params![scope.connection_id, scope.account_id, scope.profile_id],
            )
            .map_err(|_| Error::Storage)?;
        Ok(())
    }
    pub fn record_playback(
        &mut self,
        scope: &Scope,
        card: &Card,
        position_millis: u64,
        duration_millis: u64,
        now_millis: u64,
    ) -> Result<(), Error> {
        let profile = self.profile(scope)?.ok_or(Error::Forbidden)?;
        if profile.scope.revision != scope.revision {
            return Err(Error::Conflict);
        }
        if !matches!(card.identity.kind.as_str(), "movie" | "series")
            || card.identity.id.is_empty()
            || card.identity.id.len() > 512
            || card.title.chars().count() > 512
            || card.provider_id.len() > 128
            || duration_millis > 7 * 24 * 3600 * 1000
            || position_millis > 7 * 24 * 3600 * 1000
            || (duration_millis > 0 && position_millis > duration_millis.saturating_add(60_000))
            || now_millis == 0
        {
            return Err(Error::InvalidQuery);
        }
        if position_millis == 0 {
            return Ok(());
        }
        let old = self.playback(scope, &card.identity.kind, &card.identity.id)?;
        let completed = duration_millis > 0
            && position_millis.saturating_mul(100) >= duration_millis.saturating_mul(95);
        let mut presentation = card.clone();
        presentation.poster = public_art(card.poster.as_deref());
        presentation.landscape = public_art(card.landscape.as_deref());
        presentation.logo = public_art(card.logo.as_deref());
        presentation.progress = None;
        presentation.added_at_millis = None;
        let times_watched=match &old{None=>Some(u64::from(completed)),Some(old)=>old.times_watched.map(|count|if completed && !(old.duration_millis>0&&old.position_millis.saturating_mul(100)>=old.duration_millis.saturating_mul(95)){count.saturating_add(1)}else{count})};
        let record = PlaybackRecord {
            times_watched,
            presentation: Some(presentation),
            identity: card.identity.clone(),
            parent_identity: card.parent_identity.clone(),
            provider_id: card.provider_id.clone(),
            title: card.title.clone(),
            poster: public_art(card.poster.as_deref()),
            landscape: public_art(card.landscape.as_deref()),
            position_millis,
            duration_millis,
            updated_at_millis: now_millis,
            completed_at_millis: if completed {
                Some(now_millis)
            } else {
                old.and_then(|r| r.completed_at_millis)
            },
        };
        self.db.execute("INSERT INTO engine_playback VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(connection,account,profile,kind,id) DO UPDATE SET updated_at_millis=excluded.updated_at_millis,value=excluded.value",params![scope.connection_id,scope.account_id,scope.profile_id,record.identity.kind,record.identity.id,record.updated_at_millis,serde_json::to_string(&record).map_err(|_|Error::Storage)?]).map_err(|_|Error::Storage)?;
        Ok(())
    }
    pub fn playback(
        &self,
        scope: &Scope,
        kind: &str,
        id: &str,
    ) -> Result<Option<PlaybackRecord>, Error> {
        let raw:Option<String>=self.db.query_row("SELECT value FROM engine_playback WHERE connection=?1 AND account=?2 AND profile=?3 AND kind=?4 AND id=?5",params![scope.connection_id,scope.account_id,scope.profile_id,kind,id],|r|r.get(0)).optional().map_err(|_|Error::Storage)?;
        raw.map(|value| serde_json::from_str(&value).map_err(|_| Error::Storage))
            .transpose()
    }
    pub fn history(&self, scope: &Scope, limit: u32) -> Result<Vec<PlaybackRecord>, Error> {
        let mut q=self.db.prepare("SELECT value FROM engine_playback WHERE connection=?1 AND account=?2 AND profile=?3 ORDER BY updated_at_millis DESC LIMIT ?4").map_err(|_|Error::Storage)?;
        let rows = q
            .query_map(
                params![
                    scope.connection_id,
                    scope.account_id,
                    scope.profile_id,
                    limit.clamp(1, 100)
                ],
                |r| r.get::<_, String>(0),
            )
            .map_err(|_| Error::Storage)?;
        rows.map(|row| {
            row.map_err(|_| Error::Storage)
                .and_then(|raw| serde_json::from_str(&raw).map_err(|_| Error::Storage))
        })
        .collect()
    }
}
