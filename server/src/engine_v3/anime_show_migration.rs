use super::anime_show::{AnimeShow, TvdbEpisode};
use super::*;
use std::collections::HashMap;
use viptv_engine::identity::{EpisodeCoordinates, ExternalId, MediaIdentity};

const MAX_ROWS: usize = 20_000;

pub(crate) fn start(app: App) {
    tokio::spawn(async move {
        if let Err(error) = run(&app).await {
            tracing::warn!(message = %error.1, "Anime show migration stopped");
        }
    });
}

pub(crate) async fn run(app: &App) -> Result<(), ApiError> {
    for (profile, account) in owners(app)? {
        let mut scoped = app.clone();
        scoped.addons = app.addons.clone().for_account(account);
        migrate(&scoped, profile).await?;
    }
    Ok(())
}

fn owners(app: &App) -> Result<Vec<(i64, i64)>, ApiError> {
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    let mut query = db
        .prepare("SELECT p.id,po.account_id FROM profiles p JOIN profile_owners po ON po.profile_id=p.id ORDER BY p.id")
        .map_err(db_error)?;
    let rows = query
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error);
    rows
}

struct Episode {
    rowid: i64,
    id: String,
    title_id: String,
    updated_at: i64,
    context: Value,
}

struct SavedTitle {
    rowid: i64,
    id: String,
    added_at: i64,
    context: Value,
}

struct Hidden {
    title_id: String,
    hidden_at: i64,
}

struct Target {
    id: String,
    show: String,
    ids: Vec<ExternalId>,
}
impl Target {
    fn identity(&self) -> MediaIdentity {
        MediaIdentity {
            id: self.show.clone(),
            kind: "series".into(),
            ids: self.ids.clone(),
            episode: None,
            edition: None,
        }
    }
}

async fn migrate(app: &App, profile: i64) -> Result<(), ApiError> {
    let mut shows = HashMap::new();
    for row in episodes(app, profile)? {
        let Some((release, coordinates)) = release_episode(&row) else {
            continue;
        };
        let Some(show) = show_of(app, &mut shows, &release).await else {
            continue;
        };
        let Some(tvdb) = show.tvdb_episode(&coordinates) else {
            continue;
        };
        let target = episode_target(show, &row.context["engine_parent_identity"]["ids"], tvdb);
        apply_episode(app, profile, &row, &target, tvdb)?;
    }
    for row in saved_titles(app, profile)? {
        let Some(release) = release_of_saved_title(&row) else {
            continue;
        };
        let Some(show) = show_of(app, &mut shows, &release).await else {
            continue;
        };
        let target = target(show, &row.context["identity"]["ids"]);
        apply_saved_title(app, profile, &row, &target)?;
    }
    for row in hidden_titles(app, profile)? {
        let Some(release) = release_id(&row.title_id, &Value::Null) else {
            continue;
        };
        let Some(show) = show_of(app, &mut shows, &release).await else {
            continue;
        };
        apply_hidden_title(app, profile, &row, &show.identity().id)?;
    }
    Ok(())
}

fn episodes(app: &App, profile: i64) -> Result<Vec<Episode>, ApiError> {
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    let mut query = db
        .prepare("SELECT rowid,id,title_id,updated_at,context FROM progress WHERE profile_id=?1 AND type='series' ORDER BY rowid LIMIT ?2")
        .map_err(db_error)?;
    let rows = query
        .query_map(params![profile, MAX_ROWS as i64], |r| {
            Ok(Episode {
                rowid: r.get(0)?,
                id: r.get(1)?,
                title_id: r.get(2)?,
                updated_at: r.get(3)?,
                context: serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or(Value::Null),
            })
        })
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error);
    rows
}

fn saved_titles(app: &App, profile: i64) -> Result<Vec<SavedTitle>, ApiError> {
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    let mut query = db
        .prepare("SELECT rowid,id,COALESCE(added_at_millis,0),context FROM favorites WHERE profile_id=?1 AND type='series' ORDER BY rowid LIMIT ?2")
        .map_err(db_error)?;
    let rows = query
        .query_map(params![profile, MAX_ROWS as i64], |r| {
            Ok(SavedTitle {
                rowid: r.get(0)?,
                id: r.get(1)?,
                added_at: r.get(2)?,
                context: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or(Value::Null),
            })
        })
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error);
    rows
}

