//! Durable catalog jobs. HTTP tasks are cancellable; publication additionally
//! validates the persisted run generation under the database lock.
use super::*;
use futures::{stream, StreamExt};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Weak;

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Policy {
    enabled: bool,
    interval_minutes: u64,
    concurrency: usize,
    retries: usize,
    request_timeout_seconds: u64,
    provider_ids: Vec<i64>,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_minutes: 360,
            concurrency: 2,
            retries: 1,
            request_timeout_seconds: 30,
            provider_ids: Vec::new(),
        }
    }
}
#[derive(Default)]
pub(crate) struct Control {
    started: AtomicBool,
    current: Mutex<Option<(String, Arc<AtomicBool>)>>,
    wake: Notify,
}
#[derive(Clone)]
struct Context {
    db: Arc<Mutex<Connection>>,
    providers: ProviderService,
    matching: Arc<Mutex<()>>,
    playback: Weak<PlaybackManager>,
    life: Weak<()>,
    control: Arc<Control>,
}
#[derive(Clone)]
pub(crate) struct CatalogLease {
    id: String,
    generation: i64,
    owner: i64,
    cancelled: Arc<AtomicBool>,
    attempt_cancelled: Option<Arc<AtomicBool>>,
}
struct AttemptGuard(Arc<AtomicBool>);
impl Drop for AttemptGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
impl CatalogLease {
    pub(crate) fn validate(&self, db: &Connection) -> Result<(), String> {
        if self.cancelled.load(Ordering::Acquire)
            || self
                .attempt_cancelled
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::Acquire))
        {
            return Err("Catalog run cancelled".into());
        }
        provider::accounts::active_owner(db, self.owner)?;
        let current:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_runs WHERE id=?1 AND generation=?2 AND state='running')",params![self.id,self.generation],|r|r.get(0)).map_err(|_|"Catalog run unavailable")?;
        if !current {
            return Err("Catalog run cancelled or superseded".into());
        }
        Ok(())
    }
}
pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS catalog_schedule(id INTEGER PRIMARY KEY CHECK(id=1),data TEXT NOT NULL,owner_id INTEGER,next_run INTEGER);
        CREATE TABLE IF NOT EXISTS catalog_runs(id TEXT PRIMARY KEY,state TEXT NOT NULL,owner_id INTEGER NOT NULL,policy TEXT NOT NULL,generation INTEGER NOT NULL DEFAULT 0,created_at INTEGER NOT NULL,started_at INTEGER,finished_at INTEGER,reason TEXT,matching_state TEXT NOT NULL DEFAULT 'pending');
        CREATE TABLE IF NOT EXISTS catalog_results(run_id TEXT NOT NULL REFERENCES catalog_runs(id) ON DELETE CASCADE,provider_id INTEGER NOT NULL,status TEXT NOT NULL DEFAULT 'pending',attempts INTEGER NOT NULL DEFAULT 0,reason TEXT,next_retry INTEGER,data TEXT,PRIMARY KEY(run_id,provider_id));
        CREATE TABLE IF NOT EXISTS catalog_backoff(provider_id INTEGER PRIMARY KEY,until INTEGER NOT NULL,reason TEXT NOT NULL,strikes INTEGER NOT NULL);
        UPDATE catalog_runs SET state='queued',reason='resuming_after_restart' WHERE state='running';
        UPDATE catalog_results SET status='pending' WHERE status='running';
        UPDATE catalog_runs SET state='cancelled',finished_at=strftime('%s','now'),reason='cancelled' WHERE state='cancel_requested';
        UPDATE catalog_results SET status='cancelled',reason='cancelled' WHERE status IN ('pending','running') AND run_id IN (SELECT id FROM catalog_runs WHERE state='cancelled');")?;
    db.execute(
        "INSERT OR IGNORE INTO catalog_schedule(id,data) VALUES(1,?1)",
        [serde_json::to_string(&Policy::default()).unwrap()],
    )?;
    Ok(())
}
fn policy(db: &Connection) -> Result<Policy, String> {
    let data: String = db
        .query_row("SELECT data FROM catalog_schedule WHERE id=1", [], |r| {
            r.get(0)
        })
        .map_err(|_| "Catalog settings unavailable")?;
    serde_json::from_str(&data).map_err(|_| "Catalog settings invalid".into())
}
fn queue(db: &mut Connection, owner: i64, rules: &Policy) -> Result<String, String> {
    if rules.provider_ids.is_empty() {
        return Err("Select accounts to refresh".into());
    }
    let active:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_runs WHERE state IN ('queued','running','cancel_requested'))",[],|r|r.get(0)).map_err(|_|"Catalog runs unavailable")?;
    if active {
        return Err("A catalog run is already active".into());
    }
    let id = uuid::Uuid::new_v4().to_string();
    let now = util::now();
    let tx = db.transaction().map_err(|_| "Catalog run unavailable")?;
    tx.execute("INSERT INTO catalog_runs(id,state,owner_id,policy,created_at) VALUES(?1,'queued',?2,?3,?4)",params![id,owner,serde_json::to_string(rules).unwrap(),now]).map_err(|_|"Catalog run unavailable")?;
    for provider in &rules.provider_ids {
        tx.execute(
            "INSERT INTO catalog_results(run_id,provider_id) VALUES(?1,?2)",
            params![id, provider],
        )
        .map_err(|_| "Catalog run unavailable")?;
    }
    tx.execute(
        "UPDATE catalog_schedule SET next_run=?1 WHERE id=1",
        [rules
            .enabled
            .then_some(now + rules.interval_minutes as i64 * 60)],
    )
    .map_err(|_| "Catalog schedule unavailable")?;
    tx.execute("DELETE FROM catalog_runs WHERE id IN (SELECT id FROM catalog_runs WHERE state NOT IN ('queued','running','cancel_requested') ORDER BY created_at DESC,rowid DESC LIMIT -1 OFFSET 20)",[]).map_err(|_|"Catalog history unavailable")?;
    tx.commit().map_err(|_| "Catalog run unavailable")?;
    Ok(id)
}
pub(crate) fn summary(db: &Connection) -> Result<Value, String> {
    let rules = policy(db)?;
    let next: Option<i64> = db
        .query_row(
            "SELECT next_run FROM catalog_schedule WHERE id=1",
            [],
            |r| r.get(0),
        )
        .map_err(|_| "Catalog schedule unavailable")?;
    let last=db.query_row("SELECT id,state,created_at,started_at,finished_at,reason,matching_state FROM catalog_runs ORDER BY created_at DESC,rowid DESC LIMIT 1",[],|r|Ok(json!({"id":r.get::<_,String>(0)?,"state":r.get::<_,String>(1)?,"created_at":r.get::<_,i64>(2)?,"started_at":r.get::<_,Option<i64>>(3)?,"finished_at":r.get::<_,Option<i64>>(4)?,"reason":r.get::<_,Option<String>>(5)?,"matching_state":r.get::<_,String>(6)?}))).optional().map_err(|_|"Catalog history unavailable")?;
    let last = if let Some(mut last) = last {
        let mut q=db.prepare("SELECT provider_id,status,attempts,reason,next_retry FROM catalog_results WHERE run_id=?1 ORDER BY provider_id").map_err(|_|"Catalog results unavailable")?;
        let rows=q.query_map([last["id"].as_str().unwrap()],|r|Ok(json!({"provider_id":r.get::<_,i64>(0)?,"status":r.get::<_,String>(1)?,"attempts":r.get::<_,i64>(2)?,"reason":r.get::<_,Option<String>>(3)?,"next_retry":r.get::<_,Option<i64>>(4)?}))).map_err(|_|"Catalog results unavailable")?.collect::<rusqlite::Result<Vec<_>>>().map_err(|_|"Catalog results unavailable")?;
        last["accounts"] = json!(rows);
        last
    } else {
        Value::Null
    };
    Ok(json!({"settings":rules,"next_run":next,"last_run":last}))
}
pub(crate) fn start(a: &App) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    if a.catalog_control.started.swap(true, Ordering::AcqRel) {
        return;
    }
    let context = Context {
        db: a.db.clone(),
        providers: a.providers.clone(),
        matching: a.family_matching_gate.clone(),
        playback: Arc::downgrade(&a.playback),
        life: Arc::downgrade(&a.automation_life),
        control: a.catalog_control.clone(),
    };
    runtime.spawn(async move {
        loop {
            if context.life.upgrade().is_none()||context.playback.upgrade().is_none_or(|p|p.is_shutting_down()){break;}
            let worker=context.clone();
            let claim=tokio::task::spawn_blocking(move||claim(&worker)).await;
            if let Ok(Ok(Some((lease,rules))))=claim {
                *context.control.current.lock().unwrap()=Some((lease.id.clone(),lease.cancelled.clone()));
                execute(context.clone(),lease,rules).await;
                *context.control.current.lock().unwrap()=None;
            }else{tokio::select!{_=tokio::time::sleep(Duration::from_secs(1))=>{},_=context.control.wake.notified()=>{}}}
        }
    });
}
fn claim(c: &Context) -> Result<Option<(CatalogLease, Policy)>, String> {
    let mut db = c.db.lock().unwrap();
    let queued: Option<String> = db
        .query_row(
            "SELECT id FROM catalog_runs WHERE state='queued' ORDER BY created_at,rowid LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| "Catalog runs unavailable")?;
    let id = if let Some(id) = queued {
        id
    } else {
        let rules = policy(&db)?;
        let (owner, next): (Option<i64>, Option<i64>) = db
            .query_row(
                "SELECT owner_id,next_run FROM catalog_schedule WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| "Catalog schedule unavailable")?;
        if crate::activity::paused(&db)
            || !rules.enabled
            || next.is_none_or(|at| at > util::now())
            || rules.provider_ids.is_empty()
        {
            return Ok(None);
        }
        let Some(owner) = owner else {
            return Ok(None);
        };
        if provider::accounts::active_owner(&db, owner).is_err() {
            db.execute(
                "UPDATE catalog_schedule SET next_run=?1 WHERE id=1",
                [util::now() + 3600],
            )
            .map_err(|_| "Catalog schedule unavailable")?;
            return Ok(None);
        }
        queue(&mut db, owner, &rules)?
    };
    let (owner, data): (i64, String) = db
        .query_row(
            "SELECT owner_id,policy FROM catalog_runs WHERE id=?1",
            [&id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| "Catalog run unavailable")?;
    let rules: Policy = serde_json::from_str(&data).map_err(|_| "Catalog run policy invalid")?;
    db.execute(
        "UPDATE catalog_runs SET state='running',generation=generation+1,started_at=?2 WHERE id=?1",
        params![id, util::now()],
    )
    .map_err(|_| "Catalog run unavailable")?;
    let generation = db
        .query_row(
            "SELECT generation FROM catalog_runs WHERE id=?1",
            [&id],
            |r| r.get(0),
        )
        .map_err(|_| "Catalog run unavailable")?;
    Ok(Some((
        CatalogLease {
            id,
            generation,
            owner,
            cancelled: Arc::new(AtomicBool::new(false)),
            attempt_cancelled: None,
        },
        rules,
    )))
}
fn classify(message: &str) -> (&'static str, bool, i64) {
    match message {
        "Account credentials expired or rejected"
        | "Provider returned HTTP 401"
        | "Provider returned HTTP 403" => ("authentication_failed", false, 3600),
        "Provider returned HTTP 429" => ("rate_limited", false, 900),
        "Provider returned an empty catalog; previous metadata retained" => {
            ("empty_catalog", false, 900)
        }
        "Provider index contains invalid entries"
        | "Provider returned invalid data"
        | "Provider response is too large" => ("invalid_metadata", false, 900),
        "Provider not found or disabled" | "Provider was deleted or disabled during sync" => {
            ("provider_unavailable", false, 300)
        }
        _ => ("request_failed", true, 300),
    }
}
async fn cancelled(c: &Context, lease: &CatalogLease) {
    loop {
        if lease.cancelled.load(Ordering::Acquire)
            || c.life.upgrade().is_none()
            || c.playback.upgrade().is_none_or(|p| p.is_shutting_down())
        {
            lease.cancelled.store(true, Ordering::Release);
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
async fn account(
    c: Context,
    lease: CatalogLease,
    provider: i64,
    retries: usize,
    timeout_seconds: u64,
) {
    let before = c.clone();
    let access = lease.clone();
    let ready=tokio::task::spawn_blocking(move||{
        let db=before.db.lock().unwrap();access.validate(&db)?;
        let wait:Option<(i64,String)>=db.query_row("SELECT until,reason FROM catalog_backoff WHERE provider_id=?1 AND until>?2",params![provider,util::now()],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(|_|"Account backoff unavailable")?;
        if let Some((until,reason))=wait {db.execute("UPDATE catalog_results SET status='backoff',reason=?3,next_retry=?4 WHERE run_id=?1 AND provider_id=?2",params![access.id,provider,reason,until]).map_err(|_|"Catalog results unavailable")?;return Ok::<_,String>(false);}
        Ok(true)
    }).await;
    if !matches!(ready, Ok(Ok(true))) {
        return;
    }
    for attempt in 0..=retries {
        let worker = c.clone();
        let access = lease.clone();
        let started=tokio::task::spawn_blocking(move||{let db=worker.db.lock().unwrap();access.validate(&db)?;db.execute("UPDATE catalog_results SET status='running',attempts=attempts+1 WHERE run_id=?1 AND provider_id=?2",params![access.id,provider]).map_err(|_|"Catalog result unavailable")?;Ok::<_,String>(())}).await;
        if !matches!(started, Ok(Ok(()))) {
            return;
        }
        let attempt_guard = AttemptGuard(Arc::new(AtomicBool::new(false)));
        let mut attempt_lease = lease.clone();
        attempt_lease.attempt_cancelled = Some(attempt_guard.0.clone());
        let result = tokio::time::timeout(
            Duration::from_secs(timeout_seconds),
            c.providers.sync_catalog(provider, attempt_lease),
        )
        .await
        .unwrap_or_else(|_| Err("Provider request failed or timed out".into()));
        drop(attempt_guard);
        let (reason, retry, delay) = result
            .as_ref()
            .err()
            .map(|e| classify(e))
            .unwrap_or(("", false, 0));
        if retry && attempt < retries {
            tokio::time::sleep(Duration::from_millis(250 * (1 << attempt))).await;
            continue;
        }
        let worker = c.clone();
        let access = lease.clone();
        let _=tokio::task::spawn_blocking(move||{
            let mut db=worker.db.lock().unwrap();access.validate(&db)?;let tx=db.transaction().map_err(|_|"Catalog results unavailable")?;
            let next=if result.is_err(){
                let strikes:i64=tx.query_row("SELECT strikes FROM catalog_backoff WHERE provider_id=?1",[provider],|r|r.get(0)).optional().map_err(|_|"Account backoff unavailable")?.unwrap_or(0)+1;
                let next=util::now()+(delay*(1i64<<strikes.min(6).saturating_sub(1))).min(21600);
                tx.execute("INSERT INTO catalog_backoff(provider_id,until,reason,strikes) VALUES(?1,?2,?3,?4) ON CONFLICT(provider_id) DO UPDATE SET until=excluded.until,reason=excluded.reason,strikes=excluded.strikes",params![provider,next,reason,strikes]).map_err(|_|"Account backoff unavailable")?;Some(next)
            }else{tx.execute("DELETE FROM catalog_backoff WHERE provider_id=?1",[provider]).map_err(|_|"Account backoff unavailable")?;None};
            tx.execute("UPDATE catalog_results SET status=?3,reason=?4,next_retry=?5,data=?6 WHERE run_id=?1 AND provider_id=?2",params![access.id,provider,if result.is_ok(){"completed"}else{"failed"},if reason.is_empty(){None}else{Some(reason)},next,result.ok().map(|v|v.to_string())]).map_err(|_|"Catalog results unavailable")?;
            tx.commit().map_err(|_|"Catalog results unavailable")?;Ok::<_,String>(())
        }).await;
        return;
    }
}
async fn execute(c: Context, lease: CatalogLease, rules: Policy) {
    let worker = c.clone();
    let access = lease.clone();
    let pending=tokio::task::spawn_blocking(move||{let db=worker.db.lock().unwrap();access.validate(&db)?;let result=db.prepare("SELECT provider_id FROM catalog_results WHERE run_id=?1 AND status IN ('pending','running') ORDER BY provider_id").map_err(|_|"Catalog results unavailable")?.query_map([&access.id],|r|r.get::<_,i64>(0)).map_err(|_|"Catalog results unavailable")?.collect::<rusqlite::Result<Vec<_>>>().map_err(|_|"Catalog results unavailable".to_owned());result}).await;
    let mut reason = None;
    if let Ok(Ok(providers)) = pending {
        let work = stream::iter(providers)
            .map(|provider| {
                account(
                    c.clone(),
                    lease.clone(),
                    provider,
                    rules.retries,
                    rules.request_timeout_seconds,
                )
            })
            .buffer_unordered(rules.concurrency)
            .collect::<Vec<_>>();
        tokio::select! {_=work=>{},_=cancelled(&c,&lease)=>{reason=Some("cancelled");},_=tokio::time::sleep(Duration::from_secs(600))=>{reason=Some("run_deadline");lease.cancelled.store(true,Ordering::Release);}}
    } else {
        reason = Some("authorization_lost");
    }
    let mut matching = "cancelled";
    if reason.is_none() {
        for attempt in 0..3 {
            let worker = c.clone();
            let access = lease.clone();
            let work = tokio::task::spawn_blocking(move || {
                lineup::matching::match_catalog(&worker.db, &worker.matching, &access)
            });
            let result = tokio::select! {
                result=work=>Some(result),
                _=cancelled(&c,&lease)=>{reason=Some("cancelled");None},
            };
            match result {
                Some(Ok(Ok(_))) => {
                    matching = "completed";
                    break;
                }
                Some(Ok(Err(ApiError(code, _))))
                    if attempt < 2
                        && (code == StatusCode::CONFLICT
                            || code == StatusCode::TOO_MANY_REQUESTS) =>
                {
                    tokio::select! {
                        _=tokio::time::sleep(Duration::from_secs(1<<attempt))=>{},
                        _=cancelled(&c,&lease)=>{reason=Some("cancelled");break;},
                    }
                }
                Some(_) => {
                    matching = "retry_required";
                    break;
                }
                None => break,
            }
        }
    }
    let worker = c.clone();
    let access = lease.clone();
    let _=tokio::task::spawn_blocking(move||{
        let db=worker.db.lock().unwrap();
        if reason.is_none() && access.validate(&db).is_err() { reason=Some("authorization_lost"); }
        let unfinished:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_results WHERE run_id=?1 AND status IN ('pending','running'))",[&access.id],|r|r.get(0)).map_err(|_|"Catalog results unavailable")?;
        if reason.is_none() && unfinished { reason=Some("incomplete_results"); }
        let state=if reason.is_some(){"cancelled"}else{"completed"};
        db.execute("UPDATE catalog_runs SET state=?3,finished_at=?4,reason=?5,matching_state=?6 WHERE id=?1 AND generation=?2",params![access.id,access.generation,state,util::now(),reason,matching]).map_err(|_|"Catalog run unavailable")?;
        if state=="cancelled" {db.execute("UPDATE catalog_results SET status='cancelled',reason=COALESCE(reason,'cancelled') WHERE run_id=?1 AND status IN ('pending','running')",[&access.id]).map_err(|_|"Catalog results unavailable")?;}
        let settings=policy(&db)?;db.execute("UPDATE catalog_schedule SET next_run=?1 WHERE id=1",[settings.enabled.then_some(util::now()+settings.interval_minutes as i64*60)]).map_err(|_|"Catalog schedule unavailable")?;Ok::<_,String>(())
    }).await;
}

pub(crate) async fn list(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
) -> ApiResult {
    blocking(move || {
        let db = a.db.lock().unwrap();
        provider::accounts::owner(&lease, &db)?;
        summary(&db).map(axum::Json).map_err(ApiError::from)
    })
    .await
}
pub(crate) async fn configure(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    axum::Json(value): axum::Json<Value>,
) -> ApiResult {
    let rules: Policy =
        serde_json::from_value(value).map_err(|_| ApiError::from("Invalid catalog schedule"))?;
    if !(15..=10080).contains(&rules.interval_minutes)
        || !(1..=4).contains(&rules.concurrency)
        || rules.retries > 3
        || !(5..=60).contains(&rules.request_timeout_seconds)
        || rules.provider_ids.len() > 20
        || rules.provider_ids.iter().any(|id| *id < 1)
        || rules
            .provider_ids
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != rules.provider_ids.len()
    {
        return Err("Choose 0–20 unique accounts, interval 15–10080 minutes, concurrency 1–4, retries 0–3 and request timeout 5–60 seconds".into());
    }
    if rules.enabled && rules.provider_ids.is_empty() {
        return Err("Select accounts before enabling the schedule".into());
    }
    let control = a.catalog_control.clone();
    let wake = control.clone();
    let result = blocking(move || {
        let db = a.db.lock().unwrap();
        provider::accounts::owner(&lease, &db)?;
        for id in &rules.provider_ids {
            let exists: bool = db
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM providers WHERE id=?1)",
                    [id],
                    |r| r.get(0),
                )
                .map_err(db_error)?;
            if !exists {
                return Err("Selected account no longer exists".into());
            }
        }
        let auth::Principal::Account { account_id, .. } = lease.principal;
        db.execute(
            "UPDATE catalog_schedule SET data=?1,owner_id=?2,next_run=?3 WHERE id=1",
            params![
                serde_json::to_string(&rules).unwrap(),
                account_id,
                rules.enabled.then_some(util::now())
            ],
        )
        .map_err(db_error)?;
        if !rules.enabled {
            cancel_current(&db, &control).map_err(ApiError::from)?;
        }
        summary(&db).map(axum::Json).map_err(ApiError::from)
    })
    .await;
    wake.wake.notify_one();
    result
}
pub(crate) async fn run(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
) -> Result<(StatusCode, axum::Json<Value>), ApiError> {
    let control = a.catalog_control.clone();
    let response = blocking(move || {
        let mut db = a.db.lock().unwrap();
        provider::accounts::owner(&lease, &db)?;
        let rules = policy(&db)?;
        let auth::Principal::Account { account_id, .. } = lease.principal;
        queue(&mut db, account_id, &rules).map_err(|message| {
            ApiError(
                if message == "A catalog run is already active" {
                    StatusCode::CONFLICT
                } else {
                    StatusCode::BAD_REQUEST
                },
                message,
            )
        })?;
        summary(&db).map(axum::Json).map_err(ApiError::from)
    })
    .await?;
    control.wake.notify_one();
    Ok((StatusCode::ACCEPTED, response))
}
fn cancel_current(db: &Connection, control: &Control) -> Result<(), String> {
    db.execute("UPDATE catalog_runs SET state='cancelled',finished_at=?1,reason='cancelled' WHERE state='queued'",[util::now()]).map_err(|_|"Catalog cancellation unavailable")?;
    db.execute(
        "UPDATE catalog_runs SET state='cancel_requested' WHERE state='running'",
        [],
    )
    .map_err(|_| "Catalog cancellation unavailable")?;
    db.execute("UPDATE catalog_results SET status='cancelled',reason='cancelled' WHERE status IN ('pending','running') AND run_id IN (SELECT id FROM catalog_runs WHERE state='cancelled')",[]).map_err(|_|"Catalog cancellation unavailable")?;
    if let Some((_, cancelled)) = &*control.current.lock().unwrap() {
        cancelled.store(true, Ordering::Release);
    }
    Ok(())
}
pub(crate) async fn cancel(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
) -> ApiResult {
    blocking(move || {
        let db = a.db.lock().unwrap();
        provider::accounts::owner(&lease, &db)?;
        cancel_current(&db, &a.catalog_control)?;
        summary(&db).map(axum::Json).map_err(ApiError::from)
    })
    .await
}

pub(crate) fn pause(db: &Connection, control: &Control) -> Result<(), String> {
    cancel_current(db, control)
}

/// Explicit authority for durable automation; never fabricate an HTTP session.
#[derive(Clone)]
pub(crate) struct OwnerAccess {
    owner: i64,
    request: Option<ResourceLease>,
}
impl OwnerAccess {
    pub(crate) fn request(lease: ResourceLease) -> Self {
        let auth::Principal::Account { account_id, .. } = lease.principal;
        Self {
            owner: account_id,
            request: Some(lease),
        }
    }
    pub(crate) fn scheduled(owner: i64) -> Self {
        Self {
            owner,
            request: None,
        }
    }
    pub(crate) fn validate(&self, db: &Connection) -> Result<(), ApiError> {
        if let Some(lease) = &self.request {
            lease.validate(db)?;
        }
        provider::accounts::active_owner(db, self.owner).map_err(ApiError::from)
    }
}
