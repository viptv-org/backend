//! Desired exclusions and observed media health are separate durable records.
use super::*;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Policy {
    enabled: bool,
    interval_minutes: u64,
    sample_seconds: u64,
    concurrency: usize,
    startup_seconds: u64,
    budget_seconds: u64,
    max_sample_mib: usize,
    retry_minutes: [u64; 4],
    reserve_multiplier: u64,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_minutes: 360,
            sample_seconds: 8,
            concurrency: 2,
            startup_seconds: 10,
            budget_seconds: 30,
            max_sample_mib: 32,
            retry_minutes: [5, 15, 60, 360],
            reserve_multiplier: 2,
        }
    }
}
#[derive(Default)]
pub(crate) struct Control {
    jobs: Mutex<HashMap<String, Arc<Job>>>,
    started: AtomicBool,
}
struct Job {
    cancel: AtomicBool,
    finished: AtomicBool,
    pool: i64,
}
pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS candidate_health(live_id TEXT PRIMARY KEY,disabled INTEGER NOT NULL DEFAULT 0,excluded_until INTEGER NOT NULL DEFAULT 0,source_key TEXT,at INTEGER,state TEXT NOT NULL DEFAULT 'unknown',reason TEXT,next_check INTEGER NOT NULL DEFAULT 0,failures INTEGER NOT NULL DEFAULT 0,data TEXT,checking INTEGER NOT NULL DEFAULT 0);
    CREATE TABLE IF NOT EXISTS health_accounts(provider_id INTEGER PRIMARY KEY,until INTEGER NOT NULL,reason TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS health_settings(id INTEGER PRIMARY KEY CHECK(id=1),data TEXT NOT NULL,owner_id INTEGER);
    UPDATE candidate_health SET checking=0,reason='interrupted_by_restart',next_check=0 WHERE checking=1;")?;
    db.execute(
        "INSERT OR IGNORE INTO health_settings(id,data) VALUES(1,?1)",
        [serde_json::to_string(&Policy::default()).unwrap()],
    )?;
    Ok(())
}
fn policy(db: &Connection) -> Result<Policy, ApiError> {
    let raw: String = db
        .query_row("SELECT data FROM health_settings WHERE id=1", [], |r| {
            r.get(0)
        })
        .map_err(db_error)?;
    serde_json::from_str(&raw).map_err(|_| "Invalid health settings".into())
}
pub(crate) fn fingerprint(db: &Connection, id: &str) -> Result<String, String> {
    let raw:String=db.query_row("SELECT json_array(p.url,p.username,p.password,l.stream_id,l.name,l.category,l.epg_channel_id,COALESCE((SELECT warp FROM provider_routes WHERE provider_id=p.id),0)) FROM provider_live l JOIN providers p ON p.id=l.provider_id WHERE l.id=?1",[id],|r|r.get(0)).map_err(|_|"Candidate unavailable")?;
    Ok(format!("{:x}", Sha256::digest(raw)))
}
pub(crate) fn eligible(db: &Connection, id: &str) -> Result<bool, String> {
    let row=db.query_row("SELECT disabled,excluded_until,source_key,state,next_check FROM candidate_health WHERE live_id=?1",[id],|r|Ok((r.get::<_,bool>(0)?,r.get::<_,i64>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,String>(3)?,r.get::<_,i64>(4)?))).optional().map_err(|_|"Health state unavailable")?;
    if let Some((disabled, until, source, state, next)) = row {
        if disabled || until > util::now() {
            return Ok(false);
        }
        if state == "cooling_down"
            && next > util::now()
            && source.as_deref() == fingerprint(db, id).ok().as_deref()
        {
            return Ok(false);
        }
    }
    let blocked:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM provider_live l JOIN health_accounts h ON h.provider_id=l.provider_id WHERE l.id=?1 AND h.until>?2)",params![id,util::now()],|r|r.get(0)).map_err(|_|"Health state unavailable")?;
    Ok(!blocked)
}
fn summary(db: &Connection) -> Result<Value, ApiError> {
    let mut q=db.prepare("SELECT DISTINCT l.id,l.name,p.name,COALESCE(h.disabled,0),COALESCE(h.excluded_until,0),h.at,COALESCE(h.state,'unknown'),h.reason,COALESCE(h.next_check,0),h.data,h.source_key,(SELECT json_group_array(channel_id) FROM (SELECT channel_id FROM family_candidates WHERE live_id=l.id UNION SELECT channel_id FROM family_match_results WHERE live_id=l.id AND status='reserve')) FROM provider_live l JOIN providers p ON p.id=l.provider_id LEFT JOIN candidate_health h ON h.live_id=l.id WHERE l.id IN (SELECT live_id FROM family_candidates UNION SELECT live_id FROM family_match_results WHERE status='reserve') ORDER BY l.name,l.id LIMIT 2000").map_err(db_error)?;
    let rows=q.query_map([],|r|Ok((r.get::<_,String>(0)?,json!({"id":r.get::<_,String>(0)?,"channel_ids":serde_json::from_str::<Value>(&r.get::<_,String>(11)?).unwrap_or(json!([])),"name":r.get::<_,String>(1)?,"provider":r.get::<_,String>(2)?,"disabled":r.get::<_,bool>(3)?,"excluded_until":r.get::<_,i64>(4)?,"at":r.get::<_,Option<i64>>(5)?,"state":r.get::<_,String>(6)?,"reason":r.get::<_,Option<String>>(7)?,"next_check":r.get::<_,i64>(8)?,"sample":r.get::<_,Option<String>>(9)?.and_then(|s|serde_json::from_str::<Value>(&s).ok())}),r.get::<_,Option<String>>(10)?))).map_err(db_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
    let candidates = rows
        .into_iter()
        .map(|(id, mut row, source)| {
            if row["disabled"] == true {
                row["state"] = json!("disabled");
            } else if row["excluded_until"].as_i64().unwrap_or(0) > util::now() {
                row["state"] = json!("cooling_down");
                row["reason"] = json!("owner_exclusion");
            } else if source.as_deref() != fingerprint(db, &id).ok().as_deref() {
                row["state"] = json!("unknown");
                row["sample"] = Value::Null;
            }
            row
        })
        .collect::<Vec<_>>();
    Ok(json!({"settings":policy(db)?,"candidates":candidates}))
}
pub(crate) async fn list(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
) -> ApiResult {
    blocking(move || {
        let db = a.db.lock().unwrap();
        provider::accounts::owner(&lease, &db)?;
        {
            let mut value = summary(&db)?;
            value["checking"] = json!(a
                .health_control
                .jobs
                .lock()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>());
            Ok(axum::Json(value))
        }
    })
    .await
}
pub(crate) async fn configure(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    axum::Json(value): axum::Json<Value>,
) -> ApiResult {
    let rules: Policy = serde_json::from_value(value).map_err(|_| "Invalid health settings")?;
    if !(15..=10080).contains(&rules.interval_minutes)
        || !(4..=15).contains(&rules.sample_seconds)
        || !(1..=2).contains(&rules.concurrency)
        || !(3..=20).contains(&rules.startup_seconds)
        || !(15..=60).contains(&rules.budget_seconds)
        || rules.budget_seconds < rules.sample_seconds + rules.startup_seconds
        || !(1..=32).contains(&rules.max_sample_mib)
        || rules.retry_minutes.iter().any(|v| !(1..=10080).contains(v))
        || rules.retry_minutes.windows(2).any(|v| v[1] < v[0])
        || !(1..=8).contains(&rules.reserve_multiplier)
    {
        return Err(
            "Invalid check bounds: interval 15–10080 minutes, sample 4–15s, startup 3–20s, total 15–60s covering startup plus sample, bytes 1–32 MiB, concurrency 1–2, increasing retries 1–10080 minutes, reserve multiplier 1–8".into(),
        );
    }
    blocking(move || {
        let db = a.db.lock().unwrap();
        provider::accounts::owner(&lease, &db)?;
        let before = crate::activity::snapshot(&db, "health_settings", "")?;
        if !rules.enabled {
            pause(&a);
        }
        let auth::Principal::Account { account_id, .. } = lease.principal;
        db.execute(
            "UPDATE health_settings SET data=?1,owner_id=?2 WHERE id=1",
            params![serde_json::to_string(&rules).unwrap(), account_id],
        )
        .map_err(db_error)?;
        crate::activity::record(&db, "health_settings", "", before)?;
        summary(&db).map(axum::Json)
    })
    .await
}
pub(crate) async fn override_candidate(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
    axum::Json(value): axum::Json<Value>,
) -> ApiResult {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Change {
        disabled: bool,
        exclude_minutes: u64,
    }
    let change: Change =
        serde_json::from_value(value).map_err(|_| "Set disabled and exclusion minutes")?;
    if change.exclude_minutes > 10080 {
        return Err("Exclusion is limited to seven days".into());
    }
    blocking(move||{let db=a.db.lock().unwrap();provider::accounts::owner(&lease,&db)?;fingerprint(&db,&id)?;let before=crate::activity::snapshot(&db,"candidate_exclusion",&id)?;db.execute("INSERT INTO candidate_health(live_id,disabled,excluded_until) VALUES(?1,?2,?3) ON CONFLICT(live_id) DO UPDATE SET disabled=excluded.disabled,excluded_until=excluded.excluded_until",params![id,change.disabled,if change.exclude_minutes==0{0}else{util::now()+change.exclude_minutes as i64*60}]).map_err(db_error)?;crate::activity::record(&db,"candidate_exclusion",&id,before)?;summary(&db).map(axum::Json)}).await
}
pub(crate) async fn check(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
) -> Result<(StatusCode, axum::Json<Value>), ApiError> {
    provider::accounts::owner(&lease, &a.db.lock().unwrap())?;
    begin(a, crate::automation::OwnerAccess::request(lease), id).await?;
    Ok((StatusCode::ACCEPTED, axum::Json(json!({"accepted":true}))))
}
async fn begin(a: App, lease: crate::automation::OwnerAccess, id: String) -> Result<(), ApiError> {
    let (pool, source, rules, account) = {
        let db = a.db.lock().unwrap();
        lease.validate(&db)?;
        let account: i64 = db
            .query_row(
                "SELECT provider_id FROM provider_live WHERE id=?1",
                [&id],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        (
            provider::pools::ensure(&db, account)?,
            fingerprint(&db, &id)?,
            policy(&db)?,
            account,
        )
    };
    let job = Arc::new(Job {
        cancel: AtomicBool::new(false),
        finished: AtomicBool::new(false),
        pool,
    });
    {
        let mut jobs = a.health_control.jobs.lock().unwrap();
        if jobs.contains_key(&id)
            || jobs.len() >= rules.concurrency
            || jobs.values().any(|job| job.pool == pool)
        {
            return Err(ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "Health check deferred; probe capacity is busy".into(),
            ));
        }
        jobs.insert(id.clone(), job.clone());
    }
    {
        let db = a.db.lock().unwrap();
        if let Err(error)=db.execute("INSERT INTO candidate_health(live_id,checking,next_check) VALUES(?1,1,?2) ON CONFLICT(live_id) DO UPDATE SET checking=1,next_check=excluded.next_check",params![id,util::now()+60]) {a.health_control.jobs.lock().unwrap().remove(&id);job.finished.store(true,Ordering::Release);return Err(db_error(error));}
    }
    tokio::spawn(async move {
        let work = async {
            if let Some(engine) = session::shared::active_candidate(&a, &id) {
                let before = a.playback.live_progress(&engine).await;
                tokio::time::sleep(Duration::from_secs(rules.sample_seconds)).await;
                if a.playback.input_running(&engine).await
                    && before.is_some()
                    && a.playback.live_progress(&engine).await != before
                {
                    return Ok(
                        json!({"state":"healthy","reason":"active_playback_progress","sample_seconds":rules.sample_seconds}),
                    );
                }
                return Err("deferred_active_playback".to_owned());
            }

            let blocked: bool = {
                let db = a.db.lock().unwrap();
                db.query_row("SELECT EXISTS(SELECT 1 FROM health_accounts WHERE provider_id=?1 AND until>?2)",params![account,util::now()],|r|r.get(0)).unwrap_or(true)
            };
            if blocked {
                return Err("account_backoff".to_owned());
            }
            let candidate = id.clone();
            let (url, _) = a
                .providers
                .blocking(move |p| p.probe_source(&candidate))
                .await
                .map_err(|_| "candidate_unavailable")?;
            let permit = a
                .providers
                .acquire_playback_for_kind(account, "live")
                .await
                .map_err(|_| "deferred_capacity")?;
            let proxy = crate::provider::egress::proxy(&a.db.lock().unwrap(), account)?;
            a.playback
                .sample_media(
                    url,
                    permit,
                    proxy,
                    crate::playback::SampleLimits {
                        seconds: rules.sample_seconds,
                        startup_seconds: rules.startup_seconds,
                        budget_seconds: rules.budget_seconds,
                        max_bytes: rules.max_sample_mib * 1024 * 1024,
                    },
                )
                .await
        };
        let cancel = async {
            loop {
                if job.cancel.load(Ordering::Acquire)
                    || a.playback.is_shutting_down()
                    || lease.validate(&a.db.lock().unwrap()).is_err()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        };
        let result =
            tokio::select! {value=work=>value,_=cancel=>Err("deferred_for_viewer".to_owned())};
        a.playback.settle_cancelled_inputs().await;
        {
            let db = a.db.lock().unwrap();
            if lease.validate(&db).is_ok() && fingerprint(&db, &id).ok().as_deref() == Some(&source)
            {
                let old: i64 = db
                    .query_row(
                        "SELECT failures FROM candidate_health WHERE live_id=?1",
                        [&id],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);
                let deferred = result
                    .as_ref()
                    .err()
                    .is_some_and(|e| e.starts_with("deferred") || e == "account_backoff");
                let failures = if result.is_ok() {
                    0
                } else if deferred {
                    old
                } else {
                    old.saturating_add(1)
                };
                let delay = if result.is_ok() {
                    rules.interval_minutes as i64
                        * 60
                        * if db
                            .query_row(
                                "SELECT EXISTS(SELECT 1 FROM family_candidates WHERE live_id=?1)",
                                [&id],
                                |r| r.get::<_, bool>(0),
                            )
                            .unwrap_or(false)
                        {
                            1
                        } else {
                            rules.reserve_multiplier as i64
                        }
                } else if deferred {
                    60
                } else {
                    rules.retry_minutes[failures.saturating_sub(1).min(3) as usize] as i64 * 60
                };
                let jitter = if deferred {
                    0
                } else {
                    (Sha256::digest(id.as_bytes())[0] as i64) % 31
                };
                let state = if deferred {
                    "unknown"
                } else if let Ok(value) = &result {
                    value["state"].as_str().unwrap_or("healthy")
                } else {
                    "cooling_down"
                };
                let reason = result
                    .as_ref()
                    .map(|v| v["reason"].as_str().unwrap_or("decoded_advancing_media"))
                    .unwrap_or_else(|e| e.as_str());
                if deferred {
                    let _=db.execute("UPDATE candidate_health SET checking=0,reason=?2,next_check=?3 WHERE live_id=?1",params![id,reason,util::now()+delay]);
                } else {
                    let _=db.execute("UPDATE candidate_health SET checking=0,source_key=?2,at=?3,state=?4,reason=?5,next_check=?6,failures=?7,data=?8 WHERE live_id=?1",params![id,source,util::now(),state,reason,util::now()+delay+jitter,failures,result.as_ref().ok().map(Value::to_string)]);
                }
                if reason == "authentication_failed" || reason == "rate_limited" {
                    let _=db.execute("INSERT INTO health_accounts(provider_id,until,reason) VALUES(?1,?2,?3) ON CONFLICT(provider_id) DO UPDATE SET until=excluded.until,reason=excluded.reason",params![account,util::now()+if reason=="authentication_failed"{3600}else{900},reason]);
                }
            }
        }
        let _ = a.db.lock().unwrap().execute(
            "UPDATE candidate_health SET checking=0 WHERE live_id=?1",
            [&id],
        );
        a.health_control.jobs.lock().unwrap().remove(&id);
        job.finished.store(true, Ordering::Release);
        if result.is_ok() || result.as_ref().is_err_and(|e| !e.starts_with("deferred")) {
            let _ = tokio::task::spawn_blocking(move || {
                crate::lineup::matching::match_health(&a, &lease)
            })
            .await;
        }
    });
    Ok(())
}
pub(crate) async fn preempt(a: &App) {
    let jobs = a
        .health_control
        .jobs
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect::<Vec<_>>();
    for job in &jobs {
        job.cancel.store(true, Ordering::Release);
    }
    for job in jobs {
        while !job.finished.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}
pub(crate) fn start(a: &App) {
    if a.health_control.started.swap(true, Ordering::AcqRel) {
        return;
    }
    let mut a = a.clone();
    let life = Arc::downgrade(&a.automation_life);
    a.automation_life = Arc::new(());
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            if life.upgrade().is_none() || a.playback.is_shutting_down() {
                break;
            }
            let next = {
                let db = a.db.lock().unwrap();
                let Ok(rules) = policy(&db) else {
                    continue;
                };
                if crate::activity::paused(&db) || !rules.enabled {
                    continue;
                }
                let owner: Option<i64> = db
                    .query_row("SELECT owner_id FROM health_settings WHERE id=1", [], |r| {
                        r.get(0)
                    })
                    .ok()
                    .flatten();
                let candidate:Option<String>=db.query_row("SELECT c.live_id FROM (SELECT channel_id,live_id FROM family_candidates UNION SELECT channel_id,live_id FROM family_match_results WHERE status='reserve') c JOIN family_channels f ON f.id=c.channel_id LEFT JOIN candidate_health h ON h.live_id=c.live_id WHERE json_extract(f.data,'$.enabled')=1 AND COALESCE(h.disabled,0)=0 AND COALESCE(h.excluded_until,0)<=?1 AND COALESCE(h.next_check,0)<=?1 ORDER BY (SELECT COALESCE(MAX(updated_at),0) FROM progress WHERE id=c.channel_id) DESC,(SELECT COUNT(*) FROM family_candidates peers WHERE peers.channel_id=c.channel_id),COALESCE(h.next_check,0),c.live_id LIMIT 1",[util::now()],|r|r.get(0)).optional().ok().flatten();
                owner.zip(candidate)
            };
            if let Some((owner, id)) = next {
                let _ = begin(
                    a.clone(),
                    crate::automation::OwnerAccess::scheduled(owner),
                    id,
                )
                .await;
            }
        }
    });
}

pub(crate) fn pause(a: &App) {
    for job in a.health_control.jobs.lock().unwrap().values() {
        job.cancel.store(true, Ordering::Release);
    }
}
