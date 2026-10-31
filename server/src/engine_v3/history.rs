use super::*;
use viptv_engine::{
    identity::MediaIdentity,
    playback::{HistoryEntry, PlaybackRecord},
};

const PRESENTATION: [&str; 4] = [
    "engine_identity",
    "engine_parent_identity",
    "engine_provider_id",
    "background",
];

pub(crate) fn context(value: &Value) -> Result<Value, ApiError> {
    let mut context = matching_context(value)?;
    for key in PRESENTATION {
        let field = &value[key];
        if field.is_null() {
            continue;
        }
        if key.ends_with("identity") {
            let identity: MediaIdentity = serde_json::from_value(field.clone())
                .map_err(|_| ApiError::from("Invalid history identity"))?;
            if identity.id.is_empty()
                || identity.id.len() > 512
                || identity.id.chars().any(char::is_control)
                || identity.ids.len() > 32
                || identity.ids.iter().any(|id| {
                    id.namespace.len() > 64
                        || id.value.len() > 512
                        || id
                            .namespace
                            .chars()
                            .chain(id.value.chars())
                            .any(char::is_control)
                })
                || identity
                    .edition
                    .as_ref()
                    .is_some_and(|v| v.len() > 512 || v.chars().any(char::is_control))
            {
                return Err("Invalid history identity".into());
            }
        } else {
            let text = field.as_str().ok_or("Invalid history presentation")?;
            let limit = if key == "background" { 4096 } else { 128 };
            if text.len() > limit || text.chars().any(char::is_control) {
                return Err("Invalid history presentation".into());
            }
        }
        context[key] = field.clone();
    }
    Ok(context)
}

pub(crate) fn restore(item: &mut Value, context: &Value) {
    for key in PRESENTATION {
        if !context[key].is_null() {
            item[key] = context[key].clone();
        }
    }
}

pub(super) fn record(item: &Value) -> Option<PlaybackRecord> {
    let projected = super::projection::card(item, "library")?;
    let identity = serde_json::from_value::<MediaIdentity>(item["engine_identity"].clone())
        .ok()
        .filter(|identity| {
            identity.id == projected.identity.id && identity.kind == projected.identity.kind
        })
        .unwrap_or(projected.identity);
    let millis = |key: &str| {
        item[key]
            .as_f64()
            .filter(|v| v.is_finite() && *v >= 0.0)
            .map(|v| (v * 1000.0).round() as u64)
            .unwrap_or(0)
    };
    let updated = item["updated_at"]
        .as_u64()
        .unwrap_or(0)
        .saturating_mul(1000);
    let parent_identity =
        serde_json::from_value::<MediaIdentity>(item["engine_parent_identity"].clone())
            .ok()
            .filter(|parent| {
                identity.episode.is_some()
                    && parent.kind == "series"
                    && parent.episode.is_none()
                    && Some(parent.id.as_str()) == item["series_id"].as_str()
            })
            .or_else(|| {
                item["series_id"]
                    .as_str()
                    .filter(|_| identity.episode.is_some())
                    .map(|id| MediaIdentity {
                        id: id.into(),
                        kind: "series".into(),
                        ids: vec![],
                        episode: None,
                        edition: None,
                    })
            });
    Some(PlaybackRecord {
        metadata_origin: None,
        times_watched: None,
        presentation: None,
        identity,
        parent_identity,
        provider_id: item["engine_provider_id"]
            .as_str()
            .unwrap_or("library")
            .into(),
        title: projected.title,
        poster: projected.poster,
        landscape: projected.landscape,
        position_millis: millis("position"),
        duration_millis: millis("duration"),
        updated_at_millis: updated,
        completed_at_millis: (item["watched"] == true
            && item["watch_date_known"] != false
            && updated > 0)
            .then_some(updated),
    })
}
#[derive(Deserialize)]
pub(crate) struct Page {
    #[serde(default)]
    offset: u32,
    #[serde(default = "history_limit")]
    limit: u32,
    #[serde(default, rename = "exclude_live")]
    _exclude_live: Option<bool>,
}
fn history_limit() -> u32 {
    20
}

