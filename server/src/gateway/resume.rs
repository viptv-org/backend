use crate::{account_api::Error, Connection, OptionalExtension};

pub(super) fn position(
    db: &Connection,
    profile: i64,
    kind: &str,
    media_id: Option<&str>,
) -> Result<f64, Error> {
    let (position, reason) = outcome(db, profile, kind, media_id)?;
    let tag = media_id.map(|id| super::diagnostics::record_tag("shared", kind, id)).unwrap_or_default();
    let record = if reason == "no_record" { "" } else { tag.as_str() };
    super::diagnostics::resume(reason, position, record, &tag);
    Ok(position)
}

fn outcome(
    db: &Connection,
    profile: i64,
    kind: &str,
    media_id: Option<&str>,
) -> Result<(f64, &'static str), Error> {
    let Some(id) = media_id else {
        return Ok((0.0, "no_record"));
    };
    if kind == "live" {
        return Ok((0.0, "no_record"));
    }
    let row=db.query_row("SELECT position,duration,context FROM progress WHERE profile_id=?1 AND type=?2 AND id=?3",rusqlite::params![profile,kind,id],|r|Ok((r.get::<_,f64>(0)?,r.get::<_,f64>(1)?,r.get::<_,String>(2)?))).optional().map_err(|_|Error::Code("provider_storage_unavailable"))?;
    let Some((position, duration, context)) = row else {
        return Ok((0.0, "no_record"));
    };
    let completed = serde_json::from_str::<serde_json::Value>(&context)
        .ok()
        .is_some_and(|v| v["watched_override"] == true);
    if !position.is_finite()
        || position <= 0.0
        || position > 1e9
        || completed
        || (duration.is_finite() && duration > 0.0 && position / duration >= 0.95)
    {
        return Ok((0.0, "not_resumable"));
    }
    Ok((position, "found"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_resume_lookup_reports_its_reason() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE progress(profile_id INTEGER,type TEXT,id TEXT,position REAL,duration REAL,context TEXT);
            INSERT INTO progress VALUES(1,'series','paused',30,100,'{}');
            INSERT INTO progress VALUES(1,'series','finished',96,100,'{}');").unwrap();
        for (id, expected) in [(Some("paused"), (30.0, "found")), (Some("finished"), (0.0, "not_resumable")), (Some("missing"), (0.0, "no_record")), (None, (0.0, "no_record"))] {
            assert_eq!(outcome(&db, 1, "series", id).ok(), Some(expected));
        }
    }
    #[test]
    fn opaque_episode_resume_is_selected_profile_exact_id_and_server_completion_bound() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE progress(profile_id INTEGER,type TEXT,id TEXT,position REAL,duration REAL,context TEXT);
            INSERT INTO progress VALUES(1,'series','opaque-episode',12.5,100,'{}');
            INSERT INTO progress VALUES(2,'series','opaque-episode',70,100,'{}');
            INSERT INTO progress VALUES(1,'series','mapped-imdb-episode',80,100,'{}');
            INSERT INTO progress VALUES(1,'movie','unknown-duration',20,0,'{}');
            INSERT INTO progress VALUES(1,'movie','completed',95,100,'{}');
            INSERT INTO progress VALUES(1,'movie','manual-watched',12,100,'{\"watched_override\":true}');").unwrap();
        assert_eq!(
            position(&db, 1, "series", Some("opaque-episode")).ok(),
            Some(12.5)
        );
        assert_eq!(
            position(&db, 2, "series", Some("opaque-episode")).ok(),
            Some(70.0)
        );
        assert_eq!(
            position(&db, 1, "movie", Some("opaque-episode")).ok(),
            Some(0.0)
        );
        assert_eq!(position(&db, 1, "series", None).ok(), Some(0.0));
        assert_eq!(
            position(&db, 1, "movie", Some("unknown-duration")).ok(),
            Some(20.0)
        );
        for id in ["completed", "manual-watched"] {
            assert_eq!(position(&db, 1, "movie", Some(id)).ok(), Some(0.0));
        }
        assert_eq!(
            position(&db, 1, "live", Some("opaque-episode")).ok(),
            Some(0.0)
        );
    }
}
