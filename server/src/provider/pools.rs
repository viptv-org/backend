//! One reservation authority per upstream subscription. Semaphores have a fixed
//! physical size; configured admission limits never replace an in-flight gate.
use super::*;
use crate::{ApiError, ApiResult, ResourceLease};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Extension,
};
use rusqlite::OptionalExtension;
const GATE_SIZE: usize = 32;
const BUSY: &str = "Stop pool playback before regrouping or changing its allowance";
pub(super) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS account_pools(id INTEGER PRIMARY KEY AUTOINCREMENT,name TEXT NOT NULL,configured_limit INTEGER NOT NULL,external_reserve INTEGER NOT NULL DEFAULT 0);
    CREATE TABLE IF NOT EXISTS provider_pools(provider_id INTEGER PRIMARY KEY,pool_id INTEGER NOT NULL REFERENCES account_pools(id));
    CREATE TABLE IF NOT EXISTS account_observations(pool_id INTEGER PRIMARY KEY REFERENCES account_pools(id),reported_limit INTEGER,limit_at INTEGER,reported_usage INTEGER,usage_at INTEGER,external_estimate INTEGER);")
}
pub(crate) fn ensure(db: &Connection, id: i64) -> Result<i64, String> {
    if let Some(pool) = db
        .query_row(
            "SELECT pool_id FROM provider_pools WHERE provider_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?
    {
        return Ok(pool);
    }
    let (name, url, user): (String, String, String) = db
        .query_row(
            "SELECT name,url,username FROM providers WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(|_| "Provider not found")?;
    let identity = base_url(&url)?.to_string();
    let mut q=db.prepare("SELECT p.id,p.url,p.username,p.max_connections,m.pool_id FROM providers p LEFT JOIN provider_pools m ON m.provider_id=p.id ORDER BY p.id").map_err(db_error)?;
    let mut members = Vec::new();
    let mut existing = None;
    let mut limit = GATE_SIZE;
    for row in q
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, usize>(3)?,
                r.get::<_, Option<i64>>(4)?,
            ))
        })
        .map_err(db_error)?
    {
        let (peer, url, login, cap, pool) = row.map_err(db_error)?;
        if login == user && base_url(&url)?.as_str() == identity {
            members.push(peer);
            limit = limit.min(cap);
            existing = existing.or(pool);
        }
    }
    let pool = if let Some(pool) = existing {
        pool
    } else {
        db.execute(
            "INSERT INTO account_pools(name,configured_limit) VALUES(?1,?2)",
            params![name, limit],
        )
        .map_err(db_error)?;
        db.last_insert_rowid()
    };
    db.execute("UPDATE account_pools SET configured_limit=MIN(configured_limit,?2),external_reserve=MIN(external_reserve,?2) WHERE id=?1",params![pool,limit]).map_err(db_error)?;
    for member in members {
        db.execute(
            "INSERT OR IGNORE INTO provider_pools(provider_id,pool_id) VALUES(?1,?2)",
            params![member, pool],
        )
        .map_err(db_error)?;
    }
    db.execute("UPDATE providers SET max_connections=(SELECT configured_limit FROM account_pools WHERE id=?1) WHERE id IN(SELECT provider_id FROM provider_pools WHERE pool_id=?1)",[pool]).map_err(db_error)?;
    Ok(pool)
}
fn gate(gates: &mut HashMap<i64, PlaybackGate>, pool: i64) -> &mut PlaybackGate {
    gates.entry(pool).or_insert_with(|| PlaybackGate {
        issued: 0,
        report_generation: 0,
        semaphore: Arc::new(Semaphore::new(GATE_SIZE)),
    })
}
pub(super) fn active(gates: &HashMap<i64, PlaybackGate>, pool: i64) -> usize {
    gates.get(&pool).map_or(0, |g| {
        GATE_SIZE.saturating_sub(g.semaphore.available_permits())
    })
}
#[derive(Default)]
struct Observation {
    reported_limit: Option<usize>,
    limit_at: Option<i64>,
    reported_usage: Option<usize>,
    usage_at: Option<i64>,
    external: Option<usize>,
}
pub(super) fn snapshot(
    db: &Connection,
    gates: &HashMap<i64, PlaybackGate>,
    pool: i64,
) -> Result<Value, String> {
    let (name, limit, reserve): (String, usize, usize) = db
        .query_row(
            "SELECT name,configured_limit,external_reserve FROM account_pools WHERE id=?1",
            [pool],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(|_| "Account pool not found")?;
    let mut q = db
        .prepare("SELECT provider_id FROM provider_pools WHERE pool_id=?1 ORDER BY provider_id")
        .map_err(db_error)?;
    let members = q
        .query_map([pool], |r| r.get::<_, i64>(0))
        .map_err(db_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error)?;
    let local = active(gates, pool);
    let observed:Option<Observation>=db.query_row("SELECT reported_limit,limit_at,reported_usage,usage_at,external_estimate FROM account_observations WHERE pool_id=?1",[pool],|r|Ok(Observation{reported_limit:r.get(0)?,limit_at:r.get(1)?,reported_usage:r.get(2)?,usage_at:r.get(3)?,external:r.get(4)?})).optional().map_err(db_error)?;
    let Observation {
        reported_limit,
        limit_at,
        reported_usage,
        usage_at,
        external,
    } = observed.unwrap_or_default();
    let now = crate::util::now();
    let age = |at: Option<i64>| at.map(|at| now.saturating_sub(at).max(0));
    let fresh = |at: Option<i64>| at.is_some_and(|at| at <= now && now - at <= 60);
    // Retain the last valid ceiling and usage floor even after observations go stale.
    // Subtract known continuous local viewers from reported usage; never add both totals.
    let effective = reported_limit.map_or(limit, |reported| limit.min(reported));
    let accounted = (local + external.unwrap_or(0)).max(reported_usage.unwrap_or(0));
    let confidence = if reported_usage.is_none() {
        "unknown"
    } else if !fresh(usage_at) || (reported_limit.is_some() && !fresh(limit_at)) {
        "stale"
    } else {
        "estimated"
    };
    Ok(
        json!({"id":pool,"name":name,"provider_ids":members,"configured_limit":limit,"external_reserve":reserve,"local_reservations":local,"effective_limit":effective,"estimated_free":effective.saturating_sub(reserve).saturating_sub(local).min(effective.saturating_sub(accounted)),"reported_limit":reported_limit,"reported_usage":reported_usage,"limit_age_seconds":age(limit_at),"usage_age_seconds":age(usage_at),"confidence":confidence}),
    )
}
pub(super) fn acquire(
    db: &Connection,
    gates: &mut HashMap<i64, PlaybackGate>,
    provider: i64,
) -> Result<tokio::sync::OwnedSemaphorePermit, String> {
    let pool = ensure(db, provider)?;
    if snapshot(db, gates, pool)?["estimated_free"]
        .as_u64()
        .unwrap_or(0)
        == 0
    {
        return Err("Provider connection limit reached".into());
    }
    let gate = gate(gates, pool);
    let permit = gate
        .semaphore
        .clone()
        .try_acquire_owned()
        .map_err(|_| "Provider connection limit reached".to_owned())?;
    gate.issued = gate.issued.saturating_add(1);
    Ok(permit)
}
pub(super) fn set_limit(db: &Connection, pool: i64, limit: i64) -> Result<(), String> {
    let reserve: i64 = db
        .query_row(
            "SELECT external_reserve FROM account_pools WHERE id=?1",
            [pool],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    if limit < reserve {
        return Err("Allowance cannot be lower than the external reserve".into());
    }
    db.execute(
        "UPDATE account_pools SET configured_limit=?2 WHERE id=?1",
        params![pool, limit],
    )
    .map_err(db_error)?;
    db.execute("UPDATE providers SET max_connections=?2 WHERE id IN(SELECT provider_id FROM provider_pools WHERE pool_id=?1)",params![pool,limit]).map_err(db_error)?;
    Ok(())
}
fn error(message: String) -> ApiError {
    ApiError(
        if message == BUSY {
            StatusCode::CONFLICT
        } else {
            StatusCode::BAD_REQUEST
        },
        message,
    )
}
pub(crate) async fn list(
    State(a): State<crate::App>,
    Extension(lease): Extension<ResourceLease>,
) -> ApiResult {
    a.providers
        .blocking(move |s| {
            let db = s.lock()?;
            accounts::owner(&lease, &db)?;
            let ids = db
                .prepare("SELECT id FROM providers ORDER BY id")
                .map_err(db_error)?
                .query_map([], |r| r.get::<_, i64>(0))
                .map_err(db_error)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(db_error)?;
            let mut pools = std::collections::BTreeSet::new();
            for id in ids {
                pools.insert(ensure(&db, id)?);
            }
            let gates = s
                .playback_gates
                .lock()
                .map_err(|_| "Account limiter unavailable")?;
            let rows = pools
                .into_iter()
                .map(|id| snapshot(&db, &gates, id))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(json!({"pools":rows}))
        })
        .await
        .map(axum::Json)
        .map_err(error)
}
pub(crate) async fn configure(
    State(a): State<crate::App>,
    Extension(lease): Extension<ResourceLease>,
    Path(pool): Path<i64>,
    axum::Json(value): axum::Json<Value>,
) -> ApiResult {
    let limit = value["configured_limit"]
        .as_i64()
        .filter(|n| (1..=32).contains(n))
        .ok_or("Allowance must be between 1 and 32")?;
    let reserve = value["external_reserve"]
        .as_i64()
        .filter(|n| (0..=limit).contains(n))
        .ok_or("Reserve must be between zero and the allowance")?;
    a.providers.blocking(move |s| {
        let mut db=s.lock()?;accounts::owner(&lease,&db)?;
        let gates=s.playback_gates.lock().map_err(|_|"Account limiter unavailable")?;
        if active(&gates,pool)>0 {return Err(BUSY.into());}
        let tx=db.transaction().map_err(db_error)?;
        if tx.execute("UPDATE account_pools SET configured_limit=?2,external_reserve=?3 WHERE id=?1",params![pool,limit,reserve]).map_err(db_error)?==0 {return Err("Account pool not found".into());}
        tx.execute("UPDATE providers SET max_connections=?2 WHERE id IN(SELECT provider_id FROM provider_pools WHERE pool_id=?1)",params![pool,limit]).map_err(db_error)?;
        tx.commit().map_err(db_error)?;snapshot(&db,&gates,pool)
    }).await.map(axum::Json).map_err(error)
}
pub(crate) async fn assign(
    State(a): State<crate::App>,
    Extension(lease): Extension<ResourceLease>,
    Path(provider): Path<i64>,
    axum::Json(value): axum::Json<Value>,
) -> ApiResult {
    let target = match value.get("pool_id") {
        Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_i64()
                .filter(|n| *n > 0)
                .ok_or("Select an account pool")?,
        ),
        None => return Err("Select an account pool".into()),
    };
    a.providers.blocking(move |s| {
        let mut db = s.lock()?;
        accounts::owner(&lease, &db)?;
        let source = ensure(&db, provider)?;
        let mut gates = s.playback_gates.lock().map_err(|_| "Account limiter unavailable")?;
        if target == Some(source) { return snapshot(&db, &gates, source); }
        if active(&gates, source) > 0 || target.is_some_and(|id| active(&gates, id) > 0) { return Err(BUSY.into()); }
        let original = snapshot(&db, &gates, source)?;
        let dest = snapshot(&db, &gates, target.unwrap_or(source))?;
        let (name, url, user): (String, String, String) = db.query_row("SELECT name,url,username FROM providers WHERE id=?1", [provider], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(db_error)?;
        let identity = base_url(&url)?.to_string();
        let mut q = db.prepare("SELECT id,url,username FROM providers").map_err(db_error)?;
        let mut members = Vec::new();
        for row in q.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))).map_err(db_error)? {
            let (id, url, login) = row.map_err(db_error)?;
            if login == user && base_url(&url)?.as_str() == identity { members.push(id); }
        }
        drop(q);
        if target.is_none() && original["provider_ids"].as_array().is_some_and(|ids| ids.len() == members.len()) {
            return Ok(original);
        }
        let limit = original["configured_limit"].as_i64().unwrap().min(dest["configured_limit"].as_i64().unwrap());
        let reserve = original["external_reserve"].as_i64().unwrap().max(dest["external_reserve"].as_i64().unwrap()).min(limit);
        let tx = db.transaction().map_err(db_error)?;
        let target = if let Some(target) = target { target } else {
            tx.execute("INSERT INTO account_pools(name,configured_limit,external_reserve) VALUES(?1,?2,?3)", params![name, limit, reserve]).map_err(db_error)?;
            tx.last_insert_rowid()
        };
        // Regrouping cannot erase an observed ceiling or outside usage. Preserve
        // the original timestamps, so moving accounts never makes a report fresh.
        tx.execute("INSERT INTO account_observations(pool_id,reported_limit,limit_at,reported_usage,usage_at,external_estimate)
            SELECT ?2,reported_limit,limit_at,reported_usage,usage_at,external_estimate FROM account_observations WHERE pool_id=?1
            ON CONFLICT(pool_id) DO UPDATE SET
            limit_at=CASE WHEN account_observations.reported_limit IS NULL OR excluded.reported_limit<account_observations.reported_limit THEN excluded.limit_at WHEN excluded.reported_limit=account_observations.reported_limit THEN MIN(account_observations.limit_at,excluded.limit_at) ELSE account_observations.limit_at END,
            reported_limit=CASE WHEN account_observations.reported_limit IS NULL THEN excluded.reported_limit WHEN excluded.reported_limit IS NULL THEN account_observations.reported_limit ELSE MIN(account_observations.reported_limit,excluded.reported_limit) END,
            usage_at=CASE WHEN account_observations.reported_usage IS NULL OR excluded.reported_usage>account_observations.reported_usage THEN excluded.usage_at WHEN excluded.reported_usage=account_observations.reported_usage THEN MIN(account_observations.usage_at,excluded.usage_at) ELSE account_observations.usage_at END,
            reported_usage=CASE WHEN account_observations.reported_usage IS NULL THEN excluded.reported_usage WHEN excluded.reported_usage IS NULL THEN account_observations.reported_usage ELSE MAX(account_observations.reported_usage,excluded.reported_usage) END,
            external_estimate=CASE WHEN account_observations.external_estimate IS NULL THEN excluded.external_estimate WHEN excluded.external_estimate IS NULL THEN account_observations.external_estimate ELSE MAX(account_observations.external_estimate,excluded.external_estimate) END", params![source, target]).map_err(db_error)?;
        tx.execute("UPDATE account_pools SET configured_limit=?2,external_reserve=?3 WHERE id=?1", params![target,limit,reserve]).map_err(db_error)?;
        for id in members {
            tx.execute("INSERT INTO provider_pools(provider_id,pool_id) VALUES(?1,?2) ON CONFLICT(provider_id) DO UPDATE SET pool_id=excluded.pool_id", params![id,target]).map_err(db_error)?;
        }
        tx.execute("UPDATE providers SET max_connections=?2 WHERE id IN(SELECT provider_id FROM provider_pools WHERE pool_id=?1)", params![target,limit]).map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        for pool in [source, target] {
            let gate = gate(&mut gates, pool);
            gate.report_generation = gate.report_generation.saturating_add(1);
        }
        snapshot(&db, &gates, target)
    }).await.map(axum::Json).map_err(error)
}

