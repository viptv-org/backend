use super::*;
use serde::Serialize;

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Policy {
    pub(super) enabled: bool,
    pub(super) interval_minutes: u64,
    pub(super) concurrency: usize,
    pub(super) retries: usize,
    pub(super) request_timeout_seconds: u64,
    pub(super) provider_ids: Vec<i64>,
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

pub(super) fn queue(db: &mut Connection, owner: i64, rules: &Policy) -> Result<String, String> {
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

pub(super) fn claim(c: &Context) -> Result<Option<(CatalogLease, Policy)>, String> {
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
pub(super) fn classify(message: &str) -> (&'static str, bool, i64) {
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
