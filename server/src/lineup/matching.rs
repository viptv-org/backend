//! Conservative, source-scoped matching. Desired corrections are independent of
//! catalog observations, and only existing family identities can gain candidates.
use super::*;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

#[derive(Clone, serde::Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Policy {
    confidence: u8,
    ambiguity_margin: u8,
    ambiguity_policy: String,
    active_candidates: usize,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            confidence: 95,
            ambiguity_margin: 10,
            ambiguity_policy: "review".into(),
            active_candidates: 4,
        }
    }
}
pub(super) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS family_matching_settings(id INTEGER PRIMARY KEY CHECK(id=1),data TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS family_match_overrides(channel_id TEXT NOT NULL,live_id TEXT NOT NULL,decision TEXT NOT NULL,observed_name TEXT NOT NULL,preference INTEGER NOT NULL DEFAULT 2147483647,PRIMARY KEY(channel_id,live_id));
        CREATE TABLE IF NOT EXISTS family_match_aliases(channel_id TEXT NOT NULL,alias TEXT NOT NULL,PRIMARY KEY(channel_id,alias));
        CREATE TABLE IF NOT EXISTS family_verified_ids(channel_id TEXT NOT NULL,provider_id INTEGER NOT NULL,source_key TEXT NOT NULL,epg_id TEXT NOT NULL,PRIMARY KEY(channel_id,provider_id,source_key,epg_id));
        CREATE TABLE IF NOT EXISTS family_provider_groups(provider_id INTEGER PRIMARY KEY,upstream_group TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS family_match_results(channel_id TEXT NOT NULL,live_id TEXT NOT NULL,data TEXT NOT NULL,status TEXT NOT NULL,PRIMARY KEY(channel_id,live_id));")?;
    if db.execute(
        "INSERT OR IGNORE INTO family_matching_settings(id,data) VALUES(1,?1)",
        [serde_json::to_string(&Policy::default()).unwrap()],
    )? == 1
    {
        db.execute("INSERT OR IGNORE INTO family_match_overrides(channel_id,live_id,decision,observed_name) SELECT f.id,json_extract(c.value,'$.id'),'pin',json_extract(c.value,'$.name') FROM family_channels f,json_each(f.data,'$.candidates') c",[])?;
        db.execute("UPDATE family_match_overrides SET preference=COALESCE((SELECT rank FROM family_candidates c WHERE c.channel_id=family_match_overrides.channel_id AND c.live_id=family_match_overrides.live_id),2147483647)",[])?;
        let rows=db.prepare("SELECT o.channel_id,o.live_id FROM family_match_overrides o JOIN provider_live l ON l.id=o.live_id AND l.name=o.observed_name WHERE o.decision='pin'")?.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        for (channel, live) in rows {
            record_verified(db, &channel, &live)?;
        }
    }
    db.execute_batch("CREATE TABLE IF NOT EXISTS family_matching_revision(id INTEGER PRIMARY KEY CHECK(id=1),value INTEGER NOT NULL); INSERT OR IGNORE INTO family_matching_revision VALUES(1,0);")?;
    for table in SNAPSHOT_TABLES
        .iter()
        .filter(|table| **table != "family_match_results")
    {
        for operation in ["INSERT", "UPDATE", "DELETE"] {
            db.execute_batch(&format!("CREATE TRIGGER IF NOT EXISTS family_revision_{table}_{operation} AFTER {operation} ON {table} BEGIN UPDATE family_matching_revision SET value=value+1 WHERE id=1; END;"))?;
        }
    }
    Ok(())
}
fn policy(db: &Connection) -> Result<Policy, ApiError> {
    let data: String = db
        .query_row(
            "SELECT data FROM family_matching_settings WHERE id=1",
            [],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    serde_json::from_str(&data).map_err(|_| "Matching settings unavailable".into())
}
fn owner(lease: &ResourceLease, db: &Connection) -> Result<(), ApiError> {
    crate::provider::accounts::owner(lease, db).map_err(ApiError::from)
}
pub(super) fn manual_saved(
    db: &Connection,
    channel: &str,
    old: Option<&Value>,
    saved: &[Value],
) -> rusqlite::Result<()> {
    if let Some(previous) = old.and_then(|v| v["candidates"].as_array()) {
        for candidate in previous {
            if !saved.iter().any(|v| v["id"] == candidate["id"]) {
                db.execute("INSERT INTO family_match_overrides(channel_id,live_id,decision,observed_name) VALUES(?1,?2,'reject',?3) ON CONFLICT(channel_id,live_id) DO UPDATE SET decision='reject'",params![channel,candidate["id"].as_str(),candidate["name"].as_str()])?;
            }
        }
    }
    for (preference, candidate) in saved.iter().enumerate() {
        db.execute("INSERT INTO family_match_overrides(channel_id,live_id,decision,observed_name) VALUES(?1,?2,'pin',?3) ON CONFLICT(channel_id,live_id) DO UPDATE SET decision='pin',observed_name=excluded.observed_name",params![channel,candidate["id"].as_str(),candidate["name"].as_str()])?;
        db.execute(
            "UPDATE family_match_overrides SET preference=?3 WHERE channel_id=?1 AND live_id=?2",
            params![channel, candidate["id"].as_str(), preference],
        )?;
        db.execute(
            "INSERT OR IGNORE INTO family_aliases(live_id,channel_id) VALUES(?1,?2)",
            params![candidate["id"].as_str(), channel],
        )?;
        record_verified(db, channel, candidate["id"].as_str().unwrap())?;
    }
    Ok(())
}
fn source_key(url: &str, user: &str) -> String {
    format!("{:x}", Sha256::digest(format!("{url}\n{user}")))
}
fn record_verified(db: &Connection, channel: &str, live: &str) -> rusqlite::Result<()> {
    let source=db.query_row("SELECT l.provider_id,l.epg_channel_id,p.url,p.username FROM provider_live l JOIN providers p ON p.id=l.provider_id WHERE l.id=?1 AND l.epg_channel_id IS NOT NULL AND trim(l.epg_channel_id)<>''",[live],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?))).optional()?;
    if let Some((provider, epg, url, user)) = source {
        db.execute("INSERT OR IGNORE INTO family_verified_ids(channel_id,provider_id,source_key,epg_id) VALUES(?1,?2,?3,?4)",params![channel,provider,source_key(&url,&user),epg])?;
    }
    Ok(())
}
fn words(value: &str) -> Vec<String> {
    value
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}
fn decorated(word: &str) -> bool {
    matches!(
        word,
        "hd" | "sd"
            | "fhd"
            | "uhd"
            | "4k"
            | "8k"
            | "1080p"
            | "720p"
            | "1080i"
            | "hevc"
            | "h264"
            | "h265"
            | "50fps"
            | "60fps"
    )
}
fn sibling_spelling(value: &str) -> String {
    value
        .replace("hbo 2", "hbo2")
        .replace("fs 1", "fs1")
        .replace("fs 2", "fs2")
}
fn normalized(value: &str) -> String {
    sibling_spelling(
        &words(value)
            .into_iter()
            .filter(|w| !decorated(w))
            .collect::<Vec<_>>()
            .join(" "),
    )
}
fn input_key(name: &str) -> String {
    sibling_spelling(
        &words(name)
            .into_iter()
            .filter(|w| {
                !decorated(w)
                    && !matches!(
                        w.as_str(),
                        "us" | "usa" | "en" | "eng" | "english" | "east" | "west"
                    )
            })
            .collect::<Vec<_>>()
            .join(" "),
    )
}
struct Input {
    id: String,
    provider: i64,
    name: String,
    category: String,
    epg: String,
    source: String,
    pool: i64,
    group: String,
}
struct Evidence {
    score: u8,
    reason: &'static str,
    safe: bool,
}
fn evidence(
    channel: &Value,
    input: &Input,
    aliases: &[String],
    verified_id: bool,
    pinned: bool,
) -> Option<Evidence> {
    let tokens = words(&input.name);
    let labels = words(&format!("{} {}", input.name, input.category));
    let has = |word: &str| labels.iter().any(|w| w == word);
    let east = has("east");
    let west = has("west");
    let market = words(channel["market"].as_str().unwrap_or(""));
    let stripped = tokens
        .iter()
        .filter(|w| {
            !decorated(w)
                && !matches!(
                    w.as_str(),
                    "us" | "usa" | "en" | "eng" | "english" | "east" | "west"
                )
                && !market.contains(w)
        })
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    let stripped = sibling_spelling(&stripped);
    let network = normalized(channel["network"].as_str().unwrap_or(""));
    let exact = stripped == network;
    let alias = aliases.iter().any(|a| a == &stripped);
    let sibling_group = |name: &str| match name {
        "hbo" | "hbo2" => 1,
        "fx" | "fxx" => 2,
        "fs1" | "fs2" => 3,
        _ => 0,
    };
    if stripped != network
        && sibling_group(&network) != 0
        && sibling_group(&network) == sibling_group(&stripped)
    {
        return Some(Evidence {
            score: 0,
            reason: "sibling_network_conflict",
            safe: false,
        });
    }
    // Short names and numbered siblings never use approximate matching.
    let fuzzy = !exact
        && !alias
        && network.len() >= 8
        && stripped.len() >= 8
        && network
            .chars()
            .filter(|c| c.is_numeric())
            .eq(stripped.chars().filter(|c| c.is_numeric()))
        && network.chars().take(4).eq(stripped.chars().take(4))
        && one_edit(&network, &stripped);
    if !pinned && !verified_id && !exact && !alias && !fuzzy {
        return None;
    }
    let foreign = [
        "ca",
        "canada",
        "uk",
        "gb",
        "au",
        "australia",
        "fr",
        "france",
        "de",
        "germany",
        "mx",
        "mexico",
    ]
    .iter()
    .any(|w| has(w));
    let other_language = [
        "es",
        "spa",
        "spanish",
        "espanol",
        "español",
        "français",
        "french",
        "deutsch",
        "german",
        "pt",
        "portuguese",
        "latino",
    ]
    .iter()
    .any(|w| has(w));
    let feed = channel["feed"].as_str().unwrap_or("");
    if foreign
        || other_language
        || (east && west)
        || (east && feed != "east")
        || (west && feed != "west")
    {
        return Some(Evidence {
            score: 0,
            reason: "region_language_or_feed_conflict",
            safe: false,
        });
    }
    if feed == "local" && !market.iter().all(|w| labels.contains(w)) && !pinned && !verified_id {
        return Some(Evidence {
            score: 0,
            reason: "local_market_unverified",
            safe: false,
        });
    }
    let identity_known = (has("us") || has("usa"))
        && (has("en") || has("eng") || has("english"))
        && match feed {
            "east" => east,
            "west" => west,
            "national" => true,
            "local" => market.iter().all(|w| labels.contains(w)),
            _ => false,
        };
    if !identity_known && !pinned && !verified_id {
        return Some(Evidence {
            score: 70,
            reason: "region_language_or_feed_unverified",
            safe: false,
        });
    }
    Some(if pinned {
        Evidence {
            score: 100,
            reason: "owner_pin",
            safe: true,
        }
    } else if verified_id {
        Evidence {
            score: 100,
            reason: "verified_source_id",
            safe: true,
        }
    } else if exact {
        Evidence {
            score: 98,
            reason: "exact_normalized_name",
            safe: true,
        }
    } else if alias {
        Evidence {
            score: 97,
            reason: "owner_alias",
            safe: true,
        }
    } else {
        Evidence {
            score: 90,
            reason: "constrained_name_similarity",
            safe: true,
        }
    })
}
fn one_edit(a: &str, b: &str) -> bool {
    let a = a.chars().collect::<Vec<_>>();
    let b = b.chars().collect::<Vec<_>>();
    if a.len().abs_diff(b.len()) > 1 {
        return false;
    }
    let (mut i, mut j, mut edits) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            i += 1;
            j += 1;
        } else {
            edits += 1;
            if edits > 1 {
                return false;
            }
            if a.len() >= b.len() {
                i += 1;
            }
            if b.len() >= a.len() {
                j += 1;
            }
        }
    }
    edits + (a.len() - i) + (b.len() - j) <= 1
}

