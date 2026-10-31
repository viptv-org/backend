use super::*;
use viptv_engine::{identity::MediaIdentity, playback::PlaybackRecord, presentation::Card};

const POSTER: &str = "https://images.metahub.space/poster/small";
const BACKGROUND: &str = "https://images.metahub.space/background/medium";
pub(crate) const READ_BATCH: usize = 200;
const BOOT_ROWS: usize = 200;
const BOOT_TOTAL: usize = 5_000;
const BOOT_SPACING: Duration = Duration::from_millis(50);

fn imdb(id: &str) -> Option<&str> {
    id.strip_prefix("tt")
        .filter(|digits| {
            (5..=12).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit())
        })
        .map(|_| id)
}
pub(crate) fn title(id: &str) -> Option<&str> {
    imdb(id.split(':').next().unwrap_or(id))
}
pub(crate) fn poster_url(id: &str) -> Option<String> {
    imdb(id).map(|id| format!("{POSTER}/{id}/img"))
}
fn background_url(id: &str) -> Option<String> {
    imdb(id).map(|id| format!("{BACKGROUND}/{id}/img"))
}

fn identity_imdb(identity: &MediaIdentity) -> Option<&str> {
    imdb(&identity.id).or_else(|| {
        identity
            .ids
            .iter()
            .filter(|id| id.namespace == "imdb")
            .find_map(|id| imdb(&id.value))
    })
}
fn title_identity<'a>(
    identity: &'a MediaIdentity,
    parent: Option<&'a MediaIdentity>,
) -> Option<&'a str> {
    let parent = parent.and_then(identity_imdb);
    if identity.episode.is_some() {
        parent.or_else(|| identity_imdb(identity))
    } else {
        identity_imdb(identity).or(parent)
    }
}

fn blank(value: &Option<String>) -> bool {
    value.as_deref().is_none_or(str::is_empty)
}

fn fill_art(
    identity: &MediaIdentity,
    parent: Option<&MediaIdentity>,
    poster: &mut Option<String>,
    landscape: &mut Option<String>,
) -> bool {
    let Some(title) = title_identity(identity, parent) else {
        return false;
    };
    let mut changed = false;
    if blank(poster) {
        *poster = poster_url(title);
        changed |= poster.is_some();
    }
    if blank(landscape) {
        *landscape = background_url(title);
        changed |= landscape.is_some();
    }
    changed
}

pub(crate) fn fill(card: &mut Card) -> bool {
    fill_art(
        &card.identity,
        card.parent_identity.as_ref(),
        &mut card.poster,
        &mut card.landscape,
    )
}

pub(crate) fn identity_poster(identity: &MediaIdentity) -> Option<String> {
    title_identity(identity, None).and_then(poster_url)
}

pub(crate) fn fill_record(record: &mut PlaybackRecord) -> bool {
    fill_art(
        &record.identity,
        record.parent_identity.as_ref(),
        &mut record.poster,
        &mut record.landscape,
    )
}

pub(crate) fn fill_progress(title: &str, poster: &mut Option<String>, context: &mut Value) {
    if blank(poster) {
        *poster = poster_url(title);
    }
    if context.is_object() && context["background"].as_str().is_none_or(str::is_empty) {
        if let Some(url) = background_url(title) {
            context["background"] = json!(url);
        }
    }
}

fn identity_value(value: &Value) -> Option<String> {
    serde_json::from_value::<MediaIdentity>(value.clone())
        .ok()
        .and_then(|identity| identity_imdb(&identity).map(str::to_owned))
}
fn saved_title(id: &str, raw: &str) -> Option<String> {
    serde_json::from_str::<Card>(raw)
        .ok()
        .and_then(|card| {
            title_identity(&card.identity, card.parent_identity.as_ref()).map(str::to_owned)
        })
        .or_else(|| imdb(id).map(str::to_owned))
}
fn progress_title(id: &str, title_id: &str, context: &Value) -> Option<String> {
    imdb(title_id)
        .map(str::to_owned)
        .or_else(|| title(id).map(str::to_owned))
        .or_else(|| identity_value(&context["engine_parent_identity"]))
        .or_else(|| identity_value(&context["engine_identity"]))
}

