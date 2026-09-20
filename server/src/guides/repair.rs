use super::*;

pub(super) fn channel(db: &Connection, id: &str) -> Result<Value, ApiError> {
    let data: String = db
        .query_row("SELECT data FROM family_channels WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .map_err(db_error)?;
    serde_json::from_str(&data).map_err(|_| "Family channel invalid".into())
}
pub(super) fn programmes(db: &Connection, id: &str) -> Result<Vec<Value>, ApiError> {
    let mut q=db.prepare("SELECT f.start,f.end,f.data,f.source_id,f.guide_id,s.updated_at FROM family_programmes f JOIN guide_sources s ON s.id=f.source_id AND s.enabled=1 JOIN guide_mappings m ON m.channel_id=f.channel_id AND m.source_id=f.source_id AND m.guide_id=f.guide_id JOIN guide_channels g ON g.source_id=m.source_id AND g.guide_id=m.guide_id AND g.name=m.observed_name WHERE f.channel_id=?1 AND f.end>?2 ORDER BY f.start,f.end LIMIT 2000").map_err(db_error)?;
    let result = q
        .query_map(params![id, util::now()], |r| {
            let mut data: Value =
                serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or(json!({}));
            data["start"] = json!(r.get::<_, i64>(0)?);
            data["end"] = json!(r.get::<_, i64>(1)?);
            data["source_id"] = json!(r.get::<_, i64>(3)?);
            data["guide_id"] = json!(r.get::<_, String>(4)?);
            data["source_updated_at"] = json!(r.get::<_, Option<i64>>(5)?);
            Ok(data)
        })
        .map_err(db_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error);
    result
}
pub(super) fn coverage(programs: &[Value], now: i64) -> Value {
    let mut cursor = now;
    let mut covered24 = 0;
    let mut covered72 = 0;
    let mut gaps = Vec::new();
    let mut overlaps = 0;
    for p in programs {
        let start = p["start"].as_i64().unwrap_or(0).max(now);
        let end = p["end"].as_i64().unwrap_or(0);
        if end <= now {
            continue;
        }
        if start > cursor && cursor < now + 72 * 3600 && gaps.len() < 50 {
            gaps.push(json!({"start":cursor,"end":start.min(now+72*3600)}));
        }
        if start < cursor {
            overlaps += 1;
        }
        let from = start.max(cursor);
        covered24 += (end.min(now + 86400) - from).max(0);
        covered72 += (end.min(now + 72 * 3600) - from).max(0);
        cursor = cursor.max(end);
    }
    if cursor < now + 72 * 3600 && gaps.len() < 50 {
        gaps.push(json!({"start":cursor,"end":now+72*3600}));
    }
    json!({"current_programme":programs.iter().find(|p|p["start"].as_i64().unwrap_or(i64::MAX)<=now&&p["end"].as_i64().unwrap_or(0)>now),"coverage_24_hours":covered24 as f64/3600.0,"coverage_72_hours":covered72 as f64/3600.0,"gaps":gaps,"overlaps":overlaps,"needs_attention":covered24<86400,"horizon":cursor})
}
pub(super) fn automatic_mappings(db: &Connection) -> Result<(), ApiError> {
    let mut q = db
        .prepare(
            "SELECT id,data FROM family_channels WHERE json_extract(data,'$.enabled')=1 LIMIT 1000",
        )
        .map_err(db_error)?;
    let channels = q
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(db_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error)?;
    let mut suggestions = Vec::new();
    for (id, data) in &channels {
        let value: Value = serde_json::from_str(data).map_err(|_| "Family channel invalid")?;
        let needle = format!(
            "%{}%",
            value["network"]
                .as_str()
                .unwrap_or("")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let mut q=db.prepare("SELECT c.source_id,c.guide_id,c.name FROM guide_channels c JOIN guide_sources s ON s.id=c.source_id AND s.enabled=1 WHERE c.name LIKE ?1 ESCAPE '\\' ORDER BY c.source_id,c.guide_id LIMIT 100").map_err(db_error)?;
        for row in q
            .query_map([needle], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(db_error)?
        {
            let (source, guide, name) = row.map_err(db_error)?;
            if lineup::matching::compatible_guide(&value, &name, false) {
                suggestions.push((id.clone(), source, guide, name, false));
            }
        }
    }
    // A provider-scoped EPG ID is trusted only through a currently verified
    // stream mapping; identical IDs on unrelated providers never imply identity.
    for (id, data) in &channels {
        let value: Value = serde_json::from_str(data).map_err(|_| "Family channel invalid")?;
        let mut q = db.prepare("SELECT DISTINCT g.source_id,g.guide_id,g.name FROM family_candidates c JOIN provider_live l ON l.id=c.live_id JOIN family_channels f ON f.id=c.channel_id JOIN guide_sources s ON s.provider_id=l.provider_id AND s.enabled=1 JOIN guide_channels g ON g.source_id=s.id AND (g.guide_id=l.epg_channel_id OR g.guide_id='xtream-stream:'||l.stream_id) WHERE c.channel_id=?1 AND EXISTS(SELECT 1 FROM json_each(f.data,'$.candidates') saved WHERE json_extract(saved.value,'$.id')=l.id AND json_extract(saved.value,'$.name')=l.name) LIMIT 100").map_err(db_error)?;
        let rows = q
            .query_map([id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(db_error)?;
        for row in rows {
            let (source, guide, name) = row.map_err(db_error)?;
            if lineup::matching::compatible_guide(&value, &name, true) {
                suggestions.retain(|(family, sid, gid, _, _)| {
                    !(family == id && *sid == source && gid == &guide)
                });
                suggestions.push((id.clone(), source, guide, name, true));
            }
        }
    }
    let mut counts = HashMap::new();
    for (_, source, guide, _, _) in &suggestions {
        *counts.entry((*source, guide.clone())).or_insert(0) += 1;
    }
    let tx = db.unchecked_transaction().map_err(db_error)?;
    tx.execute("DELETE FROM guide_mappings WHERE pinned=0", [])
        .map_err(db_error)?;
    for (id, source, guide, name, verified) in suggestions {
        if counts[&(source, guide.clone())] != 1 {
            continue;
        }
        tx.execute("INSERT OR IGNORE INTO guide_mappings(channel_id,source_id,guide_id,observed_name,pinned,verified,priority) SELECT ?1,?2,?3,?4,0,?5,100 WHERE NOT EXISTS(SELECT 1 FROM guide_rejections WHERE channel_id=?1 AND source_id=?2 AND guide_id=?3)",params![id,source,guide,name,verified]).map_err(db_error)?;
    }
    tx.commit().map_err(db_error)?;
    Ok(())
}
pub(super) fn repair(db: &mut Connection, id: &str, apply: bool) -> Result<Value, ApiError> {
    repair_guarded(db, id, apply, None)
}
pub(super) fn repair_guarded(
    db: &mut Connection,
    id: &str,
    apply: bool,
    guard: Option<&RunLease>,
) -> Result<Value, ApiError> {
    if let Some(guard) = guard {
        guard.validate(db)?;
    }
    let family = channel(db, id)?;
    let existing = programmes(db, id)?;
    let mut intervals = existing
        .iter()
        .map(|p| (p["start"].as_i64().unwrap(), p["end"].as_i64().unwrap()))
        .collect::<Vec<_>>();
    let mut additions = Vec::new();
    {
        let mut q=db.prepare("SELECT p.source_id,p.guide_id,p.start,p.end,p.data,g.name,(m.pinned OR m.verified) FROM guide_mappings m JOIN guide_sources s ON s.id=m.source_id AND s.enabled=1 JOIN guide_channels g ON g.source_id=m.source_id AND g.guide_id=m.guide_id AND g.name=m.observed_name JOIN source_programmes p ON p.source_id=m.source_id AND p.guide_id=m.guide_id WHERE m.channel_id=?1 AND p.end>?2 ORDER BY m.pinned DESC,m.priority,s.updated_at DESC,p.source_id,p.start,p.end LIMIT 10000").map_err(db_error)?;
        for row in q
            .query_map(params![id, util::now()], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, bool>(6)?,
                ))
            })
            .map_err(db_error)?
        {
            let (source, guide, start, end, data, name, pinned) = row.map_err(db_error)?;
            if !lineup::matching::compatible_guide(&family, &name, pinned)
                || intervals.iter().any(|(a, b)| start < *b && end > *a)
            {
                continue;
            }
            intervals.push((start, end));
            let data: Value = serde_json::from_str(&data).map_err(|_| "Stored guide invalid")?;
            additions.push(json!({"source_id":source,"guide_id":guide,"start":start,"end":end,"programme":data}));
        }
    }
    let revision = format!(
        "{:x}",
        Sha256::digest(json!([id, existing, additions]).to_string())
    );
    if apply {
        let tx = db.transaction().map_err(db_error)?;
        tx.execute(
            "DELETE FROM family_programmes WHERE end<?1",
            [util::now() - 86400],
        )
        .map_err(db_error)?;
        for row in &additions {
            tx.execute("INSERT OR IGNORE INTO family_programmes(channel_id,source_id,guide_id,start,end,data) VALUES(?1,?2,?3,?4,?5,?6)",params![id,row["source_id"].as_i64(),row["guide_id"].as_str(),row["start"].as_i64(),row["end"].as_i64(),row["programme"].to_string()]).map_err(db_error)?;
        }
        event(&tx, None, Some(id), "compatible_gaps_filled")?;
        if let Some(guard) = guard {
            guard.validate(&tx)?;
        }
        tx.commit().map_err(db_error)?;
    }
    Ok(
        json!({"channel_id":id,"revision":revision,"additions":additions,"preserved":existing.len(),"applied":apply}),
    )
}
pub(crate) fn read(db: &Connection, id: &str) -> Result<Value, ApiError> {
    let rules = policy(db)?;
    let timezone: chrono_tz::Tz = rules
        .timezone
        .parse()
        .map_err(|_| "Guide timezone invalid")?;
    let mut programs = programmes(db, id)?;
    for p in &mut programs {
        if let Some(date) = DateTime::<Utc>::from_timestamp(p["start"].as_i64().unwrap_or(0), 0) {
            let local = date.with_timezone(&timezone);
            p["display_time"] = json!(local.format("%-I:%M %p %Z").to_string());
            p["display_date"] = json!(local.format("%Y-%m-%d").to_string());
        }
        p["id"] = json!(format!("{id}:{}", p["start"]));
    }
    let now = util::now();
    let timeline: Vec<Value> = (0..54).filter_map(|slot| {
        let start = now / 1800 * 1800 + slot * 1800;
        let local = DateTime::<Utc>::from_timestamp(start, 0)?.with_timezone(&timezone);
        Some(json!({"start":start,"display_time":local.format("%-I:%M %p %Z").to_string(),"display_date":local.format("%a %b %-d").to_string()}))
    }).collect();
    let status = coverage(&programs, now);
    programs.truncate(100);
    Ok(
        json!({"programs":programs,"timezone":rules.timezone,"timeline":timeline,"coverage":status,"missing":status["current_programme"].is_null()}),
    )
}
