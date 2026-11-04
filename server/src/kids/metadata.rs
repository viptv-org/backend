use super::*;

pub(super) const RESTRICTED_AGE: i64 = 18;
const ADULT_GENRES: [&str; 6] = ["adult", "porn", "pornography", "hentai", "erotica", "erotic"];
const RATING_AGES: [(&str, i64); 63] = [
    ("G", 0),
    ("TVG", 0),
    ("TVY", 0),
    ("TVY7", 7),
    ("TVY7FV", 7),
    ("PG", 10),
    ("GP", 10),
    ("TVPG", 10),
    ("PG13", 13),
    ("TV14", 14),
    ("R", 17),
    ("NC17", 18),
    ("TVMA", 18),
    ("X", 18),
    ("XX", 18),
    ("XXX", 18),
    ("NR18", 18),
    ("EC", 0),
    ("E", 0),
    ("E10", 10),
    ("T", 13),
    ("M", 17),
    ("AO", 18),
    ("U", 0),
    ("UC", 0),
    ("12A", 12),
    ("15A", 15),
    ("16A", 16),
    ("R18", 18),
    ("R21", 18),
    ("M18", 18),
    ("NC16", 16),
    ("MA15", 15),
    ("X18", 18),
    ("RC", 18),
    ("R13", 13),
    ("R15", 15),
    ("R16", 16),
    ("RP13", 13),
    ("RP16", 16),
    ("RP18", 18),
    ("PG12", 12),
    ("RX", 18),
    ("ALL", 0),
    ("L", 0),
    ("AL", 0),
    ("TP", 0),
    ("C", 0),
    ("C8", 8),
    ("14A", 14),
    ("18A", 18),
    ("UA", 12),
    ("UA7", 7),
    ("UA13", 13),
    ("UA16", 16),
    ("A", 18),
    ("S", 18),
    ("ADULT", 18),
    ("ADULTSONLY", 18),
    ("HENTAI", 18),
    ("PORN", 18),
    ("EROTICA", 18),
    ("EROTIC", 18),
];
const RATING_PREFIXES: [&str; 31] = [
    "RATED", "MPAA", "ESRB", "BBFC", "FSK", "AB", "PEGI", "KMRB", "EIRIN", "ACB", "OFLC", "CBFC",
    "USA", "US", "UK", "GB", "DE", "JP", "JA", "KR", "KO", "AU", "NZ", "CA", "FR", "BR", "IN",
    "IE", "NL", "ES", "IT",
];

fn marker_age(key: &str) -> Option<i64> {
    if !key.is_empty() && key.bytes().all(|b| b.is_ascii_digit()) {
        return key
            .parse::<i64>()
            .ok()
            .filter(|age| *age <= 21)
            .map(|age| age.min(RESTRICTED_AGE));
    }
    RATING_AGES
        .iter()
        .find(|(marker, _)| *marker == key)
        .map(|(_, age)| *age)
}
pub(super) fn known_age(value: &str) -> Option<i64> {
    marker_text_age(value).or_else(|| {
        value
            .split_once(" - ")
            .and_then(|(marker, _)| marker_text_age(marker))
    })
}
fn marker_text_age(marker: &str) -> Option<i64> {
    let tokens: Vec<String> = marker
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_uppercase)
        .collect();
    let mut start = 0;
    while start < tokens.len() {
        let key = tokens[start..].concat();
        if let Some(age) = marker_age(&key) {
            return Some(age);
        }
        let glued = RATING_PREFIXES.iter().find_map(|prefix| {
            key.strip_prefix(prefix)
                .filter(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
        });
        if let Some(rest) = glued {
            return marker_age(rest);
        }
        if !RATING_PREFIXES.contains(&tokens[start].as_str()) {
            return None;
        }
        start += 1;
    }
    None
}
fn rating_age(value: &Value) -> Option<Option<i64>> {
    match value {
        Value::Null => None,
        Value::String(text) if text.trim().is_empty() => None,
        Value::String(text) => Some(known_age(text)),
        Value::Number(number) => Some(known_age(&number.to_string())),
        _ => Some(None),
    }
}
pub(super) fn assessment(meta: &Value) -> (Option<i64>, bool) {
    let mut ages = HashSet::new();
    let mut unknown = false;
    for key in ["certification", "contentRating", "ageRating", "mpaaRating"] {
        match meta.get(key).and_then(rating_age) {
            None => {}
            Some(Some(age)) => {
                ages.insert(age);
            }
            Some(None) => {
                unknown = true;
                ages.insert(RESTRICTED_AGE);
            }
        }
    }
    if meta["genres"].as_array().is_some_and(|v| {
        v.iter().any(|v| {
            v.as_str()
                .is_some_and(|v| ADULT_GENRES.contains(&v.trim().to_lowercase().as_str()))
        })
    }) {
        ages.insert(RESTRICTED_AGE);
    }
    let age = ages.iter().max().copied();
    (age, unknown || ages.len() > 1)
}
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
fn tracks_unrated(db: &Connection, account: i64) -> Result<bool, ApiError> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM kids_profiles k JOIN profile_owners o ON o.profile_id=k.profile_id WHERE o.account_id=?1 AND k.allow_unrated=1)",
        [account],
        |r| r.get(0),
    )
    .map_err(db_error)
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
    if age.is_none() && !conflict && !keep_unknown && !tracks_unrated(db, account)? {
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
    db.execute("DELETE FROM kids_media WHERE account_id=?1 AND rowid NOT IN(SELECT rowid FROM kids_media WHERE account_id=?1 ORDER BY updated_at DESC,rowid DESC LIMIT 10000) AND NOT EXISTS(SELECT 1 FROM kids_approvals a JOIN profile_owners o ON o.profile_id=a.profile_id WHERE o.account_id=?1 AND a.kind=kids_media.kind AND a.id=kids_media.parent_id)",[account]).map_err(db_error)?;
    Ok(())
}
pub(super) type MediaRecord = (Value, String, Option<i64>, bool);
pub(super) fn media_batch(
    db: &Connection,
    p: &auth::Principal,
    keys: &[(String, String)],
) -> Result<HashMap<(String, String), Value>, ApiError> {
    let mut out = HashMap::new();
    if keys.is_empty() {
        return Ok(out);
    }
    let encoded = serde_json::to_string(
        &keys
            .iter()
            .map(|(kind, id)| json!({"k": kind, "i": id}))
            .collect::<Vec<_>>(),
    )
    .map_err(|_| ApiError::from("Title metadata unavailable"))?;
    let mut stmt = db
        .prepare(
            "SELECT json_extract(j.value,'$.k'),json_extract(j.value,'$.i'),m.metadata FROM json_each(?1) AS j JOIN kids_media m ON m.account_id=?2 AND m.kind=json_extract(j.value,'$.k') AND m.id=json_extract(j.value,'$.i')",
        )
        .map_err(db_error)?;
    let rows = stmt
        .query_map(
            params![encoded, p.account_id()],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
        )
        .map_err(db_error)?;
    for row in rows {
        let (kind, id, metadata) = row.map_err(db_error)?;
        let metadata: Value = serde_json::from_str(&metadata)
            .map_err(|_| ApiError::from("Title metadata unavailable"))?;
        out.insert((kind, id), metadata);
    }
    Ok(out)
}
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
            values: vec![],
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