fn reported(value: &Value, allow_zero: bool) -> Option<usize> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|v| v.parse().ok()))
        .filter(|n| *n <= 1_000_000 && (allow_zero || *n > 0))
        .map(|n| n as usize)
}
pub(crate) async fn refresh(
    State(a): State<crate::App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<i64>,
) -> ApiResult {
    let auth = lease.clone();
    let (provider, pool, issued, report_generation) = a
        .providers
        .blocking(move |s| {
            let db = s.lock()?;
            accounts::owner(&auth, &db)?;
            let pool = ensure(&db, id)?;
            let provider = db
                .query_row(
                    "SELECT id,name,url,username,password FROM providers WHERE id=?1 AND enabled=1",
                    [id],
                    |r| {
                        Ok(Provider {
                            id: r.get(0)?,
                            name: r.get(1)?,
                            url: r.get(2)?,
                            username: r.get(3)?,
                            password: r.get(4)?,
                        })
                    },
                )
                .map_err(|_| "Provider not found or disabled")?;
            let mut gates = s
                .playback_gates
                .lock()
                .map_err(|_| "Account limiter unavailable")?;
            let gate = gate(&mut gates, pool);
            gate.report_generation = gate.report_generation.saturating_add(1);
            Ok((provider, pool, gate.issued, gate.report_generation))
        })
        .await?;
    let warp = super::egress::enabled(&a.db.lock().unwrap(), provider.id);
    let report = accounts::login_report(
        &a.providers,
        &json!({"url":provider.url,"username":provider.username,"password":provider.password,"warp":warp}),
    )
    .await
    .map_err(|_| {
        ApiError::from("Account report unavailable; the previous observation was retained")
    })?;
    a.providers.blocking(move |s| {
        let db=s.lock()?;accounts::owner(&lease,&db)?;provider.ensure_current(&db)?;
        if ensure(&db,id)?!=pool {return Err("Account pool changed; refresh its report again".into());}
        let gates=s.playback_gates.lock().map_err(|_|"Account limiter unavailable")?;
        if gates.get(&pool).is_none_or(|g|g.report_generation!=report_generation) {return Err("A newer account report superseded this request".into());}
        let local=active(&gates,pool);let new_admissions=gates.get(&pool).map_or(0,|g|g.issued.saturating_sub(issued));
        let continuous=local.saturating_sub(usize::try_from(new_admissions).unwrap_or(usize::MAX));
        let limit=reported(&report["max_connections"],false);let usage=reported(&report["active_cons"],true);let now=crate::util::now();
        db.execute("INSERT INTO account_observations(pool_id,reported_limit,limit_at,reported_usage,usage_at,external_estimate) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(pool_id) DO UPDATE SET reported_limit=COALESCE(excluded.reported_limit,account_observations.reported_limit),limit_at=COALESCE(excluded.limit_at,account_observations.limit_at),reported_usage=COALESCE(excluded.reported_usage,account_observations.reported_usage),usage_at=COALESCE(excluded.usage_at,account_observations.usage_at),external_estimate=COALESCE(excluded.external_estimate,account_observations.external_estimate)",params![pool,limit,limit.map(|_|now),usage,usage.map(|_|now),usage.map(|n|n.saturating_sub(continuous))]).map_err(db_error)?;
        snapshot(&db,&gates,pool)
    }).await.map(axum::Json).map_err(error)
}