pub(crate) fn reconcile(db: &mut Connection) -> Result<Value, ApiError> {
    let rules = policy(db)?;
    let channels = super::channels(db).map_err(db_error)?;
    let mut aliases: HashMap<String, Vec<String>> = HashMap::new();
    let mut ids = HashSet::new();
    let mut overrides = HashMap::new();
    {
        let mut q = db
            .prepare("SELECT channel_id,alias FROM family_match_aliases")
            .map_err(db_error)?;
        for row in q
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(db_error)?
        {
            let (c, a) = row.map_err(db_error)?;
            aliases.entry(c).or_default().push(a);
        }
    }
    {
        let mut q = db
            .prepare("SELECT channel_id,provider_id,source_key,epg_id FROM family_verified_ids")
            .map_err(db_error)?;
        for row in q
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .map_err(db_error)?
        {
            ids.insert(row.map_err(db_error)?);
        }
    }
    {
        let mut q = db
            .prepare("SELECT channel_id,live_id,decision,observed_name FROM family_match_overrides")
            .map_err(db_error)?;
        for row in q
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .map_err(db_error)?
        {
            let (c, id, d, n) = row.map_err(db_error)?;
            overrides.insert((c, id), (d, n));
        }
    }
    // Index exact names and constrained fuzzy prefixes once, not once per catalog row.
    let mut by_name: HashMap<String, HashSet<usize>> = HashMap::new();
    let mut by_prefix: HashMap<String, HashSet<usize>> = HashMap::new();
    let mut by_id: HashMap<(i64, String, String), HashSet<usize>> = HashMap::new();
    let mut by_override: HashMap<String, HashSet<usize>> = HashMap::new();
    let mut by_station: HashMap<String, HashSet<usize>> = HashMap::new();
    for (index, channel) in channels.iter().enumerate() {
        let id = channel["id"].as_str().unwrap();
        let network = normalized(channel["network"].as_str().unwrap());
        let market = if channel["feed"] == "local" {
            format!(" {}", normalized(channel["market"].as_str().unwrap()))
        } else {
            String::new()
        };
        by_name
            .entry(format!("{network}{market}"))
            .or_default()
            .insert(index);
        if channel["feed"] == "local" {
            by_station
                .entry(network.split_whitespace().next().unwrap_or("").to_owned())
                .or_default()
                .insert(index);
            for alias in aliases.get(id).into_iter().flatten() {
                by_station
                    .entry(alias.split_whitespace().next().unwrap_or("").to_owned())
                    .or_default()
                    .insert(index);
            }
        }
        for alias in aliases.get(id).into_iter().flatten() {
            by_name
                .entry(format!("{alias}{market}"))
                .or_default()
                .insert(index);
        }
        if network.len() >= 8 {
            by_prefix
                .entry(network.chars().take(4).collect())
                .or_default()
                .insert(index);
        }
        for (channel, provider, source, epg) in &ids {
            if channel == id {
                by_id
                    .entry((*provider, source.clone(), epg.clone()))
                    .or_default()
                    .insert(index);
            }
        }
        for (channel, live) in overrides.keys() {
            if channel == id {
                by_override.entry(live.clone()).or_default().insert(index);
            }
        }
    }
    // Bound each atomic pass rather than silently publishing a partial catalog.
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM provider_live", [], |r| r.get(0))
        .map_err(db_error)?;
    if count > 500_000 {
        return Err("Matching supports at most 500000 imported live entries per pass; narrow provider live imports".into());
    }
    let providers = db
        .prepare("SELECT id FROM providers ORDER BY id")
        .map_err(db_error)?
        .query_map([], |r| r.get::<_, i64>(0))
        .map_err(db_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error)?;
    for provider in providers {
        crate::provider::pools::ensure(db, provider)?;
    }
    let tx = db.transaction().map_err(db_error)?;
    tx.execute("UPDATE family_match_results SET status='missing'", [])
        .map_err(db_error)?;
    let mut q=tx.prepare("SELECT l.id,l.provider_id,l.name,COALESCE(l.category,''),COALESCE(l.epg_channel_id,''),m.pool_id,COALESCE(g.upstream_group,''),p.url,p.username FROM provider_live l JOIN providers p ON p.id=l.provider_id JOIN provider_pools m ON m.provider_id=p.id LEFT JOIN family_provider_groups g ON g.provider_id=p.id WHERE p.enabled=1 AND p.enable_live=1 ORDER BY l.id").map_err(db_error)?;
    let mut result_count = 0usize;
    let mut category_allowed = HashMap::new();
    for row in q
        .query_map([], |r| {
            Ok(Input {
                id: r.get(0)?,
                provider: r.get(1)?,
                name: r.get(2)?,
                category: r.get(3)?,
                epg: r.get(4)?,
                source: source_key(&r.get::<_, String>(7)?, &r.get::<_, String>(8)?),
                pool: r.get(5)?,
                group: r.get(6)?,
            })
        })
        .map_err(db_error)?
    {
        let input = row.map_err(db_error)?;
        let key = input_key(&input.name);
        let mut relevant = by_name.get(&key).cloned().unwrap_or_default();
        for token in key.split_whitespace() {
            if let Some(found) = by_station.get(token) {
                relevant.extend(found);
            }
        }
        if let Some(found) = by_prefix.get(&key.chars().take(4).collect::<String>()) {
            relevant.extend(found);
        }
        if let Some(found) = by_id.get(&(input.provider, input.source.clone(), input.epg.clone())) {
            relevant.extend(found);
        }
        if let Some(found) = by_override.get(&input.id) {
            relevant.extend(found);
        }
        if relevant.is_empty() {
            continue;
        }
        let allowed = *category_allowed
            .entry(input.category.clone())
            .or_insert_with(|| {
                crate::live_catalog::category_exclusion(&input.category).is_none()
                    && crate::live_policy::category_enabled(&tx, &input.category)
            });
        if !allowed || crate::live_catalog::exclusion(&input.name, "").is_some() {
            continue;
        }
        let mut relevant = relevant.into_iter().collect::<Vec<_>>();
        relevant.sort_unstable();
        let mut contenders = Vec::new();
        for index in relevant {
            let channel = &channels[index];
            let channel_id = channel["id"].as_str().unwrap();
            let decision = overrides.get(&(channel_id.to_owned(), input.id.clone()));
            let pinned = decision.is_some_and(|(d, n)| d == "pin" && n == &input.name);
            let verified = ids.contains(&(
                channel_id.to_owned(),
                input.provider,
                input.source.clone(),
                input.epg.clone(),
            )) && !input.epg.is_empty();
            if let Some(mut e) = evidence(
                channel,
                &input,
                aliases.get(channel_id).map_or(&[], Vec::as_slice),
                verified,
                pinned,
            )
            .or_else(|| {
                decision.map(|_| Evidence {
                    score: 70,
                    reason: "pinned_name_changed",
                    safe: false,
                })
            }) {
                if decision.is_some_and(|(d, _)| d == "reject") {
                    e = Evidence {
                        score: 0,
                        reason: "owner_rejection",
                        safe: false,
                    };
                } else if decision.is_some_and(|(d, n)| d == "pin" && n != &input.name) {
                    e = Evidence {
                        score: 70,
                        reason: "pinned_name_changed",
                        safe: false,
                    };
                }
                contenders.push((channel_id, e, pinned));
            }
        }
        contenders.sort_by(|a, b| {
            b.2.cmp(&a.2)
                .then_with(|| b.1.score.cmp(&a.1.score))
                .then_with(|| a.0.cmp(b.0))
        });
        let best = contenders.first().map_or(0, |c| c.1.score);
        let runner = contenders.get(1).map_or(0, |c| c.1.score);
        let clear = best.saturating_sub(runner) >= rules.ambiguity_margin
            && contenders.get(1).is_none_or(|c| c.1.score < best);
        let competing = contenders
            .iter()
            .take(3)
            .map(|(id, e, _)| json!({"channel_id":id,"score":e.score,"reason":e.reason}))
            .collect::<Vec<_>>();
        for (index, (channel, e, pinned)) in contenders.iter().enumerate() {
            result_count += 1;
            if result_count > 50_000 {
                return Err("Matching produced more than 50000 associations; narrow aliases or the selected lineup".into());
            }
            let legacy: Option<String> = tx
                .query_row(
                    "SELECT channel_id FROM family_aliases WHERE live_id=?1",
                    [&input.id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(db_error)?;
            let accepted = index == 0
                && e.safe
                && (clear || *pinned)
                && e.score >= rules.confidence
                && legacy.as_deref().is_none_or(|id| id == *channel);
            let status = if accepted {
                "accepted"
            } else if e.score == 0 || rules.ambiguity_policy == "reject" {
                "rejected"
            } else {
                "review"
            };
            let data = json!({"channel_id":channel,"candidate_id":input.id,"name":input.name,"provider_id":input.provider,"pool_id":input.pool,"upstream_group":input.group,"score":e.score,"reason":e.reason,"pinned":pinned,"runner_up_margin":best.saturating_sub(runner),"competing":competing,"seen_at":crate::util::now()});
            tx.execute("INSERT INTO family_match_results(channel_id,live_id,data,status) VALUES(?1,?2,?3,?4) ON CONFLICT(channel_id,live_id) DO UPDATE SET data=excluded.data,status=excluded.status",params![channel,input.id,data.to_string(),status]).map_err(db_error)?;
        }
    }
    drop(q);
    for mut channel in channels {
        let id = channel["id"].as_str().unwrap().to_owned();
        let mut q=tx.prepare("SELECT data FROM family_match_results WHERE channel_id=?1 AND status='accepted' ORDER BY json_extract(data,'$.pinned') DESC,COALESCE((SELECT preference FROM family_match_overrides o WHERE o.channel_id=family_match_results.channel_id AND o.live_id=family_match_results.live_id),2147483647),json_extract(data,'$.score') DESC,live_id").map_err(db_error)?;
        let mut available = q
            .query_map([&id], |r| r.get::<_, String>(0))
            .map_err(db_error)?
            .map(|r| serde_json::from_str::<Value>(&r?).map_err(|_| rusqlite::Error::InvalidQuery))
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db_error)?;
        drop(q);
        available.sort_by_key(|c| {
            !crate::health::eligible(&tx, c["candidate_id"].as_str().unwrap()).unwrap_or(false)
        });
        let mut pools = HashSet::new();
        let mut groups = HashSet::new();
        let mut selected = Vec::new();
        while !available.is_empty() && selected.len() < rules.active_candidates {
            let eligible_count = available
                .iter()
                .take_while(|c| {
                    crate::health::eligible(&tx, c["candidate_id"].as_str().unwrap())
                        .unwrap_or(false)
                })
                .count();
            let choices = if eligible_count > 0 {
                &available[..eligible_count]
            } else {
                &available[..]
            };
            let index = choices
                .iter()
                .position(|c| {
                    !pools.contains(&c["pool_id"].as_i64().unwrap())
                        && (c["upstream_group"] == ""
                            || !groups.contains(c["upstream_group"].as_str().unwrap()))
                })
                .or_else(|| {
                    choices
                        .iter()
                        .position(|c| !pools.contains(&c["pool_id"].as_i64().unwrap()))
                })
                .unwrap_or(0);
            let c = available.remove(index);
            pools.insert(c["pool_id"].as_i64().unwrap());
            groups.insert(c["upstream_group"].as_str().unwrap().to_owned());
            selected.push(c);
        }
        tx.execute("DELETE FROM family_candidates WHERE channel_id=?1", [&id])
            .map_err(db_error)?;
        let mut saved = Vec::new();
        for (rank, c) in selected.iter().enumerate() {
            let live = c["candidate_id"].as_str().unwrap();
            tx.execute(
                "INSERT INTO family_candidates(channel_id,live_id,rank) VALUES(?1,?2,?3)",
                params![id, live, rank],
            )
            .map_err(db_error)?;
            tx.execute("UPDATE family_match_results SET status='active' WHERE channel_id=?1 AND live_id=?2",params![id,live]).map_err(db_error)?;
            saved.push(json!({"id":live,"name":c["name"],"verified":true}));
        }
        tx.execute("UPDATE family_match_results SET status='reserve' WHERE channel_id=?1 AND status='accepted'",[&id]).map_err(db_error)?;
        channel["candidates"] = json!(saved);
        tx.execute(
            "UPDATE family_channels SET data=?2 WHERE id=?1",
            params![id, channel.to_string()],
        )
        .map_err(db_error)?;
    }
    tx.commit().map_err(db_error)?;
    summary(db, None, 0)
}

fn summary(db: &Connection, channel: Option<&str>, offset: usize) -> Result<Value, ApiError> {
    let rules = policy(db)?;
    let mut q=db.prepare("SELECT data,status FROM family_match_results WHERE (?1 IS NULL OR channel_id=?1) ORDER BY CASE status WHEN 'review' THEN 0 WHEN 'active' THEN 1 WHEN 'reserve' THEN 2 ELSE 3 END,live_id LIMIT 100 OFFSET ?2").map_err(db_error)?;
    let rows = q
        .query_map(params![channel, offset.min(500_000)], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(db_error)?
        .map(|row| {
            let (data, status) = row.map_err(db_error)?;
            let mut v: Value = serde_json::from_str(&data)
                .map_err(|_| ApiError::from("Matching result unavailable"))?;
            v["status"] = json!(status);
            Ok(v)
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    let mut counts = Vec::new();
    for c in super::channels(db).map_err(db_error)? {
        let id = c["id"].as_str().unwrap();
        let active: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM family_candidates WHERE channel_id=?1",
                [id],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        let reserves:i64=db.query_row("SELECT COUNT(*) FROM family_match_results WHERE channel_id=?1 AND status='reserve'",[id],|r|r.get(0)).map_err(db_error)?;
        let review: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM family_match_results WHERE channel_id=?1 AND status='review'",
                [id],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        counts.push(json!({"id":id,"name":c["name"],"active":active,"reserves":reserves,"review":review,"shortage":rules.active_candidates.saturating_sub(active as usize)}));
    }
    let mut q = db
        .prepare("SELECT alias FROM family_match_aliases WHERE channel_id=?1 ORDER BY alias")
        .map_err(db_error)?;
    let aliases = q
        .query_map([channel], |r| r.get::<_, String>(0))
        .map_err(db_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error)?;
    let mut q=db.prepare("SELECT p.id,p.name,COALESCE(g.upstream_group,'') FROM providers p LEFT JOIN family_provider_groups g ON p.id=g.provider_id ORDER BY p.id").map_err(db_error)?;
    let providers=q.query_map([],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"name":r.get::<_,String>(1)?,"upstream_group":r.get::<_,String>(2)?}))).map_err(db_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
    Ok(
        json!({"settings":rules,"channels":counts,"matches":rows,"aliases":aliases,"providers":providers}),
    )
}
#[derive(Deserialize)]
pub(crate) struct MatchQuery {
    channel_id: Option<String>,
    #[serde(default)]
    offset: usize,
}
pub(crate) async fn list(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Query(q): Query<MatchQuery>,
) -> ApiResult {
    blocking(move || {
        let db = a.db.lock().unwrap();
        owner(&lease, &db)?;
        summary(&db, q.channel_id.as_deref(), q.offset).map(axum::Json)
    })
    .await
}
pub(crate) async fn run(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
) -> ApiResult {
    blocking(move || {
        let db = a.db.lock().unwrap();
        owner(&lease, &db)?;
        drop(db);
        match_owned(&a, &lease).map(axum::Json)
    })
    .await
}
pub(crate) async fn configure(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    axum::Json(v): axum::Json<Value>,
) -> ApiResult {
    let rules: Policy =
        serde_json::from_value(v).map_err(|_| ApiError::from("Invalid matching settings"))?;
    if !(80..=100).contains(&rules.confidence)
        || rules.ambiguity_margin > 30
        || !(1..=20).contains(&rules.active_candidates)
        || !matches!(rules.ambiguity_policy.as_str(), "review" | "reject")
    {
        return Err("Matching requires confidence 80–100, margin 0–30, candidates 1–20 and review/reject ambiguity policy".into());
    }
    blocking(move || {
        let db = a.db.lock().unwrap();
        owner(&lease, &db)?;
        db.execute(
            "UPDATE family_matching_settings SET data=?1 WHERE id=1",
            [serde_json::to_string(&rules).unwrap()],
        )
        .map_err(db_error)?;
        summary(&db, None, 0).map(axum::Json)
    })
    .await
}
pub(crate) async fn group(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<i64>,
    axum::Json(v): axum::Json<Value>,
) -> ApiResult {
    let group = v["upstream_group"]
        .as_str()
        .filter(|s| s.len() <= 64)
        .ok_or("Upstream group must be at most 64 characters")?
        .trim()
        .to_owned();
    blocking(move||{let db=a.db.lock().unwrap();owner(&lease,&db)?;let exists:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM providers WHERE id=?1)",[id],|r|r.get(0)).map_err(db_error)?;if !exists{return Err("Provider not found".into());}db.execute("INSERT INTO family_provider_groups(provider_id,upstream_group) VALUES(?1,?2) ON CONFLICT(provider_id) DO UPDATE SET upstream_group=excluded.upstream_group",params![id,group]).map_err(db_error)?;Ok(axum::Json(json!({"saved":true})))}).await
}
pub(crate) async fn correct(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(channel): Path<String>,
    axum::Json(v): axum::Json<Value>,
) -> ApiResult {
    blocking(move||{
        let mut db=a.db.lock().unwrap();owner(&lease,&db)?;
        let current=super::channels(&db).map_err(db_error)?.into_iter().find(|c|c["id"]==channel).ok_or("Channel not found")?;
        let tx=db.transaction().map_err(db_error)?;
        if let Some(values)=v.get("aliases") {
            let aliases=values.as_array().filter(|v|v.len()<=20).ok_or("Use at most 20 literal aliases")?;
            let names=aliases.iter().map(|v|v.as_str().filter(|s|!s.trim().is_empty()&&s.len()<=128).map(normalized).ok_or_else(||ApiError::from("Aliases must be nonempty names up to 128 characters"))).collect::<Result<Vec<_>,_>>()?;
            tx.execute("DELETE FROM family_match_aliases WHERE channel_id=?1",[&channel]).map_err(db_error)?;
            for name in names {tx.execute("INSERT OR IGNORE INTO family_match_aliases(channel_id,alias) VALUES(?1,?2)",params![channel,name]).map_err(db_error)?;}
        }
        if let Some(id)=v.get("candidate_id") {
            let id=id.as_str().filter(|s|s.len()<=128).ok_or("Candidate ID required")?;
            let decision=v["decision"].as_str().filter(|s|matches!(*s,"pin"|"reject"|"clear")).ok_or("Choose pin, reject or clear")?;
            let name:Option<String>=tx.query_row("SELECT name FROM provider_live WHERE id=?1",[id],|r|r.get(0)).optional().map_err(db_error)?;
            if decision=="pin" {
                if v["verified"]!=true||name.as_deref()!=v["observed_name"].as_str()||name.is_none(){return Err("Reload and verify the current US English feed before pinning".into());}
                let name=name.as_deref().unwrap();
                let input=Input{id:id.into(),provider:0,name:name.into(),category:tx.query_row("SELECT COALESCE(category,'') FROM provider_live WHERE id=?1",[id],|r|r.get(0)).map_err(db_error)?,epg:String::new(),source:String::new(),pool:0,group:String::new()};
                if evidence(&current,&input,&[],false,true).is_none_or(|e|!e.safe){return Err("Candidate conflicts with this channel's country, language or feed".into());}
                let legacy:Option<String>=tx.query_row("SELECT channel_id FROM family_aliases WHERE live_id=?1",[id],|r|r.get(0)).optional().map_err(db_error)?;
                if legacy.is_some_and(|c|c!=channel){return Err("Candidate already belongs to another channel".into());}
                manual_saved(&tx,&channel,None,&[json!({"id":id,"name":name})]).map_err(db_error)?;
            }else if decision=="reject" {
                tx.execute("INSERT INTO family_match_overrides(channel_id,live_id,decision,observed_name) VALUES(?1,?2,'reject',?3) ON CONFLICT(channel_id,live_id) DO UPDATE SET decision='reject'",params![channel,id,name.unwrap_or_default()]).map_err(db_error)?;
                tx.execute("DELETE FROM family_candidates WHERE channel_id=?1 AND live_id=?2",params![channel,id]).map_err(db_error)?;
            }else{tx.execute("DELETE FROM family_match_overrides WHERE channel_id=?1 AND live_id=?2",params![channel,id]).map_err(db_error)?;}
        }
        tx.commit().map_err(db_error)?;
        drop(db);
        match_owned(&a,&lease)?;
        let db=a.db.lock().unwrap();
        summary(&db,Some(&channel),0).map(axum::Json)
    }).await
}

// A private shared-memory SQLite database keeps snapshot copying in SQLite,
// without copying profile/history/VOD tables or holding the application mutex
// during matching. No snapshot or credentials are written to the filesystem.
const SNAPSHOT_TABLES: &[&str] = &[
    "providers",
    "provider_live",
    "live_category_rules",
    "account_pools",
    "provider_pools",
    "family_channels",
    "family_candidates",
    "family_aliases",
    "family_matching_settings",
    "family_match_overrides",
    "family_match_aliases",
    "family_verified_ids",
    "family_provider_groups",
    "family_match_results",
    "candidate_health",
    "health_accounts",
];
struct Snapshot {
    db: Connection,
    uri: String,
    revision: i64,
}
fn snapshot(db: &Connection) -> Result<Snapshot, ApiError> {
    let (streams, channels): (i64, i64) = db
        .query_row(
            "SELECT (SELECT COUNT(*) FROM provider_live),(SELECT COUNT(*) FROM family_channels)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(db_error)?;
    if streams > 500_000 || channels > 1000 {
        return Err("Matching supports up to 500000 imported live entries and 1000 family identities per pass; narrow the selected imports".into());
    }

    let providers = db
        .prepare("SELECT id FROM providers")
        .map_err(db_error)?
        .query_map([], |r| r.get::<_, i64>(0))
        .map_err(db_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error)?;
    for id in providers {
        crate::provider::pools::ensure(db, id)?;
    }
    let uri = format!(
        "file:viptv-family-match-{}?mode=memory&cache=shared",
        uuid::Uuid::new_v4()
    );
    let copy = Connection::open_with_flags(
        &uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
            | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
            | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(db_error)?;
    db.execute("ATTACH DATABASE ?1 AS family_snapshot", [&uri])
        .map_err(db_error)?;
    let result = (|| {
        let tx = db.unchecked_transaction().map_err(db_error)?;
        let revision = tx
            .query_row(
                "SELECT value FROM family_matching_revision WHERE id=1",
                [],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        for table in SNAPSHOT_TABLES {
            let schema: String = tx
                .query_row(
                    "SELECT sql FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |r| r.get(0),
                )
                .map_err(db_error)?;
            let columns = schema
                .find('(')
                .ok_or("Matching snapshot schema unavailable")?;
            tx.execute_batch(&format!(
                "CREATE TABLE family_snapshot.{table} {}",
                &schema[columns..]
            ))
            .map_err(db_error)?;
        }
        for table in SNAPSHOT_TABLES {
            tx.execute_batch(&format!(
                "INSERT INTO family_snapshot.{table} SELECT * FROM main.{table}"
            ))
            .map_err(db_error)?;
        }
        tx.commit().map_err(db_error)?;
        Ok::<_, ApiError>(revision)
    })();
    let detached = db
        .execute_batch("DETACH DATABASE family_snapshot")
        .map_err(db_error);
    let revision = result?;
    detached?;
    Ok(Snapshot {
        db: copy,
        uri,
        revision,
    })
}
fn match_owned(a: &App, lease: &ResourceLease) -> Result<Value, ApiError> {
    match_authorized(&a.db, &a.family_matching_gate, |db| owner(lease, db))
}
pub(crate) fn match_catalog(
    db: &Mutex<Connection>,
    gate: &Mutex<()>,
    lease: &crate::automation::CatalogLease,
) -> Result<Value, ApiError> {
    match_authorized(db, gate, |db| lease.validate(db).map_err(ApiError::from))
}
pub(crate) fn match_health(
    a: &App,
    lease: &crate::automation::OwnerAccess,
) -> Result<Value, ApiError> {
    match_authorized(&a.db, &a.family_matching_gate, |db| lease.validate(db))
}
fn match_authorized(
    shared_db: &Mutex<Connection>,
    gate: &Mutex<()>,
    authorize: impl Fn(&Connection) -> Result<(), ApiError>,
) -> Result<Value, ApiError> {
    let _job = gate.try_lock().map_err(|_| {
        ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "Matching is already running; retry to apply the latest settings".into(),
        )
    })?;
    let mut staged = {
        let db = shared_db.lock().unwrap();
        authorize(&db)?;
        snapshot(&db)?
    };
    reconcile(&mut staged.db)?;
    let mut db = shared_db.lock().unwrap();
    authorize(&db)?;
    let revision: i64 = db
        .query_row(
            "SELECT value FROM family_matching_revision WHERE id=1",
            [],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    if revision != staged.revision {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Catalog or matching settings changed during this pass; run matching again".into(),
        ));
    }
    db.execute("ATTACH DATABASE ?1 AS family_snapshot", [&staged.uri])
        .map_err(db_error)?;
    let result = (|| {
        let tx = db.transaction().map_err(db_error)?;
        tx.execute_batch("DELETE FROM main.family_candidates; INSERT INTO main.family_candidates SELECT * FROM family_snapshot.family_candidates;
            DELETE FROM main.family_match_results; INSERT INTO main.family_match_results SELECT * FROM family_snapshot.family_match_results;
            UPDATE main.family_channels SET data=(SELECT data FROM family_snapshot.family_channels s WHERE s.id=family_channels.id);").map_err(db_error)?;
        authorize(&tx)?;
        tx.commit().map_err(db_error)
    })();
    let detached = db
        .execute_batch("DETACH DATABASE family_snapshot")
        .map_err(db_error);
    result?;
    detached?;
    summary(&db, None, 0)
}

pub(crate) fn compatible_guide(channel: &Value, name: &str, verified: bool) -> bool {
    let input = Input {
        id: String::new(),
        provider: 0,
        name: name.to_owned(),
        category: String::new(),
        epg: String::new(),
        source: String::new(),
        pool: 0,
        group: String::new(),
    };
    evidence(channel, &input, &[], false, verified)
        .is_some_and(|e| e.safe && (verified || e.score >= 95))
}