pub(crate) fn backfill(db: &Connection, profile: i64, limit: usize) -> Result<usize, ApiError> {
    if limit == 0 {
        return Ok(0);
    }
    let filled = saved_titles(db, profile, limit)?;
    if filled < limit {
        return Ok(filled + progress_rows(db, profile, limit - filled)?);
    }
    Ok(filled)
}

fn saved_titles(db: &Connection, profile: i64, limit: usize) -> Result<usize, ApiError> {
    let rows = db
        .prepare(
            "SELECT rowid,id,poster,context FROM favorites WHERE profile_id=?1 \
             AND (poster IS NULL OR poster='' \
                  OR (context LIKE '{%' AND context LIKE '%\"landscape\":null%')) \
             AND (id LIKE 'tt%' OR context LIKE '%\"namespace\":\"imdb\"%') \
             ORDER BY rowid LIMIT ?2",
        )
        .map_err(db_error)?
        .query_map(params![profile, limit as i64], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    let mut update = db
        .prepare(
            "UPDATE favorites SET poster=CASE WHEN poster IS NULL OR poster='' THEN ?2 ELSE poster END,context=?3 \
             WHERE rowid=?1 AND context=?4",
        )
        .map_err(db_error)?;
    let mut filled = 0;
    for (row, id, stored, raw) in rows {
        let Some(title) = saved_title(&id, &raw) else {
            continue;
        };
        let stored = stored.filter(|poster| !poster.is_empty());
        let poster = stored.clone().or_else(|| poster_url(&title));
        let mut context = raw.clone();
        let mut changed = false;
        if let Ok(mut card) = serde_json::from_str::<Card>(&raw) {
            if fill(&mut card) {
                if let Ok(text) = serde_json::to_string(&card) {
                    context = text;
                    changed = true;
                }
            }
        }
        if !changed && (stored.is_some() || poster.is_none()) {
            continue;
        }
        let updated = update
            .execute(params![row, poster, context, raw])
            .map_err(db_error)?;
        filled += usize::from(updated == 1);
    }
    Ok(filled)
}

fn progress_rows(db: &Connection, profile: i64, limit: usize) -> Result<usize, ApiError> {
    let rows = db
        .prepare(
            "SELECT rowid,id,context,title_id FROM progress \
             WHERE profile_id=?1 AND type IN ('movie','series') AND (poster IS NULL OR poster='') \
             AND (COALESCE(NULLIF(title_id,''),id) LIKE 'tt%' OR context LIKE '%\"imdb\"%') \
             ORDER BY rowid LIMIT ?2",
        )
        .map_err(db_error)?
        .query_map(params![profile, limit as i64], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    let mut update = db
        .prepare("UPDATE progress SET poster=?2 WHERE rowid=?1 AND (poster IS NULL OR poster='')")
        .map_err(db_error)?;
    let mut filled = 0;
    for (row, id, raw, title_id) in rows {
        let context: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
        let Some(title) = progress_title(&id, &title_id, &context) else {
            continue;
        };
        let Some(poster) = poster_url(&title) else {
            continue;
        };
        let updated = update.execute(params![row, poster]).map_err(db_error)?;
        filled += usize::from(updated == 1);
    }
    Ok(filled)
}

pub(crate) fn start(app: App) {
    tokio::spawn(async move {
        if let Err(error) = backfill_all(&app).await {
            tracing::warn!(message = %error.1, "Card art backfill stopped");
        }
    });
}

pub(crate) async fn backfill_all(app: &App) -> Result<(), ApiError> {
    for profile in profiles(app)? {
        let mut total = 0;
        loop {
            let limit = BOOT_ROWS.min(BOOT_TOTAL.saturating_sub(total));
            if limit == 0 {
                break;
            }
            let filled = {
                let db = app
                    .db
                    .lock()
                    .map_err(|_| ApiError::from("Database unavailable"))?;
                backfill(&db, profile, limit)?
            };
            total += filled;
            if filled < limit {
                break;
            }
            tokio::time::sleep(BOOT_SPACING).await;
        }
    }
    Ok(())
}

fn profiles(app: &App) -> Result<Vec<i64>, ApiError> {
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    let rows = db
        .prepare("SELECT id FROM profiles ORDER BY id")
        .map_err(db_error)?
        .query_map([], |r| r.get::<_, i64>(0))
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    Ok(rows)
}