use super::*;

pub fn record_startup(db: &Connection, id: &str, attempts: &[Value]) -> rusqlite::Result<()> {
    let data = json!({"at":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),"attempts":attempts});
    db.execute("INSERT INTO family_startups(channel_id,data) VALUES(?1,?2) ON CONFLICT(channel_id) DO UPDATE SET data=excluded.data",params![id,data.to_string()])?;
    Ok(())
}

pub(super) async fn save(
    a: App,
    lease: ResourceLease,
    id: String,
    v: Value,
    existing: bool,
) -> ApiResult {
    blocking(move || save_record(a, lease, id, v, existing)).await
}
fn save_record(a: App, lease: ResourceLease, id: String, v: Value, existing: bool) -> ApiResult {
    let name = text(&v, "name", 128)?;
    let network = text(&v, "network", 128)?;
    let feed = text(&v, "feed", 16)?;
    if !["east", "west", "national", "local"].contains(&feed) {
        return Err("Invalid feed".into());
    }
    let market = v["market"].as_str().unwrap_or("").trim();
    if market.len() > 128
        || market.chars().any(char::is_control)
        || (feed == "local" && market.is_empty())
    {
        return Err("Local stations require a market".into());
    }
    let category = text(&v, "category", 64)?;
    let number = v["number"]
        .as_u64()
        .filter(|n| (1..=99999).contains(n))
        .ok_or("Invalid channel number")?;
    let enabled = v["enabled"].as_bool().ok_or("Invalid channel visibility")?;
    let candidates = v["candidates"]
        .as_array()
        .filter(|c| c.len() <= 20)
        .ok_or("Select at most 20 candidates")?;
    let mut db = a.db.lock().unwrap();
    lease.validate(&db)?;
    let tx = db.transaction().map_err(db_error)?;
    let all = channels(&tx).map_err(db_error)?;
    let old = all.iter().find(|c| c["id"] == id);
    if existing && old.is_none() {
        return Err(ApiError(StatusCode::NOT_FOUND, "Channel not found".into()));
    }
    if let Some(old) = old {
        if old["network"] != network || old["feed"] != feed || old["market"] != market {
            return Err("Create a new channel to change its network, feed or market".into());
        }
    }
    if all.iter().any(|c| c["id"] != id && c["number"] == number) {
        return Err("Channel number is already used".into());
    }
    let count = all
        .iter()
        .filter(|c| c["id"] != id && c["enabled"] == true)
        .count();
    if enabled
        && count
            >= settings(&tx).map_err(db_error)?["limit"]
                .as_u64()
                .unwrap_or(100) as usize
    {
        return Err("Visible channel limit reached".into());
    }
    let mut saved = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for c in candidates {
        let live_id = text(c, "id", 128)?;
        if c["verified"] != true || !seen.insert(live_id.to_owned()) {
            return Err("Verify every candidate belongs to this exact US English feed".into());
        }
        let source = tx
            .query_row(
                "SELECT name FROM provider_live WHERE id=?1",
                [&live_id],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(db_error)?;
        let prior = old
            .and_then(|o| o["candidates"].as_array())
            .and_then(|cs| cs.iter().find(|c| c["id"] == live_id));
        let source_name = source
            .or_else(|| prior.and_then(|c| c["name"].as_str().map(str::to_owned)))
            .ok_or("Candidate not found")?;
        if c["name"].as_str() != Some(source_name.as_str()) {
            return Err(
                "Candidate changed; reload and verify its current name before saving".into(),
            );
        }
        let words = source_name.to_lowercase();
        let words = words
            .split(|c: char| !c.is_alphanumeric())
            .collect::<Vec<_>>();
        if (words.contains(&"east") && feed != "east")
            || (words.contains(&"west") && feed != "west")
        {
            return Err("Candidate conflicts with this channel's exact feed".into());
        }
        let alias = tx
            .query_row(
                "SELECT channel_id FROM family_aliases WHERE live_id=?1",
                [&live_id],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(db_error)?;
        if alias.is_some_and(|a| a != id) {
            return Err("Candidate already belongs to another channel".into());
        }
        saved.push(json!({"id":live_id,"name":source_name,"verified":true}));
    }
    let data = json!({"id":id,"name":name,"network":network,"country":"US","language":"en","feed":feed,"market":market,"category":category,"number":number,"enabled":enabled,"candidates":saved});
    tx.execute("INSERT INTO family_channels(id,data) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET data=excluded.data",params![id,data.to_string()]).map_err(db_error)?;
    tx.execute("DELETE FROM family_candidates WHERE channel_id=?1", [&id])
        .map_err(db_error)?;
    for (rank, c) in saved.iter().enumerate() {
        tx.execute(
            "INSERT INTO family_candidates(channel_id,live_id,rank) VALUES(?1,?2,?3)",
            params![id, c["id"].as_str(), rank],
        )
        .map_err(db_error)?;
        tx.execute(
            "INSERT OR IGNORE INTO family_aliases(live_id,channel_id) VALUES(?1,?2)",
            params![c["id"].as_str(), id],
        )
        .map_err(db_error)?;
    }
    matching::manual_saved(&tx, &id, old, &saved).map_err(db_error)?;
    tx.commit().map_err(db_error)?;
    Ok(axum::Json(data))
}
