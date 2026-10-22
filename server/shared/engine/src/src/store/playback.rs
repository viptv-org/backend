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
        self.db.execute("DELETE FROM engine_scoped_records WHERE connection=?1 AND account=?2 AND profile=?3 AND record_kind='playback'",params![scope.connection_id,scope.account_id,scope.profile_id]).map_err(|_|Error::Storage)?;
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
        if card.metadata_origin.is_some() {
            return self.record_scoped_playback(
                scope,
                card,
                position_millis,
                duration_millis,
                now_millis,
                false,
            );
        }
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
        let times_watched = match &old {
            None => Some(u64::from(completed)),
            Some(old) => old.times_watched.map(|count| {
                if completed
                    && !(old.duration_millis > 0
                        && old.position_millis.saturating_mul(100)
                            >= old.duration_millis.saturating_mul(95))
                {
                    count.saturating_add(1)
                } else {
                    count
                }
            }),
        };
        let record = PlaybackRecord {
            metadata_origin: None,
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
        self.library_scope(scope, false)?;
        let shared = self.playback_key(
            scope,
            &super::scoped_library::Key("shared".into(), kind.into(), id.into()),
        )?;
        let legacy = self.playback_key(
            scope,
            &super::scoped_library::Key("legacy".into(), kind.into(), id.into()),
        )?;
        Ok(match (shared, legacy) {
            (Some(a), Some(b)) => Some(if a.updated_at_millis >= b.updated_at_millis {
                a
            } else {
                b
            }),
            (a, b) => a.or(b),
        })
    }
    pub fn history(&self, scope: &Scope, limit: u32) -> Result<Vec<PlaybackRecord>, Error> {
        self.library_scope(scope, false)?;
        let sql = format!(
            "SELECT value,legacy FROM (SELECT p.value,p.updated_at_millis,p.kind,p.id AS identity_id,'legacy' AS identity_scope,1 AS legacy FROM engine_playback p WHERE p.connection=?1 AND p.account=?2 AND p.profile=?3 AND NOT EXISTS(SELECT 1 FROM engine_scoped_records s WHERE s.connection=p.connection AND s.account=p.account AND s.profile=p.profile AND s.record_kind='playback' AND s.identity_scope='shared' AND s.kind=p.kind AND s.identity_id=p.id AND s.updated_at_millis>=p.updated_at_millis AND {}) UNION ALL SELECT s.value,s.updated_at_millis,s.kind,s.identity_id,s.identity_scope,0 AS legacy FROM engine_scoped_records s WHERE s.connection=?1 AND s.account=?2 AND s.profile=?3 AND s.record_kind='playback' AND NOT(s.identity_scope='shared' AND EXISTS(SELECT 1 FROM engine_playback p WHERE p.connection=s.connection AND p.account=s.account AND p.profile=s.profile AND p.kind=s.kind AND p.id=s.identity_id AND p.updated_at_millis>s.updated_at_millis AND {})))ORDER BY updated_at_millis DESC,kind,identity_id,identity_scope LIMIT ?4",
            super::scoped_library::legacy_shared_sql("p"),
            super::scoped_library::legacy_shared_sql("p")
        );
        let mut query = self.db.prepare(&sql).map_err(|_| Error::Storage)?;
        let rows = query
            .query_map(
                params![
                    scope.connection_id,
                    scope.account_id,
                    scope.profile_id,
                    limit.clamp(1, 100)
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)),
            )
            .map_err(|_| Error::Storage)?;
        rows.map(|row| {
            let (value, legacy) = row.map_err(|_| Error::Storage)?;
            let mut record: PlaybackRecord =
                serde_json::from_str(&value).map_err(|_| Error::Storage)?;
            if legacy {
                record.metadata_origin = None;
                if let Some(card) = &mut record.presentation {
                    card.metadata_origin = None;
                }
            }
            Ok(record)
        })
        .collect()
    }
}