fn hidden_titles(app: &App, profile: i64) -> Result<Vec<Hidden>, ApiError> {
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    let mut query = db
        .prepare("SELECT h.title_id,COALESCE(t.hidden_at,0) FROM queue_hidden h LEFT JOIN queue_hidden_times t ON t.profile_id=h.profile_id AND t.type=h.type AND t.title_id=h.title_id WHERE h.profile_id=?1 AND h.type='series' ORDER BY h.title_id LIMIT ?2")
        .map_err(db_error)?;
    let rows = query
        .query_map(params![profile, MAX_ROWS as i64], |r| {
            Ok(Hidden {
                title_id: r.get(0)?,
                hidden_at: r.get(1)?,
            })
        })
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error);
    rows
}

async fn show_of<'a>(
    app: &App,
    shows: &'a mut HashMap<String, Option<AnimeShow>>,
    release: &MediaIdentity,
) -> Option<&'a AnimeShow> {
    if !shows.contains_key(&release.id) {
        let show = anime_show::resolve(app, release).await;
        shows.insert(release.id.clone(), show);
    }
    shows.get(&release.id).and_then(Option::as_ref)
}

fn release_episode(row: &Episode) -> Option<(MediaIdentity, EpisodeCoordinates)> {
    let context = &row.context;
    let parent = &context["engine_parent_identity"];
    let coordinates = EpisodeCoordinates {
        order: "aired".into(),
        season: context["season"]
            .as_u64()
            .or(context["engine_identity"]["episode"]["season"].as_u64())
            .and_then(|season| u32::try_from(season).ok()),
        episode: context["episode"]
            .as_u64()
            .or(context["engine_identity"]["episode"]["episode"].as_u64())
            .and_then(|episode| u32::try_from(episode).ok())?,
    };
    let release = parent["id"]
        .as_str()
        .or(context["series_id"].as_str())
        .and_then(|id| release_id(id, context))
        .or_else(|| release_id(&row.id, context))
        .or_else(|| release_id(&row.title_id, context))?;
    Some((release, coordinates))
}

fn release_of_saved_title(row: &SavedTitle) -> Option<MediaIdentity> {
    let context = &row.context;
    context["identity"]["id"]
        .as_str()
        .and_then(|id| release_id(id, context))
        .or_else(|| release_id(&row.id, context))
}

fn release_id(candidate: &str, context: &Value) -> Option<MediaIdentity> {
    let without_episode = title_of(candidate);
    let id = without_episode
        .as_deref()
        .into_iter()
        .chain(std::iter::once(candidate))
        .find(|id| anime_show::anime_release(id))?
        .to_owned();
    let mut identity = MediaIdentity {
        id,
        kind: "series".into(),
        ids: Vec::new(),
        episode: None,
        edition: None,
    };
    for stored in [
        &context["engine_parent_identity"]["ids"],
        &context["identity"]["ids"],
    ] {
        merge_stored(&mut identity.ids, stored);
    }
    Some(identity)
}

fn title_of(id: &str) -> Option<String> {
    let parts: Vec<&str> = id.split(':').collect();
    let season = parts.get(parts.len().checked_sub(2)?)?;
    let episode = parts.last()?;
    (parts.len() > 2
        && season.bytes().all(|b| b.is_ascii_digit())
        && !episode.is_empty()
        && episode.bytes().all(|b| b.is_ascii_digit()))
    .then(|| parts[..parts.len() - 2].join(":"))
}

