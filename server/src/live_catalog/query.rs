use super::*;

fn value(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or("").to_owned()
}

pub fn channels(db: &Connection) -> Result<Vec<Value>, String> {
    let mut rows = if lineup::enabled(db)? {
        lineup::live(db, None, None, 0, 500)?["channels"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    } else {
        let mut stmt=db.prepare("SELECT l.id,l.name,l.logo,l.category FROM provider_live l JOIN providers p ON p.id=l.provider_id WHERE p.enabled=1 AND p.enable_live=1 ORDER BY l.id").map_err(|_|"Live inventory unavailable")?;
        let rows = stmt.query_map([],|r|Ok(json!({"id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"logo":r.get::<_,Option<String>>(2)?,"category":r.get::<_,Option<String>>(3)?}))).map_err(|_|"Live inventory unavailable")?.collect::<Result<Vec<_>,_>>().map_err(|_|"Live inventory unavailable")?;
        rows
    };
    rows.retain_mut(|row| {
        let name = display_name(&value(row, "name"));
        row["name"] = json!(name);
        let category = value(row, "category");
        let recognized = classify(&name, &category).or_else(|| {
            if value(row, "id").starts_with("family:")
                && !prohibited(&name, &category)
                && !foreign(&words(&name))
            {
                classify(&value(row, "network"), &category)
            } else {
                None
            }
        });
        if let Some((rank, network)) = recognized {
            row["section"] = json!(GROUPS[rank]);
            row["section_rank"] = json!(rank);
            row["network"] = json!(network);
            row["type"] = json!("live");
            crate::live_policy::visible(db, &value(row, "id"))
        } else {
            false
        }
    });
    rows.sort_by_key(|v| {
        (
            v["section_rank"].as_u64().unwrap_or(99),
            words(&value(v, "network")),
            words(&value(v, "name")),
            value(v, "id"),
        )
    });
    for (index, row) in rows.iter_mut().enumerate() {
        row["number"] = json!(index + 1);
    }
    Ok(rows)
}

fn current(db: &Connection, now: i64) -> Result<HashMap<String, Value>, String> {
    let mut found = HashMap::new();
    let mut stmt=db.prepare("SELECT channel_id,data,start,end FROM family_programmes WHERE start<=?1 AND end>?1 ORDER BY start DESC").map_err(|_|"Guide unavailable")?;
    let rows = stmt
        .query_map([now], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })
        .map_err(|_| "Guide unavailable")?;
    for row in rows {
        let (id, data, start, end) = row.map_err(|_| "Guide unavailable")?;
        if let Ok(mut v) = serde_json::from_str::<Value>(&data) {
            v["start"] = json!(start);
            v["end"] = json!(end);
            found.entry(id).or_insert(v);
        }
    }
    let mut stmt=db.prepare("SELECT l.id,c.payload FROM provider_live l JOIN provider_cache c ON c.provider_id=l.provider_id AND c.cache_key='get_short_epg:'||l.stream_id WHERE c.expires_at>?1").map_err(|_|"Guide cache unavailable")?;
    for row in stmt
        .query_map([now], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(|_| "Guide cache unavailable")?
    {
        let (id, data) = row.map_err(|_| "Guide cache unavailable")?;
        let Ok(v) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        for p in v["epg_listings"].as_array().into_iter().flatten().take(100) {
            let time = |k: &str| p[k].as_i64().or_else(|| p[k].as_str()?.parse().ok());
            let start = time("start_timestamp").unwrap_or(0);
            let end = time("stop_timestamp")
                .or_else(|| time("end_timestamp"))
                .unwrap_or(0);
            if start <= now && end > now {
                use base64::Engine;
                let raw = value(p, "title");
                let title = base64::engine::general_purpose::STANDARD
                    .decode(&raw)
                    .ok()
                    .and_then(|b| String::from_utf8(b).ok())
                    .unwrap_or(raw);
                found
                    .entry(id.clone())
                    .or_insert(json!({"title":title,"start":start,"end":end}));
                break;
            }
        }
    }
    Ok(found)
}

pub fn browse(
    db: &Connection,
    category: Option<&str>,
    search: Option<&str>,
    collection: Option<&str>,
    profile: Option<i64>,
    offset: usize,
    limit: usize,
) -> Result<Value, String> {
    let mut rows = channels(db)?;
    let current = current(db, util::now())?;
    if let Some(kind) = collection.filter(|v| matches!(*v, "recent" | "favorites")) {
        let profile = profile.ok_or("Choose a profile")?;
        let table = if kind == "recent" {
            "progress"
        } else {
            "favorites"
        };
        let mut stmt = db
            .prepare(&format!(
                "SELECT id FROM {table} WHERE profile_id=?1 AND type='live' ORDER BY {}",
                if kind == "recent" {
                    "updated_at DESC,id"
                } else {
                    "name,id"
                }
            ))
            .map_err(|_| "Saved channels unavailable")?;
        let ids = stmt
            .query_map([profile], |r| r.get::<_, String>(0))
            .map_err(|_| "Saved channels unavailable")?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "Saved channels unavailable")?;
        let mut ranks = HashMap::new();
        for (rank, id) in ids.iter().enumerate() {
            let canonical =
                lineup::canonical_id(db, id).map_err(|_| "Saved channels unavailable")?;
            ranks.entry(canonical).or_insert(rank);
        }
        rows.retain(|v| ranks.contains_key(&value(v, "id")));
        if kind == "recent" {
            rows.sort_by_key(|v| ranks[&value(v, "id")]);
        }
    }
    let raw_query = words(search.unwrap_or(""));
    let normalized_query = if matches!(
        raw_query.as_str(),
        "cn" | "nick" | "fs1" | "fs2" | "espn2" | "nat geo" | "tcm"
    ) {
        key(&raw_query)
    } else {
        raw_query
    };
    let query = normalized_query.chars().take(128).collect::<String>();
    let terms = query.split_whitespace().collect::<Vec<_>>();
    rows.retain_mut(|v| {
        if let Some(p) = current.get(&value(v, "id")) {
            if prohibited(&value(p, "title"), "") {
                return false;
            }
            v["now"] = p.clone();
        }
        if category.is_some_and(|g| {
            !g.is_empty() && g != "all" && g.trim_start_matches("section:") != value(v, "section")
        }) {
            return false;
        }
        let channel = words(&format!("{} {}", value(v, "name"), value(v, "network")));
        let group = words(&value(v, "section"));
        let title = words(&value(&v["now"], "title"));
        terms
            .iter()
            .all(|term| channel.contains(term) || group.contains(term) || title.contains(term))
    });
    let total = rows.len();
    Ok(
        json!({"channels":rows.into_iter().skip(offset).take(limit.min(200)).collect::<Vec<_>>(),"total":total,"search_scope":"US channels, sections and currently airing programmes with available guide data"}),
    )
}
pub fn categories(db: &Connection) -> Result<Value, String> {
    let rows = channels(db)?;
    let categories = GROUPS
        .iter()
        .filter_map(|group| {
            let count = rows.iter().filter(|v| v["section"] == *group).count();
            (count > 0).then(|| json!({"id":format!("section:{group}"),"name":group,"count":count}))
        })
        .collect::<Vec<_>>();
    Ok(json!({"total":categories.len(),"categories":categories}))
}
