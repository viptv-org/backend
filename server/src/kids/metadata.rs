use super::*;

fn known_age(value: &str) -> Option<i64> {
    match value.trim().to_uppercase().as_str() {
        "G" | "TV-G" | "TV-Y" => Some(0),
        "TV-Y7" | "TV-Y7-FV" => Some(7),
        "PG" | "TV-PG" => Some(10),
        "PG-13" => Some(13),
        "TV-14" => Some(14),
        "R" => Some(17),
        "NC-17" | "TV-MA" => Some(18),
        _ => None,
    }
}
fn assessment(meta: &Value) -> (Option<i64>, bool) {
    let mut ages = HashSet::new();
    let mut unknown = false;
    for key in ["certification", "contentRating", "ageRating", "mpaaRating"] {
        if let Some(v) = meta.get(key) {
            if let Some(age) = v.as_str().and_then(known_age) {
                ages.insert(age);
            } else {
                unknown = true;
            }
        }
    }
    if meta["genres"].as_array().is_some_and(|v| {
        v.iter().any(|v| {
            v.as_str().is_some_and(|v| {
                matches!(v.to_lowercase().as_str(), "adult" | "porn" | "pornography")
            })
        })
    }) {
        ages.insert(18);
    }
    let age = ages.iter().max().copied();
    (age, unknown || ages.len() > 1)
}
/// Only call with metadata returned by an account's configured server-side addon fetch.
// Store only presentation and policy fields; addon extension blobs never enter the policy cache.
fn compact_metadata(meta: &Value) -> Value {
    let mut out = json!({});
    for key in [
        "id",
        "type",
        "name",
        "poster",
        "background",
        "logo",
        "description",
        "releaseInfo",
        "year",
        "runtime",
        "genres",
        "cast",
        "director",
        "imdbRating",
        "contentRating",
        "certification",
        "ageRating",
        "mpaaRating",
    ] {
        if let Some(value) = meta.get(key) {
            out[key] = match value {
                Value::String(text) => json!(text.chars().take(4096).collect::<String>()),
                Value::Array(items) => Value::Array(
                    items
                        .iter()
                        .take(32)
                        .filter(|v| v.is_string())
                        .map(|v| {
                            json!(v
                                .as_str()
                                .unwrap_or("")
                                .chars()
                                .take(256)
                                .collect::<String>())
                        })
                        .collect(),
                ),
                Value::Number(_) | Value::Bool(_) => value.clone(),
                _ => Value::Null,
            };
        }
    }
    if let Some(videos) = meta["videos"].as_array() {
        out["videos"] = Value::Array(
            videos
                .iter()
                .take(2000)
                .map(|video| {
                    let mut item = json!({});
                    for key in [
                        "id",
                        "season",
                        "episode",
                        "title",
                        "released",
                        "thumbnail",
                        "contentRating",
                        "certification",
                        "ageRating",
                        "mpaaRating",
                    ] {
                        if let Some(value) = video.get(key) {
                            item[key] = match value {
                                Value::String(text) => json!(text
                                    .chars()
                                    .take(if key == "thumbnail" { 1024 } else { 512 })
                                    .collect::<String>()),
                                Value::Number(_) | Value::Bool(_) => value.clone(),
                                _ => Value::Null,
                            };
                        }
                    }
                    item
                })
                .collect(),
        );
    }
    out
}
pub(crate) fn observe(
    db: &Connection,
    account: i64,
    kind: &str,
    meta: &Value,
    keep_unknown: bool,
) -> Result<(), ApiError> {
    if !["movie", "series"].contains(&kind) {
        return Ok(());
    }
    let Some(id) = meta["id"]
        .as_str()
        .filter(|v| !v.is_empty() && v.len() <= 512)
    else {
        return Ok(());
    };
    if !meta["name"].is_string() {
        return Ok(());
    }
    let (age, conflict) = assessment(meta);
    if age.is_none() && !keep_unknown && !conflict {
        return Ok(());
    }
    let mut metadata = compact_metadata(meta);
    let previous:Option<(Option<i64>,bool,String)>=db.query_row("SELECT age,conflict,metadata FROM kids_media WHERE account_id=?1 AND kind=?2 AND id=?3",params![account,kind,id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(db_error)?;
    if let Some((old_age, old_conflict, old_text)) = previous {
        let old: Value = serde_json::from_str(&old_text).unwrap_or(Value::Null);
        if metadata.get("videos").is_none() && old["videos"].is_array() {
            metadata["videos"] = old["videos"].clone();
        }
        let old_ids = old["videos"].as_array().map(|v| {
            v.iter()
                .filter_map(|v| v["id"].as_str())
                .collect::<HashSet<_>>()
        });
        let new_ids = metadata["videos"].as_array().map(|v| {
            v.iter()
                .filter_map(|v| v["id"].as_str())
                .collect::<HashSet<_>>()
        });
        if old_age != age || (!old_conflict && conflict) || old_ids != new_ids {
            db.execute("UPDATE kids_profiles SET revision=revision+1 WHERE enabled=1 AND profile_id IN(SELECT profile_id FROM profile_owners WHERE account_id=?1)",[account]).map_err(db_error)?;
        }
    }
    let encoded = metadata.to_string();

    db.execute("INSERT INTO kids_media(account_id,kind,id,parent_id,metadata,age,conflict,updated_at) VALUES(?1,?2,?3,?3,?4,?5,?6,?7) ON CONFLICT(account_id,kind,id) DO UPDATE SET metadata=excluded.metadata,age=COALESCE(MAX(kids_media.age,excluded.age),excluded.age,kids_media.age),conflict=MAX(kids_media.conflict,excluded.conflict,CASE WHEN kids_media.age IS NOT NULL AND excluded.age IS NOT NULL AND kids_media.age<>excluded.age THEN 1 ELSE 0 END),updated_at=excluded.updated_at",params![account,kind,id,encoded,age,conflict,util::now()]).map_err(db_error)?;
    let mut episode_policy_changed = false;
    if kind == "series" {
        // Exact video IDs are learned here, never from a playback/progress request's series_id.
        if let Some(videos) = meta["videos"]
            .as_array()
            .or_else(|| metadata["videos"].as_array())
        {
            for video in videos.iter().take(2000) {
                let Some(episode) = video["id"]
                    .as_str()
                    .filter(|v| !v.is_empty() && v.len() <= 512 && *v != id)
                else {
                    continue;
                };
                let quarantined=db.execute("INSERT OR IGNORE INTO kids_ambiguous(account_id,kind,id) SELECT account_id,kind,id FROM kids_media WHERE account_id=?1 AND kind='series' AND id=?2 AND parent_id<>?3",params![account,episode,id]).map_err(db_error)?;
                episode_policy_changed |= quarantined > 0;
                let (episode_age, episode_conflict) = assessment(video);
                let episode_age = match (age, episode_age) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    (a, b) => a.or(b),
                };
                let previous_episode:Option<(Option<i64>,bool)>=db.query_row("SELECT age,conflict FROM kids_media WHERE account_id=?1 AND kind='series' AND id=?2",params![account,episode],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(db_error)?;
                if previous_episode.is_some_and(|(prior_age, prior_conflict)| {
                    prior_age != episode_age || (!prior_conflict && episode_conflict)
                }) {
                    episode_policy_changed = true;
                }
                let compact = compact_metadata(&json!({"videos":[video]}));
                let mut item = compact["videos"][0].clone();
                item["name"] = metadata["name"].clone();
                item["poster"] = metadata["poster"].clone();
                item["series_id"] = json!(id);
                item["type"] = json!("series");
                db.execute("INSERT INTO kids_media(account_id,kind,id,parent_id,metadata,age,conflict,updated_at) VALUES(?1,'series',?2,?3,?4,?5,?6,?7) ON CONFLICT(account_id,kind,id) DO UPDATE SET metadata=excluded.metadata,age=excluded.age,conflict=MAX(kids_media.conflict,excluded.conflict,CASE WHEN kids_media.parent_id<>excluded.parent_id THEN 1 ELSE 0 END),updated_at=excluded.updated_at",params![account,episode,id,item.to_string(),episode_age,episode_conflict,util::now()]).map_err(db_error)?;
            }
        }
    }
    if episode_policy_changed {
        db.execute("UPDATE kids_profiles SET revision=revision+1 WHERE enabled=1 AND profile_id IN(SELECT profile_id FROM profile_owners WHERE account_id=?1)",[account]).map_err(db_error)?;
    }
    Ok(())
}
pub(super) fn prune_observations(db: &Connection, account: i64) -> Result<(), ApiError> {
    // Bound once per response, rather than sorting the cache for every catalog item.
    db.execute("DELETE FROM kids_media WHERE account_id=?1 AND rowid NOT IN(SELECT rowid FROM kids_media WHERE account_id=?1 ORDER BY updated_at DESC,rowid DESC LIMIT 10000) AND NOT EXISTS(SELECT 1 FROM kids_approvals a JOIN profile_owners o ON o.profile_id=a.profile_id WHERE o.account_id=?1 AND a.kind=kids_media.kind AND a.id=kids_media.parent_id)",[account]).map_err(db_error)?;
    Ok(())
}
pub(super) type MediaRecord = (Value, String, Option<i64>, bool);
pub(super) fn media(
    db: &Connection,
    p: &auth::Principal,
    kind: &str,
    id: &str,
) -> Result<Option<MediaRecord>, ApiError> {
    db.query_row("SELECT metadata,parent_id,age,conflict FROM kids_media WHERE account_id=?1 AND kind=?2 AND id=?3",params![p.account_id(),kind,id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<i64>>(2)?,r.get::<_,bool>(3)?))).optional().map_err(db_error)?.map(|(text,parent,age,conflict)|serde_json::from_str(&text).map(|meta|(meta,parent,age,conflict)).map_err(|_|ApiError::from("Title metadata unavailable"))).transpose()
}

#[derive(Deserialize)]
pub(crate) struct ParentSearch {
    #[serde(rename = "type")]
    kind: String,
    search: String,
}
pub(crate) async fn search(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Query(query): Query<ParentSearch>,
) -> ApiResult {
    let app = app.with_lease(lease);
    if !["movie", "series"].contains(&query.kind.as_str())
        || !(2..=128).contains(&query.search.trim().len())
    {
        return Err("Search for a movie or series using 2 to 128 characters".into());
    }
    {
        let db = app.db.lock().unwrap();
        app.request_lease().validate(&db)?;
        manager(&db, &app.identity(), None)?;
    }
    let result = tokio::time::timeout(
        Duration::from_secs(15),
        app.addons.discover_with_options(addon::DiscoverOptions {
            kind: query.kind.clone(),
            catalog: None,
            addon: None,
            skip: 0,
            search: Some(query.search.trim().into()),
            genre: None,
            extras: HashMap::new(),
        }),
    )
    .await
    .map_err(|_| "Title search timed out")??;
    blocking(move || {
        let mut guard = app.db.lock().unwrap();
        let db = guard.transaction().map_err(db_error)?;
        app.request_lease().validate(&db)?;
        let account = manager(&db, &app.identity(), None)?;
        let mut items = Vec::new();
        for meta in result["metas"].as_array().into_iter().flatten().take(40) {
            if !meta["id"].is_string()
                || !meta["name"].is_string()
                || assessment(meta).0.is_some_and(|age| age >= 18)
            {
                continue;
            }
            observe(&db, account, &query.kind, meta, false)?;
            items.push(
            json!({"id":meta["id"],"type":query.kind,"name":meta["name"],"poster":meta["poster"]}),
        );
        }
        prune_observations(&db, account)?;
        db.commit().map_err(db_error)?;
        Ok(axum::Json(json!({"items":items})))
    })
    .await
}
