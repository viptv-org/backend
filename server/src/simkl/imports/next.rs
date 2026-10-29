use super::*;
use viptv_engine::library::Entry;
fn aired(value: &Value, next_air: Option<u64>) -> bool {
    if value["aired"] == false {
        return false;
    }
    if value["aired"] == true {
        return true;
    }
    let date = progress::millis(&value["date"])
        .or_else(|| {
            value["date"]
                .as_str()
                .and_then(|s| {
                    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                        .ok()?
                        .and_hms_opt(0, 0, 0)
                })
                .and_then(|d| u64::try_from(d.and_utc().timestamp_millis()).ok())
        })
        .or(next_air);
    date.is_some_and(|date| date <= util::now_millis().max(0) as u64)
}
pub(super) async fn prepare(
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    access: &store::Credential,
    entry: &Entry,
) -> Option<()> {
    let next = entry.statistics.next_episode.as_ref()?;
    let category = if entry.card.category.as_deref() == Some("anime") {
        "anime"
    } else {
        "tv"
    };
    let id = entry
        .card
        .identity
        .ids
        .iter()
        .find(|i| i.namespace == "simkl")?
        .value
        .parse::<u64>()
        .ok()?;
    let (local, from_id, context) = {
        let db = app.db.lock().ok()?;
        schema::validate_db(&db, app, profile, access, lease).ok()?;
        let local:String=db.query_row("SELECT local_id FROM simkl_import_entries WHERE profile_id=?1 AND remote_kind='series' AND remote_id=?2",params![profile,entry.card.identity.id],|r|r.get(0)).ok()?;
        let latest:Option<(String,String)>=db.query_row("SELECT id,context FROM progress WHERE profile_id=?1 AND type='series' AND (title_id=?2 OR id=?2) ORDER BY updated_at DESC,rowid DESC LIMIT 1",params![profile,local],|r|Ok((r.get(0)?,r.get(1)?))).optional().ok()?;
        let (from, raw) = latest?;
        let context: Value = serde_json::from_str(&raw).ok()?;
        if context["simkl_import"]["position_basis"] != "recorded_completion"
            || context["simkl_import"]["link_revision"].as_u64() != access.revision
        {
            return Some(());
        }
        (local, from, context)
    };
    let parent = metadata::title(app, profile, access, category, id)
        .await
        .ok()?;
    if local.starts_with("tt")
        && !parent
            .identity
            .ids
            .iter()
            .any(|id| id.namespace == "imdb" && id.value == local)
    {
        return None;
    }
    let document = cache::metadata(app, profile, access, &format!("/{category}/episodes/{id}"))
        .await
        .ok()?;
    let rows = document.as_array()?;
    let candidate = if category == "anime" {
        if next.order != "anidb" {
            return None;
        }
        super::super::anime_episode::select(&document, None, Some(next.episode), None, None)?
    } else {
        let mut candidates = rows.iter().filter(|row| {
            row["episode"].as_u64() == Some(u64::from(next.episode))
                && row["season"].as_u64() == next.season.map(u64::from)
        });
        let candidate = candidates.next()?;
        if candidates.next().is_some() {
            return None;
        }
        candidate
    };
    if !aired(candidate, entry.statistics.next_air_millis) {
        return None;
    }
    let mut cards = viptv_engine::simkl::episodes(&json!([candidate]), &parent).ok()?;
    let mut target = cards.pop()?;
    let coordinates = target.identity.episode.as_ref()?;
    let mut parent = parent;
    parent.identity.id = local.clone();
    target.parent_identity = Some(parent.identity.clone());
    target.identity.id = format!("{local}:{}:{}", coordinates.season?, coordinates.episode);
    let item = json!({"id":target.identity.id,"type":"series","name":parent.title,"poster":parent.poster,"series_id":local,"season":coordinates.season,"episode":coordinates.episode,"position":0,"duration":0,"watched":false,"queue_status":"next","engine_provider_id":"simkl","engine_identity":target.identity,"engine_parent_identity":parent.identity,"engine_presentation":target,"background":parent.landscape});
    let result = json!({"status":"next","item":item}).to_string();
    store::validate(app, profile, access).ok()?;
    let db = app.db.lock().ok()?;
    schema::validate_db(&db, app, profile, access, lease).ok()?;
    kids::require_item(&db, &lease.principal, "series", item["id"].as_str()?).ok()?;
    let current:Option<String>=db.query_row("SELECT context FROM progress WHERE profile_id=?1 AND type='series' AND id=?2 AND id=(SELECT id FROM progress WHERE profile_id=?1 AND type='series' AND title_id=?3 ORDER BY updated_at DESC,rowid DESC LIMIT 1)",params![profile,from_id,local],|r|r.get(0)).optional().ok()?;
    if current
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .as_ref()
        != Some(&context)
    {
        return None;
    }
    let old:Option<String>=db.query_row("SELECT result FROM continuation_cache WHERE profile_id=?1 AND series_id=?2 AND from_id=?3",params![profile,local,from_id],|r|r.get(0)).optional().ok()?;
    db.execute("INSERT INTO continuation_cache(profile_id,series_id,from_id,result,updated_at) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(profile_id,series_id,from_id) DO UPDATE SET result=excluded.result,updated_at=excluded.updated_at",params![profile,local,from_id,result,util::now()]).ok()?;
    if old.as_ref() != Some(&result) {
        db.execute("INSERT INTO engine_library_revisions VALUES(?1,1) ON CONFLICT(profile_id) DO UPDATE SET revision=revision+1",[profile]).ok()?;
    }
    db.execute("DELETE FROM continuation_cache WHERE profile_id=?1 AND rowid NOT IN(SELECT rowid FROM continuation_cache WHERE profile_id=?1 ORDER BY updated_at DESC,rowid DESC LIMIT 200)",[profile]).ok()?;
    Some(())
}
pub(super) async fn batch(
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    access: &store::Credential,
    entries: &[Entry],
) -> usize {
    use futures::StreamExt;
    let mut work = futures::stream::iter(
        entries
            .iter()
            .filter(|e| {
                e.card.identity.kind == "series"
                    && e.statistics.status.as_deref() == Some("watching")
                    && e.statistics.next_episode.is_some()
            })
            .take(100)
            .cloned()
            .collect::<Vec<_>>(),
    )
    .map(|entry| async move { prepare(app, lease, profile, access, &entry).await })
    .buffer_unordered(3);
    let mut unresolved = 0;
    while let Some(result) = work.next().await {
        if result.is_none() {
            unresolved += 1;
        }
    }
    unresolved
}