fn merge_stored(ids: &mut Vec<ExternalId>, stored: &Value) {
    for id in stored.as_array().into_iter().flatten() {
        if ids.len() == 32 {
            return;
        }
        let Ok(id) = serde_json::from_value::<ExternalId>(id.clone()) else {
            continue;
        };
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
}

fn target(show: &AnimeShow, stored: &Value) -> Target {
    let identity = show.identity();
    let mut ids = identity.ids.clone();
    merge_stored(&mut ids, stored);
    Target {
        id: identity.id.clone(),
        show: identity.id.clone(),
        ids,
    }
}

fn episode_target(show: &AnimeShow, stored: &Value, tvdb: TvdbEpisode) -> Target {
    let mut target = target(show, stored);
    target.id = format!("{0}:{1}:{2}", target.show, tvdb.season, tvdb.episode);
    target
}

fn identity(id: &str, ids: &[ExternalId], episode: Option<(u32, u32)>) -> Value {
    json!({
        "id":id,
        "kind":"series",
        "ids":ids,
        "episode":episode.map(|(season, episode)| json!({"order":"aired","season":season,"episode":episode})),
        "edition":null,
    })
}

fn episode_context(context: &Value, target: &Target, tvdb: TvdbEpisode) -> Value {
    let mut context = context.clone();
    context["series_id"] = json!(target.show);
    context["season"] = json!(tvdb.season);
    context["episode"] = json!(tvdb.episode);
    context["engine_identity"] = identity(
        &target.id,
        &target.ids,
        Some((tvdb.season, tvdb.episode)),
    );
    context["engine_parent_identity"] = identity(&target.show, &target.ids, None);
    context
}

fn apply_episode(
    app: &App,
    profile: i64,
    row: &Episode,
    target: &Target,
    tvdb: TvdbEpisode,
) -> Result<(), ApiError> {
    let mut db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    let tx = db.transaction().map_err(db_error)?;
    let existing: Option<(i64, i64)> = tx
        .query_row(
            "SELECT rowid,updated_at FROM progress WHERE profile_id=?1 AND type='series' AND id=?2",
            params![profile, target.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(db_error)?;
    if let Some((rowid, updated_at)) = existing {
        if updated_at >= row.updated_at {
            tx.execute("DELETE FROM progress WHERE rowid=?1", [row.rowid])
                .map_err(db_error)?;
            tx.commit().map_err(db_error)?;
            return Ok(());
        }
        tx.execute("DELETE FROM progress WHERE rowid=?1", [rowid])
            .map_err(db_error)?;
    }
    tx.execute(
        "UPDATE progress SET id=?2,title_id=?3,context=?4,poster=COALESCE(NULLIF(poster,''),?6) WHERE rowid=?1 AND id=?5",
        params![
            row.rowid,
            target.id,
            target.show,
            episode_context(&row.context, target, tvdb).to_string(),
            row.id,
            super::art::identity_poster(&target.identity())
        ],
    )
    .map_err(db_error)?;
    tx.execute(
        "UPDATE OR REPLACE engine_watch_counts SET id=?3 WHERE profile_id=?1 AND type='series' AND id=?2",
        params![profile, row.id, target.id],
    )
    .map_err(db_error)?;
    tx.commit().map_err(db_error)?;
    Ok(())
}

fn apply_saved_title(
    app: &App,
    profile: i64,
    row: &SavedTitle,
    target: &Target,
) -> Result<(), ApiError> {
    if row.id == target.id {
        return Ok(());
    }
    let mut context = row.context.clone();
    if context["identity"].is_object() {
        context["identity"] = identity(&target.id, &target.ids, None);
    }
    let mut db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    let tx = db.transaction().map_err(db_error)?;
    let existing: Option<(i64, i64)> = tx
        .query_row(
            "SELECT rowid,COALESCE(added_at_millis,0) FROM favorites WHERE profile_id=?1 AND type='series' AND id=?2",
            params![profile, target.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(db_error)?;
    if let Some((rowid, added_at)) = existing {
        if added_at >= row.added_at {
            tx.execute("DELETE FROM favorites WHERE rowid=?1", [row.rowid])
                .map_err(db_error)?;
            tx.commit().map_err(db_error)?;
            return Ok(());
        }
        tx.execute("DELETE FROM favorites WHERE rowid=?1", [rowid])
            .map_err(db_error)?;
    }
    tx.execute(
        "UPDATE favorites SET id=?2,context=?3,poster=COALESCE(NULLIF(poster,''),?5) WHERE rowid=?1 AND id=?4",
        params![
            row.rowid,
            target.id,
            context.to_string(),
            row.id,
            super::art::identity_poster(&target.identity())
        ],
    )
    .map_err(db_error)?;
    tx.commit().map_err(db_error)?;
    Ok(())
}

fn apply_hidden_title(
    app: &App,
    profile: i64,
    row: &Hidden,
    show: &str,
) -> Result<(), ApiError> {
    if row.title_id == show {
        return Ok(());
    }
    let mut db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    let tx = db.transaction().map_err(db_error)?;
    tx.execute(
        "INSERT OR IGNORE INTO queue_hidden(profile_id,type,title_id) VALUES(?1,'series',?2)",
        params![profile, show],
    )
    .map_err(db_error)?;
    tx.execute(
        "INSERT OR REPLACE INTO queue_hidden_times(profile_id,type,title_id,hidden_at) VALUES(?1,'series',?2,?3)",
        params![profile, show, row.hidden_at],
    )
    .map_err(db_error)?;
    tx.execute(
        "DELETE FROM queue_hidden WHERE profile_id=?1 AND type='series' AND title_id=?2",
        params![profile, row.title_id],
    )
    .map_err(db_error)?;
    tx.commit().map_err(db_error)?;
    Ok(())
}
