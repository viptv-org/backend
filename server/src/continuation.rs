//! Episode identity and viewing-queue state. No media URL or decoder ownership lives here.
use super::*;
use axum::extract::Query;
use rusqlite::OptionalExtension;
use serde::Deserialize;

// Only normalize explicit SxxExx tokens. Absolute anime numbering and URLs are
// intentionally not guessed; absent/conflicting evidence falls back to the picker.
pub fn release_group(filename: &str) -> Option<String> {
    let mut found = false;
    let tokens: Vec<_> = filename
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|token| {
            let lower = token.to_ascii_lowercase();
            if let Some((season, episode)) = lower.strip_prefix('s').and_then(|s| s.split_once('e'))
            {
                if !season.is_empty()
                    && !episode.is_empty()
                    && season.bytes().all(|b| b.is_ascii_digit())
                    && episode.bytes().all(|b| b.is_ascii_digit())
                {
                    found = true;
                    return "episode".to_owned();
                }
            }
            lower
        })
        .collect();
    found.then(|| format!("{:x}", Sha256::digest(tokens.join(" ").as_bytes())))
}

pub fn title_id(item: &Value) -> String {
    let id = item["id"].as_str().unwrap_or("");
    if item["type"] == "series" {
        item["series_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| legacy_series_id(id))
            .to_owned()
    } else {
        id.to_owned()
    }
}
pub fn init(db: &Connection) -> rusqlite::Result<()> {
    let exists = db
        .prepare("PRAGMA table_info(progress)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|s| s == "title_id");
    if !exists {
        db.execute_batch("ALTER TABLE progress ADD COLUMN title_id TEXT NOT NULL DEFAULT '';")?;
        let rows = db
            .prepare("SELECT rowid,id,type,context FROM progress")?
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (row, id, kind, context) in rows {
            let mut item: Value = serde_json::from_str(&context).unwrap_or(json!({}));
            item["id"] = json!(id);
            item["type"] = json!(kind);
            db.execute(
                "UPDATE progress SET title_id=?1 WHERE rowid=?2",
                params![title_id(&item), row],
            )?;
        }
    }
    db.execute_batch("CREATE INDEX IF NOT EXISTS progress_titles ON progress(profile_id,type,title_id,updated_at DESC);
        CREATE TABLE IF NOT EXISTS queue_hidden(profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,type TEXT NOT NULL,title_id TEXT NOT NULL,PRIMARY KEY(profile_id,type,title_id));
        CREATE TABLE IF NOT EXISTS continuation_cache(profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,series_id TEXT NOT NULL,from_id TEXT NOT NULL,result TEXT NOT NULL,updated_at INTEGER NOT NULL,PRIMARY KEY(profile_id,series_id,from_id));
        CREATE TABLE IF NOT EXISTS viewing_settings(profile_id INTEGER PRIMARY KEY REFERENCES profiles(id) ON DELETE CASCADE,autoplay INTEGER NOT NULL DEFAULT 1);")?;
    // When a title was removed from the queue (seconds). Kept beside queue_hidden
    // so its positional shape stays stable; titles hidden before this table
    // existed count as hidden now. A release after this time brings the title back.
    db.execute_batch("CREATE TABLE IF NOT EXISTS queue_hidden_times(profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,type TEXT NOT NULL,title_id TEXT NOT NULL,hidden_at INTEGER NOT NULL,PRIMARY KEY(profile_id,type,title_id));
        INSERT OR IGNORE INTO queue_hidden_times(profile_id,type,title_id,hidden_at) SELECT profile_id,type,title_id,strftime('%s','now') FROM queue_hidden;
        CREATE TRIGGER IF NOT EXISTS queue_hidden_times_insert AFTER INSERT ON queue_hidden BEGIN INSERT OR REPLACE INTO queue_hidden_times(profile_id,type,title_id,hidden_at) VALUES(NEW.profile_id,NEW.type,NEW.title_id,strftime('%s','now')); END;
        CREATE TRIGGER IF NOT EXISTS queue_hidden_times_delete AFTER DELETE ON queue_hidden BEGIN DELETE FROM queue_hidden_times WHERE profile_id=OLD.profile_id AND type=OLD.type AND title_id=OLD.title_id; END;
        CREATE TABLE IF NOT EXISTS continuation_releases(profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,series_id TEXT NOT NULL,from_id TEXT NOT NULL,checked_at INTEGER NOT NULL,document TEXT,next_check_at INTEGER NOT NULL DEFAULT 0,failures INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(profile_id,series_id));")
}

/// Hide a queue title now. Re-hiding a title a release brought back restarts its clock.
pub(crate) fn hide(db: &Connection, profile: i64, kind: &str, title: &str) -> rusqlite::Result<()> {
    db.execute(
        "INSERT OR IGNORE INTO queue_hidden(profile_id,type,title_id) VALUES(?1,?2,?3)",
        params![profile, kind, title],
    )?;
    db.execute("INSERT INTO queue_hidden_times(profile_id,type,title_id,hidden_at) VALUES(?1,?2,?3,?4) ON CONFLICT(profile_id,type,title_id) DO UPDATE SET hidden_at=excluded.hidden_at",params![profile,kind,title,util::now()])?;
    Ok(())
}

/// Resolve only an unambiguous actual metadata entry. Never synthesize episode IDs.
pub fn resolve(current: &Value, meta: &Value, now: i64) -> Value {
    let unavailable = || json!({"status":"unavailable"});
    if current["type"] != "series" {
        return unavailable();
    }
    let Some(videos) = meta["videos"].as_array().filter(|v| v.len() <= 2000) else {
        return unavailable();
    };
    let matches: Vec<_> = videos.iter().filter(|v| v["id"] == current["id"]).collect();
    if matches.len() != 1 {
        return unavailable();
    }
    let here = matches[0];
    let coordinate =
        |v: &Value| -> Option<(u64, u64)> { Some((v["season"].as_u64()?, v["episode"].as_u64()?)) };
    let Some(at) = coordinate(here) else {
        return unavailable();
    };
    if current["season"].as_u64().is_some_and(|n| n != at.0)
        || current["episode"].as_u64().is_some_and(|n| n != at.1)
    {
        return unavailable();
    }
    let mut later: Vec<_> = videos
        .iter()
        .filter_map(|v| coordinate(v).map(|n| (n, v)))
        .filter(|(n, _)| *n > at && ((at.0 == 0 && n.0 == 0) || (at.0 > 0 && n.0 > 0)))
        .collect();
    later.sort_by_key(|(n, _)| *n);
    let Some((next_at, next)) = later.first() else {
        return json!({"status":"caught_up"});
    };
    if later.iter().filter(|(n, _)| n == next_at).count() != 1 {
        return unavailable();
    }
    let Some(id) = next["id"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 512)
    else {
        return unavailable();
    };
    if videos.iter().filter(|v| v["id"] == id).count() != 1 || current["id"] == id {
        return unavailable();
    }
    if let Some(released) = next["released"].as_str().filter(|s| !s.is_empty()) {
        let date = chrono::DateTime::parse_from_rfc3339(released)
            .map(|d| d.timestamp())
            .ok()
            .or_else(|| {
                chrono::NaiveDate::parse_from_str(released, "%Y-%m-%d")
                    .ok()?
                    .and_hms_opt(0, 0, 0)
                    .map(|d| d.and_utc().timestamp())
            });
        match date {
            Some(date) if date <= now => (),
            Some(_) => return json!({"status":"upcoming"}),
            None => return unavailable(),
        }
    }
    let mut item = matching_context(current).unwrap_or(json!({}));
    item["id"] = json!(id);
    item["type"] = json!("series");
    item["series_id"] = json!(title_id(current));
    item["season"] = json!(next_at.0);
    item["episode"] = json!(next_at.1);
    item["name"] = current["name"].clone();
    item["seriesName"] = current["name"].clone();
    item["episodeTitle"] = next["title"].clone();
    item["poster"] = current["poster"].clone();
    item["position"] = json!(0);
    item["duration"] = json!(0);
    item["queue_status"] = json!("next");
    json!({"status":"next","item":item})
}

pub async fn next(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    axum::Json(current): axum::Json<Value>,
) -> ApiResult {
    let app = app.with_lease(lease);
    text(&current, "id", 512)?;
    media_type(&current)?;
    matching_context(&current)?;
    {
        let db = app.db.lock().unwrap();
        app.require_profile(&db, profile)?;
    }
    let series = title_id(&current);
    let result =
        match tokio::time::timeout(Duration::from_secs(12), app.addons.meta("series", &series))
            .await
        {
            Ok(Ok(meta)) => resolve(&current, &meta["meta"], util::now()),
            _ => json!({"status":"unavailable"}),
        };
    let db = app.db.lock().unwrap();
    app.require_profile(&db, profile)?;
    db.execute("INSERT INTO continuation_cache(profile_id,series_id,from_id,result,updated_at) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(profile_id,series_id,from_id) DO UPDATE SET result=excluded.result,updated_at=excluded.updated_at",params![profile,series,current["id"].as_str(),result.to_string(),util::now()]).map_err(db_error)?;
    // Cache is bounded to the latest 200 episode transitions per profile.
    db.execute("DELETE FROM continuation_cache WHERE profile_id=?1 AND rowid NOT IN (SELECT rowid FROM continuation_cache WHERE profile_id=?1 ORDER BY updated_at DESC,rowid DESC LIMIT 200)",[profile]).map_err(db_error)?;
    Ok(axum::Json(result))
}

#[derive(Default, Deserialize)]
pub struct Page {
    offset: Option<u32>,
    limit: Option<u32>,
}
pub fn queue(db: &Connection, profile: i64, offset: u32, limit: u32) -> Result<Value, ApiError> {
    queue_items(db, profile, offset, limit, false)
}
/// The queue, optionally with hidden titles marked by `queue_hidden_at` (seconds).
pub(crate) fn queue_items(
    db: &Connection,
    profile: i64,
    offset: u32,
    limit: u32,
    include_hidden: bool,
) -> Result<Value, ApiError> {
    let allowed = kids::sql_allowed("progress.type", "progress.id", "?1");
    let sql = format!("WITH ranked AS (SELECT *,rowid AS activity_id,ROW_NUMBER() OVER(PARTITION BY type,CASE WHEN title_id='' THEN id ELSE title_id END ORDER BY updated_at DESC,rowid DESC) AS rank FROM progress WHERE profile_id=?1 AND type!='live' AND json_extract(context,'$.stremio_completion_only') IS NOT 1 AND {allowed}), visible AS (SELECT * FROM ranked p WHERE rank=1 AND (position>0 OR json_extract(context,'$.progress_corrected')=1 OR (json_extract(context,'$.simkl_import.position_basis')='recorded_completion' AND EXISTS(SELECT 1 FROM continuation_cache c WHERE c.profile_id=p.profile_id AND c.series_id=p.title_id AND c.from_id=p.id AND json_extract(c.result,'$.status')='next' AND c.updated_at>strftime('%s','now')-3600))) AND (json_extract(context,'$.simkl_import.position_basis') IS NOT 'recorded_completion' OR EXISTS(SELECT 1 FROM continuation_cache c WHERE c.profile_id=p.profile_id AND c.series_id=p.title_id AND c.from_id=p.id AND json_extract(c.result,'$.status')='next' AND c.updated_at>strftime('%s','now')-3600)) AND (type='series' OR (json_extract(context,'$.watched_override') IS NOT 1 AND (duration<=0 OR position/duration<0.95))) AND (?4 OR NOT EXISTS(SELECT 1 FROM queue_hidden h WHERE h.profile_id=p.profile_id AND h.type=p.type AND h.title_id=CASE WHEN p.title_id='' THEN p.id ELSE p.title_id END))) SELECT id,type,name,poster,position,duration,updated_at,context,title_id,COUNT(*) OVER(),(SELECT COALESCE(t.hidden_at,0) FROM queue_hidden h LEFT JOIN queue_hidden_times t ON t.profile_id=h.profile_id AND t.type=h.type AND t.title_id=h.title_id WHERE h.profile_id=visible.profile_id AND h.type=visible.type AND h.title_id=CASE WHEN visible.title_id='' THEN visible.id ELSE visible.title_id END) FROM visible ORDER BY updated_at DESC,activity_id DESC LIMIT ?2 OFFSET ?3");
    let mut query = db.prepare(&sql).map_err(db_error)?;
    let rows = query.query_map(params![profile,limit+1,offset,include_hidden], |r| {
        let mut item = json!({"id":r.get::<_,String>(0)?,"type":r.get::<_,String>(1)?,"name":r.get::<_,String>(2)?,"poster":r.get::<_,Option<String>>(3)?,"position":r.get::<_,f64>(4)?,"duration":r.get::<_,f64>(5)?,"updated_at":r.get::<_,i64>(6)?});
        if let Some(hidden) = r.get::<_,Option<i64>>(10)? { item["queue_hidden_at"] = json!(hidden); }
        let mut manual_watched = false;
        if let Ok(raw) = serde_json::from_str::<Value>(&r.get::<_,String>(7)?) { manual_watched = raw["watched_override"] == true; library::add_watch_fields(&mut item,&raw); engine_v3::history::restore(&mut item,&raw); if let Ok(context) = matching_context(&raw) { item.as_object_mut().unwrap().extend(context.as_object().unwrap().clone()); } }
        Ok((item,r.get::<_,String>(8)?,r.get::<_,u32>(9)?,manual_watched))
    }).map_err(db_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
    let total = rows.first().map(|r| r.2).unwrap_or(0);
    let has_more = rows.len() > limit as usize;
    let mut items = Vec::new();
    for (mut item, series, _, mut manual_watched) in rows.into_iter().take(limit as usize) {
        let hidden_at = item["queue_hidden_at"].clone();
        item["queue_title_id"] = json!(if series.is_empty() {
            title_id(&item)
        } else {
            series.clone()
        });
        if let Some(anchor) = stremio_import::continuation_item(db, profile, &item)? {
            item = anchor;
            manual_watched = true;
        } else if let Some(anchor) = furthest_watched(db, profile, &series, &item)? {
            item = anchor;
            manual_watched = true;
        }
        let position = item["position"].as_f64().unwrap_or(0.0);
        let duration = item["duration"].as_f64().unwrap_or(0.0);
        let near_end = duration > 10.0 && position >= duration - 10.0;
        // Finished by the same rule as History and the Watched badge (95 %),
        // so stopping in the credits still moves on to the next episode.
        let finished = near_end || manual_watched || item["watched"] == true;
        if item["type"] == "series" && finished {
            item["queue_status"] = json!("pending");
            let cached: Option<String> = db.query_row("SELECT result FROM continuation_cache WHERE profile_id=?1 AND series_id=?2 AND from_id=?3 AND updated_at>?4",params![profile,series,item["id"].as_str(),util::now()-3600],|r|r.get(0)).optional().map_err(db_error)?;
            if let Some(raw) = cached {
                if let Ok(result) = serde_json::from_str::<Value>(&raw) {
                    if !cached_origin_current(db, profile, &item, &result)? {
                        if !hidden_at.is_null() {
                            item["queue_hidden_at"] = hidden_at;
                        }
                        items.push(item);
                        continue;
                    }
                    if result["status"] == "next" {
                        let previous = item.clone();
                        item = result["item"].clone();
                        // The most recent chosen source/audio wins over an earlier metadata prefetch.
                        for key in CONTEXT_FIELDS {
                            if (key.starts_with("source_") || key == "audio_language")
                                && !previous[key].is_null()
                            {
                                item[key] = previous[key].clone();
                            }
                        }
                        item["queue_title_id"] = previous["queue_title_id"].clone();
                        item["updated_at"] = previous["updated_at"].clone();
                        item["previous_episode"] = previous;
                    } else {
                        item["queue_status"] = result["status"].clone();
                    }
                }
            }
        }
        if !hidden_at.is_null() {
            item["queue_hidden_at"] = hidden_at;
        }
        items.push(item);
    }
    Ok(
        json!({"items":items,"offset":offset,"total":total,"next_offset":if has_more { Some(offset+limit) } else { None }}),
    )
}
/// An episode only opened (under 1 % played) is not where the profile stands
/// when a later episode of the series is already watched: imported Stremio
/// history can name an opened episode as the last one played while a later
/// one is marked watched. The queue then stands at the furthest watched
/// episode in the same regular/special sequence, finished. Real progress,
/// a rewatch included, still resumes.
fn furthest_watched(
    db: &Connection,
    profile: i64,
    series: &str,
    current: &Value,
) -> Result<Option<Value>, ApiError> {
    let position = current["position"].as_f64().unwrap_or(0.0);
    let duration = current["duration"].as_f64().unwrap_or(0.0);
    if series.is_empty() || current["type"] != "series" || !(duration > 0.0 && position < duration * 0.01) {
        return Ok(None);
    }
    let raw: Option<String> = db
        .query_row(
            "SELECT context FROM progress WHERE profile_id=?1 AND type='series' AND id=?2",
            params![profile, current["id"].as_str()],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?;
    let Some(own) = raw.and_then(|raw| serde_json::from_str::<Value>(&raw).ok()) else {
        return Ok(None);
    };
    // A manual correction ("Mark unwatched" included) is the profile's decision.
    if own["progress_corrected"] == true || library::watched(current, &own) {
        return Ok(None);
    }
    let coordinate = |v: &Value| Some((v["season"].as_u64()?, v["episode"].as_u64()?));
    let Some(from) = coordinate(&own) else {
        return Ok(None);
    };
    let mut query = db
        .prepare("SELECT id,name,poster,position,duration,context FROM progress WHERE profile_id=?1 AND type='series' AND title_id=?2 LIMIT 10001")
        .map_err(db_error)?;
    let rows = query
        .query_map(params![profile, series], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, f64>(4)?,
                r.get::<_, String>(5)?,
            ))
        })
        .map_err(db_error)?;
    let mut furthest: Option<((u64, u64), Value)> = None;
    for row in rows {
        let (id, name, poster, position, duration, raw) = row.map_err(db_error)?;
        let Ok(context) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let Some(at) = coordinate(&context) else {
            continue;
        };
        if (at.0 == 0) != (from.0 == 0)
            || at <= from
            || furthest.as_ref().is_some_and(|(best, _)| *best >= at)
        {
            continue;
        }
        let mut item = json!({"id":id,"type":"series","name":name,"poster":poster,
            "position":position,"duration":duration,"updated_at":current["updated_at"],
            "queue_title_id":current["queue_title_id"]});
        if !library::watched(&item, &context) {
            continue;
        }
        let Ok(fields) = matching_context(&context) else {
            continue;
        };
        library::add_watch_fields(&mut item, &context);
        item.as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        furthest = Some((at, item));
    }
    Ok(furthest.map(|(_, item)| item))
}
// New typed results carry provenance. Retain legacy cache compatibility while
// refusing another add-on's identically named opaque episode or stale settings.
fn cached_origin_current(
    db: &Connection,
    profile: i64,
    item: &Value,
    result: &Value,
) -> Result<bool, ApiError> {
    let Some(origin) = result["metadata_origin"].as_str() else {
        return Ok(true);
    };
    if item["engine_provider_id"].as_str() != Some(origin) {
        return Ok(false);
    }
    let current: u64 = db
        .query_row(
            "SELECT revision FROM engine_profile_configuration WHERE profile_id=?1",
            [profile],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_error)?
        .unwrap_or(0);
    if result["metadata_configuration_revision"].as_u64() != Some(current) {
        return Ok(false);
    }
    let Some(account) = result["metadata_account_id"].as_i64() else {
        return Ok(false);
    };
    let owns: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM profile_owners WHERE profile_id=?1 AND account_id=?2)",
            params![profile, account],
            |row| row.get(0),
        )
        .map_err(db_error)?;
    if !owns {
        return Ok(false);
    }
    let revision: String = db
        .query_row(
            "SELECT revision FROM addon_config_revisions WHERE account_id=?1",
            [account],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_error)?
        .unwrap_or_else(|| "0".repeat(32));
    Ok(result["metadata_addon_revision"].as_str() == Some(revision.as_str()))
}

pub async fn page(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    Query(page): Query<Page>,
) -> ApiResult {
    let app = app.with_lease(lease);
    blocking(move || {
        let db = app.db.lock().unwrap();
        app.require_profile(&db, profile)?;
        Ok(axum::Json(queue(
            &db,
            profile,
            page.offset.unwrap_or(0),
            page.limit.unwrap_or(40).clamp(1, 100),
        )?))
    })
    .await
}
pub async fn visibility(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    axum::Json(item): axum::Json<Value>,
) -> ApiResult {
    let app = app.with_lease(lease);
    blocking(move || {
        text(&item, "id", 512)?;
        let kind = media_type(&item)?;
        matching_context(&item)?;
        let hidden = item["hidden"].as_bool().ok_or("Invalid visibility")?;
        let db = app.db.lock().unwrap();
        app.require_profile(&db, profile)?;
        if hidden {
            hide(&db, profile, kind, &title_id(&item)).map(|_| 0)
        } else {
            db.execute(
                "DELETE FROM queue_hidden WHERE profile_id=?1 AND type=?2 AND title_id=?3",
                params![profile, kind, title_id(&item)],
            )
        }
        .map_err(db_error)?;
        Ok(axum::Json(json!({"ok":true})))
    })
    .await
}
pub async fn settings(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
) -> ApiResult {
    let app = app.with_lease(lease);
    let db = app.db.lock().unwrap();
    app.require_profile(&db, profile)?;
    let autoplay: Option<bool> = db
        .query_row(
            "SELECT autoplay FROM viewing_settings WHERE profile_id=?1",
            [profile],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?;
    Ok(axum::Json(json!({"autoplay":autoplay.unwrap_or(true)})))
}
pub async fn save_settings(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    axum::Json(value): axum::Json<Value>,
) -> ApiResult {
    let app = app.with_lease(lease);
    let enabled = value["autoplay"]
        .as_bool()
        .ok_or("Invalid autoplay setting")?;
    let db = app.db.lock().unwrap();
    app.require_profile(&db, profile)?;
    db.execute("INSERT INTO viewing_settings(profile_id,autoplay) VALUES(?1,?2) ON CONFLICT(profile_id) DO UPDATE SET autoplay=excluded.autoplay",params![profile,enabled]).map_err(db_error)?;
    Ok(axum::Json(json!({"autoplay":enabled})))
}