struct Row {
    record: PlaybackRecord,
    watch: Option<Watch>,
}

#[derive(Clone, Copy)]
struct Watch {
    position: f64,
    duration: f64,
    watched: bool,
    resume_active: bool,
    completion_only: bool,
    watch_date_known: bool,
}

impl Watch {
    fn legacy(item: &Value) -> Self {
        Self {
            position: item["position"].as_f64().unwrap_or(0.0),
            duration: item["duration"].as_f64().unwrap_or(0.0),
            watched: item["watched"] == true,
            resume_active: item["resume_active"] == true,
            completion_only: item["completion_only"] == true,
            watch_date_known: item["watch_date_known"] == true,
        }
    }
    fn engine(record: &PlaybackRecord) -> Self {
        Self {
            position: record.position_millis as f64 / 1000.0,
            duration: record.duration_millis as f64 / 1000.0,
            watched: record.completed_at_millis.is_some(),
            resume_active: record.can_resume(),
            completion_only: false,
            watch_date_known: true,
        }
    }
    fn insert(self, item: &mut Value) {
        item["position"] = json!(self.position);
        item["duration"] = json!(self.duration);
        item["watched"] = json!(self.watched);
        item["resume_active"] = json!(self.resume_active);
        item["completion_only"] = json!(self.completion_only);
        item["watch_date_known"] = json!(self.watch_date_known);
    }
}

fn keep_newest(
    rows: &mut std::collections::BTreeMap<scoped_library::Key, Row>,
    mut row: Row,
) -> Result<(), ApiError> {
    crate::simkl::key::identity(&mut row.record.identity);
    if let Some(parent) = row.record.parent_identity.as_mut() {
        crate::simkl::key::identity(parent);
    }
    let key = scoped_library::raw_key(&row.record.card())?;
    if rows
        .get(&key)
        .is_none_or(|old| old.record.updated_at_millis < row.record.updated_at_millis)
    {
        rows.insert(key, row);
    }
    Ok(())
}

pub(crate) async fn list(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    Query(page): Query<Page>,
) -> ApiResult {
    if page.offset > 10000 || !(1..=100).contains(&page.limit) {
        return Err("Invalid history page".into());
    }
    let offset = page.offset as usize;
    let limit = page.limit as usize;
    let app = app.with_lease(lease);
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_profile(&db, profile)?;
    if !kids::restricted(&db, &app.identity())? {
        super::art::backfill(&db, profile, super::art::READ_BATCH)?;
    }
    let mut q=db.prepare("SELECT id,type,name,poster,position,duration,updated_at,context,title_id FROM progress WHERE profile_id=?1 AND type IN ('movie','series') ORDER BY updated_at DESC LIMIT 50001").map_err(db_error)?;
    let items=q.query_map([profile],|r|{let context=serde_json::from_str::<Value>(&r.get::<_,String>(7)?).unwrap_or(Value::Null);let mut item=json!({"id":r.get::<_,String>(0)?,"type":r.get::<_,String>(1)?,"name":r.get::<_,String>(2)?,"poster":r.get::<_,Option<String>>(3)?,"position":r.get::<_,f64>(4)?,"duration":r.get::<_,f64>(5)?,"updated_at":r.get::<_,u64>(6)?,"series_id":r.get::<_,Option<String>>(8)?});restore(&mut item,&context);crate::library::add_watch_fields(&mut item,&context);if let Ok(context)=matching_context(&context){item.as_object_mut().unwrap().extend(context.as_object().unwrap().clone());}Ok(item)}).map_err(db_error)?.collect::<Result<Vec<_>,_>>().map_err(db_error)?;
    if items.len() > 50000 {
        return Err("History exceeds its bounded item limit".into());
    }
    let mut rows = std::collections::BTreeMap::new();
    for item in kids::filter_items(&db, &app.identity(), items)? {
        if let Some(record) = record(&item) {
            keep_newest(
                &mut rows,
                Row {
                    record,
                    watch: Some(Watch::legacy(&item)),
                },
            )?;
        }
    }
    for saved in scoped_library::playback_db(&db, &app, profile)? {
        keep_newest(
            &mut rows,
            Row {
                record: saved.value,
                watch: None,
            },
        )?;
    }
    let mut rows = rows.into_values().collect::<Vec<_>>();
    rows.sort_by(|a, b| {
        b.record
            .updated_at_millis
            .cmp(&a.record.updated_at_millis)
            .then_with(|| a.record.identity.id.cmp(&b.record.identity.id))
    });
    for row in &mut rows {
        super::art::fill_record(&mut row.record);
    }
    let mut entries = Vec::new();
    for row in rows.iter().skip(offset).take(limit) {
        let mut entry = serde_json::to_value(HistoryEntry::from(&row.record))
            .map_err(|_| ApiError::from("History unavailable"))?;
        row.watch
            .unwrap_or_else(|| Watch::engine(&row.record))
            .insert(&mut entry);
        entries.push(entry);
    }

    Ok(axum::Json(Value::Array(entries)))
}

pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS engine_library_revisions(profile_id INTEGER PRIMARY KEY REFERENCES profiles(id) ON DELETE CASCADE,revision INTEGER NOT NULL);")?;
    for table in ["progress", "favorites", "queue_hidden", "queue_hidden_times"] {
        for (operation, row) in [("INSERT", "NEW"), ("UPDATE", "NEW"), ("DELETE", "OLD")] {
            db.execute_batch(&format!("CREATE TRIGGER IF NOT EXISTS engine_library_{table}_{operation} AFTER {operation} ON {table} BEGIN
                INSERT INTO engine_library_revisions(profile_id,revision) SELECT {row}.profile_id,1 WHERE EXISTS(SELECT 1 FROM profiles WHERE id={row}.profile_id)
                ON CONFLICT(profile_id) DO UPDATE SET revision=revision+1; END;"))?;
        }
    }
    Ok(())
}

pub(super) fn revision(db: &Connection, profile: i64) -> Result<u64, ApiError> {
    db.query_row(
        "SELECT revision FROM engine_library_revisions WHERE profile_id=?1",
        [profile],
        |r| r.get(0),
    )
    .optional()
    .map_err(db_error)
    .map(|r| r.unwrap_or(0))
}

pub(crate) async fn clear(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
) -> ApiResult {
    let app = app.with_lease(lease);
    tokio::task::spawn_blocking(move || {
        let mut db=app.db.lock().map_err(|_|ApiError::from("Database unavailable"))?;
        app.require_profile(&db,profile)?;
        let transaction=db.transaction().map_err(db_error)?;
        app.require_profile(&transaction,profile)?;
        transaction.execute("INSERT INTO simkl_import_history_floors VALUES(?1,?2) ON CONFLICT(profile_id) DO UPDATE SET cutoff_millis=MAX(cutoff_millis,excluded.cutoff_millis)",params![profile,util::now_millis()]).map_err(db_error)?;
        transaction.execute("DELETE FROM progress WHERE profile_id=?1",[profile]).map_err(db_error)?;
        transaction.execute("DELETE FROM continuation_cache WHERE profile_id=?1",[profile]).map_err(db_error)?;
        transaction.execute("DELETE FROM engine_scoped_records WHERE profile_id=?1 AND record_kind='playback'",[profile]).map_err(db_error)?;
        transaction.execute("DELETE FROM engine_scoped_next WHERE profile_id=?1",[profile]).map_err(db_error)?;
        transaction.commit().map_err(db_error)?;
        Ok(axum::Json(json!({"ok":true})))
    }).await.map_err(|_|ApiError::from("History unavailable"))?
}
