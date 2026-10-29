use super::*;
use viptv_engine::presentation::Card;
pub(super) fn millis(value: &Value) -> Option<u64> {
    u64::try_from(
        chrono::DateTime::parse_from_rfc3339(value.as_str()?)
            .ok()?
            .timestamp_millis(),
    )
    .ok()
}
fn receipt(position: f64, duration: f64, updated: u64, context: &str) -> String {
    json!([position, duration, updated, context]).to_string()
}
pub(super) fn apply(
    db: &Connection,
    profile: i64,
    card: &Card,
    parent: Option<&Card>,
    position: f64,
    duration: f64,
    updated: u64,
    completed: bool,
    revision: u64,
) -> Result<bool, ApiError> {
    if !position.is_finite()
        || !duration.is_finite()
        || position < 0.0
        || duration < 0.0
        || updated == 0
    {
        return Ok(false);
    }
    let mut card = card.clone();
    let mut parent = parent.cloned();
    let original = parent.as_ref().unwrap_or(&card);
    let mapped:Option<String>=db.query_row("SELECT local_id FROM simkl_import_entries WHERE profile_id=?1 AND remote_kind=?2 AND remote_id=?3",params![profile,original.identity.kind,original.identity.id],|r|r.get(0)).optional().map_err(db_error)?;
    if let Some(mapped) = mapped.filter(|id| id != &original.identity.id) {
        if let Some(parent) = parent.as_mut() {
            if let Some(episode) = card.identity.episode.as_ref() {
                card.identity.id = if let Some(season) = episode.season {
                    format!("{mapped}:{season}:{}", episode.episode)
                } else {
                    format!("{mapped}:{}:{}", episode.order, episode.episode)
                };
            }
            parent.identity.id = mapped;
            card.parent_identity = Some(parent.identity.clone());
        } else {
            card.identity.id = mapped;
        }
    }
    let parent = parent.as_ref();
    let id = &card.identity.id;
    let kind = &card.identity.kind;
    let removed:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM library_history_removals WHERE profile_id=?1 AND type=?2 AND id=?3) OR EXISTS(SELECT 1 FROM simkl_import_history_floors WHERE profile_id=?1 AND cutoff_millis>=?4)",params![profile,kind,id,updated],|r|r.get(0)).map_err(db_error)?;
    if removed {
        return Ok(false);
    }
    let current:Option<(f64,f64,u64,String)>=db.query_row("SELECT position,duration,updated_at,context FROM progress WHERE profile_id=?1 AND type=?2 AND id=?3",params![profile,kind,id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(db_error)?;
    let owned:Option<(String,u64)>=db.query_row("SELECT receipt,source_updated_at_millis FROM simkl_import_progress WHERE profile_id=?1 AND type=?2 AND id=?3",params![profile,kind,id],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(db_error)?;
    if let Some((p, d, u, c)) = &current {
        let context: Value = serde_json::from_str(c).unwrap_or(Value::Null);
        if context["simkl_import"]["link_revision"].as_u64() != Some(revision) {
            return Ok(false);
        }
        if owned
            .as_ref()
            .is_none_or(|(r, t)| r != &receipt(*p, *d, *u, c) || *t >= updated)
        {
            return Ok(false);
        }
    }
    let title = parent.map(|p| p.identity.id.as_str()).unwrap_or(id);
    let latest:Option<u64>=db.query_row("SELECT MAX(updated_at) FROM progress WHERE profile_id=?1 AND type=?2 AND (id=?3 OR title_id=?3)",params![profile,kind,title],|r|r.get(0)).map_err(db_error)?;
    if !completed && latest.is_some_and(|t| t.saturating_mul(1000) >= updated) {
        return Ok(false);
    }
    let context=json!({"engine_identity":card.identity,"engine_parent_identity":parent.map(|p|&p.identity),"engine_provider_id":"simkl","background":card.landscape,"series_id":parent.map(|p|&p.identity.id),"season":card.identity.episode.as_ref().and_then(|e|e.season),"episode":card.identity.episode.as_ref().map(|e|e.episode),"watched_override":completed,"simkl_import":{"link_revision":revision,"source_updated_at_millis":updated,"position_basis":if completed{"recorded_completion"}else{"provider_percentage"}}}).to_string();
    let seconds = updated / 1000;
    db.execute("INSERT INTO progress(profile_id,id,type,name,poster,position,duration,updated_at,context,title_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT(profile_id,type,id) DO UPDATE SET position=excluded.position,duration=excluded.duration,updated_at=excluded.updated_at,context=excluded.context",params![profile,id,kind,parent.unwrap_or(&card).title,card.poster,position,duration,seconds,context,title]).map_err(db_error)?;
    db.execute("INSERT INTO simkl_import_progress VALUES(?1,?2,?3,?4,?5) ON CONFLICT(profile_id,type,id) DO UPDATE SET receipt=excluded.receipt,source_updated_at_millis=excluded.source_updated_at_millis",params![profile,kind,id,receipt(position,duration,seconds,&context),updated]).map_err(db_error)?;
    Ok(true)
}
