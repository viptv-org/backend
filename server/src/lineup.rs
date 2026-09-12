//! Curated identities survive provider refreshes. Only explicit owner verification
//! creates a legacy alias; raw inventory remains available during migration.
use super::*;
use rusqlite::OptionalExtension;
pub(crate) mod matching;

pub fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS family_settings(id INTEGER PRIMARY KEY CHECK(id=1),enabled INTEGER NOT NULL DEFAULT 0,visible_limit INTEGER NOT NULL DEFAULT 100);
        INSERT OR IGNORE INTO family_settings(id) VALUES(1);
        CREATE TABLE IF NOT EXISTS family_recovery(id INTEGER PRIMARY KEY CHECK(id=1),data TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS family_channels(id TEXT PRIMARY KEY, data TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS family_candidates(channel_id TEXT NOT NULL REFERENCES family_channels(id),live_id TEXT NOT NULL UNIQUE,rank INTEGER NOT NULL,PRIMARY KEY(channel_id,live_id));
        CREATE TABLE IF NOT EXISTS family_startups(channel_id TEXT PRIMARY KEY REFERENCES family_channels(id),data TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS family_aliases(live_id TEXT PRIMARY KEY,channel_id TEXT NOT NULL REFERENCES family_channels(id));")?;
    matching::init(db)
}
#[derive(Clone, Copy, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecoveryPolicy {
    pub stall_seconds: u64,
    pub attempt_seconds: u64,
    pub deadline_seconds: u64,
    pub max_recoveries: u64,
}
impl Default for RecoveryPolicy {
    fn default() -> Self {
        Self {
            stall_seconds: 20,
            attempt_seconds: 20,
            deadline_seconds: 45,
            max_recoveries: 2,
        }
    }
}
impl RecoveryPolicy {
    fn validated(value: Value) -> Result<Self, ApiError> {
        let p: Self = serde_json::from_value(value)
            .map_err(|_| ApiError::from("Invalid recovery settings"))?;
        if !(5..=60).contains(&p.stall_seconds)
            || !(5..=30).contains(&p.attempt_seconds)
            || !(10..=45).contains(&p.deadline_seconds)
            || p.attempt_seconds > p.deadline_seconds
            || p.max_recoveries > 5
        {
            return Err("Recovery settings require stall 5–60s, attempt 5–30s, deadline 10–45s (at least the attempt), and 0–5 recoveries".into());
        }
        Ok(p)
    }
}
pub(crate) fn recovery_policy(db: &Connection) -> rusqlite::Result<RecoveryPolicy> {
    let data: Option<String> = db
        .query_row("SELECT data FROM family_recovery WHERE id=1", [], |r| {
            r.get(0)
        })
        .optional()?;
    data.map(|data| {
        serde_json::from_str(&data).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
    })
    .transpose()
    .map(|p| p.unwrap_or_default())
}
fn settings(db: &Connection) -> rusqlite::Result<Value> {
    db.query_row(
        "SELECT enabled,visible_limit FROM family_settings WHERE id=1",
        [],
        |r| Ok(json!({"enabled":r.get::<_,bool>(0)?,"limit":r.get::<_,i64>(1)?,"recovery":recovery_policy(db)?})),
    )
}
pub fn enabled(db: &Connection) -> Result<bool, String> {
    settings(db)
        .map(|v| v["enabled"] == true)
        .map_err(|_| "Lineup unavailable".into())
}
fn channels(db: &Connection) -> rusqlite::Result<Vec<Value>> {
    let mut q = db.prepare("SELECT data FROM family_channels")?;
    let mut rows = q
        .query_map([], |r| r.get::<_, String>(0))?
        .map(|r| {
            serde_json::from_str(&r?).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })
        })
        .collect::<rusqlite::Result<Vec<Value>>>()?;
    rows.sort_by_key(|v| {
        (
            v["number"].as_u64().unwrap_or(0),
            v["id"].as_str().unwrap_or("").to_owned(),
        )
    });
    Ok(rows)
}
pub fn live(
    db: &Connection,
    category: Option<String>,
    search: Option<String>,
    offset: usize,
    limit: usize,
) -> Result<Value, String> {
    let category = category
        .filter(|s| !s.is_empty())
        .map(|s| s.strip_prefix("category:").unwrap_or(&s).to_owned());
    let search = search.unwrap_or_default().trim().to_lowercase();
    let rows = channels(db)
        .map_err(|_| "Lineup unavailable")?
        .into_iter()
        .filter(|v| {
            v["enabled"] == true
                && category
                    .as_ref()
                    .is_none_or(|c| v["category"].as_str() == Some(c))
                && v["name"]
                    .as_str()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&search)
        })
        .collect::<Vec<_>>();
    let total = rows.len();
    Ok(
        json!({"total":total,"channels":rows.into_iter().skip(offset).take(limit.min(500)).map(|mut c| { c.as_object_mut().unwrap().remove("candidates"); c["logo"] = json!(logo(db, c["id"].as_str().unwrap_or(""))); c }).collect::<Vec<_>>()}),
    )
}
fn logo(db: &Connection, channel: &str) -> Option<String> {
    let mut query = db.prepare("SELECT l.id,l.logo FROM family_candidates c JOIN provider_live l ON l.id=c.live_id WHERE c.channel_id=?1 AND length(trim(COALESCE(l.logo,'')))>0 ORDER BY c.rank LIMIT 20").ok()?;
    let rows = query
        .query_map([channel], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .ok()?;
    for (id, logo) in rows.filter_map(Result::ok) {
        if crate::live_policy::candidate_allowed(db, &id) {
            return Some(logo);
        }
    }
    None
}
pub fn categories(db: &Connection, offset: usize, limit: usize) -> Result<Value, String> {
    let mut groups = std::collections::BTreeMap::<String, usize>::new();
    for v in channels(db).map_err(|_| "Lineup unavailable")? {
        if v["enabled"] == true {
            *groups
                .entry(v["category"].as_str().unwrap_or("").to_owned())
                .or_default() += 1;
        }
    }
    Ok(
        json!({"total":groups.len(),"categories":groups.into_iter().skip(offset).take(limit.min(100)).map(|(name,count)|json!({"id":format!("category:{name}"),"name":name,"count":count})).collect::<Vec<_>>()}),
    )
}
pub fn family_id(db: &Connection, id: &str) -> Result<Option<String>, String> {
    if !id.starts_with("family:") && !enabled(db)? {
        return Ok(None);
    }
    let id = canonical_id(db, id).map_err(|_| "Lineup unavailable")?;
    Ok(id.starts_with("family:").then_some(id))
}
pub fn candidates(db: &Connection, id: &str) -> Result<Vec<String>, String> {
    let mut q=db.prepare("SELECT c.live_id FROM family_candidates c JOIN family_channels f ON f.id=c.channel_id WHERE c.channel_id=?1 AND json_extract(f.data,'$.enabled')=1 ORDER BY c.rank LIMIT 20").map_err(|_|"Lineup unavailable")?;
    let rows = q
        .query_map([id], |r| r.get(0))
        .map_err(|_| "Lineup unavailable")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|_| "Lineup unavailable".into());
    rows
}
pub fn eligible(db: &Connection, channel: &str, candidate: &str) -> Result<bool, String> {
    if !crate::live_policy::candidate_allowed(db, candidate)
        || !crate::health::eligible(db, candidate)?
    {
        return Ok(false);
    }
    db.query_row("SELECT EXISTS(SELECT 1 FROM family_candidates c JOIN family_channels f ON f.id=c.channel_id JOIN provider_live l ON l.id=c.live_id JOIN providers p ON p.id=l.provider_id WHERE c.channel_id=?1 AND c.live_id=?2 AND json_extract(f.data,'$.enabled')=1 AND p.enabled=1 AND p.enable_live=1 AND EXISTS(SELECT 1 FROM json_each(f.data,'$.candidates') saved WHERE json_extract(saved.value,'$.id')=l.id AND json_extract(saved.value,'$.name')=l.name))",params![channel,candidate],|r|r.get(0)).map_err(|_|"Lineup unavailable".into())
}
pub fn source(db: &Connection, id: &str) -> Result<String, String> {
    let Some(channel) = family_id(db, id)? else {
        return Ok(id.to_owned());
    };
    for candidate in candidates(db, &channel)? {
        if eligible(db, &channel, &candidate)? {
            return Ok(candidate);
        }
    }
    Err("Family channel has no available candidates".into())
}
pub fn record_startup(db: &Connection, id: &str, attempts: &[Value]) -> rusqlite::Result<()> {
    let data = json!({"at":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),"attempts":attempts});
    db.execute("INSERT INTO family_startups(channel_id,data) VALUES(?1,?2) ON CONFLICT(channel_id) DO UPDATE SET data=excluded.data",params![id,data.to_string()])?;
    Ok(())
}