impl ProviderService {
    pub(crate) fn service_health(
        &self,
        lease: &ResourceLease,
        offset: u32,
    ) -> Result<Value, String> {
        let db = self.lock()?;
        accounts::owner(lease, &db)?;
        let mut result = crate::service_health::saved(&db)?;
        let total: i64 = db
            .query_row("SELECT count(*) FROM providers", [], |r| r.get(0))
            .map_err(db_error)?;
        let mut query = db.prepare(
            "SELECT p.id,p.name,p.enabled,m.pool_id,
             (SELECT max(COALESCE(r.finished_at,r.started_at)) FROM catalog_results c JOIN catalog_runs r ON r.id=c.run_id WHERE c.provider_id=p.id AND c.status='completed'),
             (SELECT c.status FROM catalog_results c JOIN catalog_runs r ON r.id=c.run_id WHERE c.provider_id=p.id ORDER BY r.created_at DESC,r.rowid DESC LIMIT 1),
             (SELECT until FROM catalog_backoff b WHERE b.provider_id=p.id AND b.until>?1)
             FROM providers p LEFT JOIN provider_pools m ON m.provider_id=p.id ORDER BY p.id LIMIT 20 OFFSET ?2"
        ).map_err(db_error)?;
        let rows = query.query_map(params![crate::util::now(),offset], |r| Ok((
            json!({"id":r.get::<_,i64>(0)?,"name":r.get::<_,String>(1)?,"enabled":r.get::<_,bool>(2)?,"last_catalog_at":r.get::<_,Option<i64>>(4)?,"catalog_state":r.get::<_,Option<String>>(5)?.unwrap_or_else(||"not_observed".into()),"retry_at":r.get::<_,Option<i64>>(6)?}),r.get::<_,Option<i64>>(3)?
        ))).map_err(db_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
        let gates = self
            .playback_gates
            .lock()
            .map_err(|_| "Account limiter unavailable")?;
        let mut items = Vec::with_capacity(rows.len());
        for (mut row, pool) in rows {
            // Never synthesize a new pool or probe credentials just to render status.
            row["pool"] = match pool {
                Some(id) => {
                    let observed = snapshot(&db, &gates, id)?;
                    json!({"id":id,"estimated_free":observed["estimated_free"],"effective_limit":observed["effective_limit"],"local_reservations":observed["local_reservations"],"confidence":observed["confidence"]})
                }
                None => Value::Null,
            };
            items.push(row);
        }
        result["providers"] = json!({"total":total,"items":items,"next_offset":(i64::from(offset)+20<total).then_some(u64::from(offset)+20)});
        Ok(result)
    }
}