pub(super) async fn list(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
) -> ApiResult {
    blocking(move || {
let db=a.db.lock().unwrap();
lease.validate(&db)?;
        Ok(axum::Json(json!({"settings":settings(&db).map_err(db_error)?,"channels":owner_channels(&db).map_err(db_error)?})))
    })
.await
}
#[derive(Deserialize)]
pub(super) struct CandidateQuery {
    #[serde(default)]
    search: String,
    #[serde(default)]
    offset: usize,
}
pub(super) async fn inventory(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Query(q): Query<CandidateQuery>,
) -> ApiResult {
    blocking(move || {
let db=a.db.lock().unwrap();
lease.validate(&db)?;
        let mut stmt=db.prepare("SELECT l.id,l.name,p.name,p.enabled AND p.enable_live FROM provider_live l JOIN providers p ON p.id=l.provider_id WHERE instr(lower(l.name),lower(?1))>0 ORDER BY l.name,l.id LIMIT 100 OFFSET ?2").map_err(db_error)?;
        let rows=stmt.query_map(params![q.search,q.offset.min(i64::MAX as usize) as i64],|r|Ok(json!({"id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"provider":r.get::<_,String>(2)?,"available":r.get::<_,bool>(3)?}))).map_err(db_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
        Ok(axum::Json(json!({"candidates":rows})))
    })
.await
}
pub(super) async fn configure(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    axum::Json(v): axum::Json<Value>,
) -> ApiResult {
    blocking(move || {
        let mut db = a.db.lock().unwrap();
        lease.validate(&db)?;
        let recovery = v.get("recovery").cloned().map(RecoveryPolicy::validated).transpose()?;
        let enabled = v["enabled"].as_bool().ok_or("Invalid enabled setting")?;
        let limit = v["limit"]
            .as_u64()
            .filter(|n| (1..=1000).contains(n))
            .ok_or("Channel limit must be between 1 and 1000")?;
        if channels(&db)
            .map_err(db_error)?
            .iter()
            .filter(|c| c["enabled"] == true)
            .count()
            > limit as usize
        {
            return Err("Disable channels before lowering the limit".into());
        }
        let tx = db.transaction().map_err(db_error)?;
        tx.execute(
            "UPDATE family_settings SET enabled=?1,visible_limit=?2 WHERE id=1",
            params![enabled, limit],
        )
        .map_err(db_error)?;
        if let Some(recovery) = recovery {
            tx.execute("INSERT INTO family_recovery(id,data) VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET data=excluded.data", [serde_json::to_string(&recovery).map_err(|_|ApiError::from("Invalid recovery settings"))?]).map_err(db_error)?;
        }
        tx.commit().map_err(db_error)?;
        Ok(axum::Json(settings(&db).map_err(db_error)?))
    })
    .await
}
pub(super) async fn create(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    axum::Json(v): axum::Json<Value>,
) -> ApiResult {
    save(
        a,
        lease,
        format!("family:{}", uuid::Uuid::new_v4()),
        v,
        false,
    )
    .await
}
pub(super) async fn update(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
    axum::Json(v): axum::Json<Value>,
) -> ApiResult {
    save(a, lease, id, v, true).await
}
async fn save(a: App, lease: ResourceLease, id: String, v: Value, existing: bool) -> ApiResult {
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

pub fn canonical_id(db: &Connection, id: &str) -> rusqlite::Result<String> {
    Ok(db
        .query_row(
            "SELECT channel_id FROM family_aliases WHERE live_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or_else(|| id.to_owned()))
}
/// Read-side aliases leave uncertain history and underlying legacy rows intact.
/// Input order is preserved so recent history keeps the newest occurrence.
pub fn references(db: &Connection, rows: Vec<Value>) -> rusqlite::Result<Vec<Value>> {
    let all = channels(db)?;
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    for mut row in rows {
        if row["type"] == "live" {
            let id = canonical_id(db, row["id"].as_str().unwrap_or(""))?;
            if let Some(channel) = all.iter().find(|c| c["id"] == id) {
                row["id"] = json!(id);
                row["name"] = channel["name"].clone();
            }
        }
        if seen.insert((row["type"].to_string(), row["id"].to_string())) {
            result.push(row);
        }
    }
    Ok(result)
}

fn owner_channels(db: &Connection) -> rusqlite::Result<Vec<Value>> {
    let mut rows = channels(db)?;
    let mut query=db.prepare("SELECT l.name,p.enabled AND p.enable_live FROM provider_live l JOIN providers p ON p.id=l.provider_id WHERE l.id=?1")?;
    for channel in &mut rows {
        let startup = db
            .query_row(
                "SELECT data FROM family_startups WHERE channel_id=?1",
                [channel["id"].as_str().unwrap_or("")],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        if let Some(startup) = startup {
            channel["last_startup"] = serde_json::from_str(&startup).unwrap_or(Value::Null);
        }

        if let Some(candidates) = channel["candidates"].as_array_mut() {
            for candidate in candidates {
                let current = query
                    .query_row([candidate["id"].as_str().unwrap_or("")], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?))
                    })
                    .optional()?;
                match current {
                    Some((name, enabled)) => {
                        candidate["status"] = json!(if !enabled {
                            "disabled"
                        } else if candidate["name"] != name {
                            "changed"
                        } else {
                            "available"
                        });
                        candidate["current_name"] = json!(name);
                    }
                    None => {
                        candidate["status"] = json!("missing");
                    }
                }
            }
        }
    }
    Ok(rows)
}

#[cfg(test)]
mod logo_tests {
    use super::*;
    #[test]
    fn family_artwork_follows_eligible_candidates_and_provider_refreshes() {
        let db = Connection::open_in_memory().unwrap();
        crate::provider::init(&db).unwrap();
        db.execute_batch("INSERT INTO providers(id,name,url,username,password) VALUES(1,'Test','http://fixture.invalid','u','p'); INSERT INTO provider_live(id,provider_id,stream_id,name,category,logo) VALUES('iptv:1:1',1,'1','USA: CNN','USA NEWS','https://images.invalid/first.png'),('iptv:1:2',1,'2','CNN','News','https://images.invalid/backup.png'); INSERT INTO family_channels(id,data) VALUES('family:test',json_object('id','family:test','name','CNN','enabled',json('true'),'number',1)); INSERT INTO family_candidates VALUES('family:test','iptv:1:1',0),('family:test','iptv:1:2',1);").unwrap();
        assert_eq!(
            live(&db, None, None, 0, 100).unwrap()["channels"][0]["logo"],
            "https://images.invalid/first.png"
        );
        db.execute("INSERT INTO live_category_rules VALUES('USA NEWS',0)", [])
            .unwrap();
        assert_eq!(
            logo(&db, "family:test").as_deref(),
            Some("https://images.invalid/backup.png")
        );
        db.execute("UPDATE provider_live SET logo='https://images.invalid/refreshed.png' WHERE stream_id='2'",[]).unwrap();
        assert_eq!(
            logo(&db, "family:test").as_deref(),
            Some("https://images.invalid/refreshed.png")
        );
        db.execute("UPDATE providers SET enabled=0", []).unwrap();
        assert!(logo(&db, "family:test").is_none());
    }
}
