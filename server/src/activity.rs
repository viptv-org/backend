//! Bounded, credential-free owner attention and reversible configuration history.
use super::*;
pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS automation_pause(id INTEGER PRIMARY KEY CHECK(id=1),paused INTEGER NOT NULL DEFAULT 0);INSERT OR IGNORE INTO automation_pause(id) VALUES(1);
CREATE TABLE IF NOT EXISTS owner_changes(id INTEGER PRIMARY KEY AUTOINCREMENT,at INTEGER NOT NULL,kind TEXT NOT NULL,target TEXT NOT NULL,before_data TEXT NOT NULL,after_data TEXT NOT NULL,undone INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS playback_events(id INTEGER PRIMARY KEY AUTOINCREMENT,at INTEGER NOT NULL,channel_id TEXT,reason TEXT NOT NULL,generation INTEGER);")
}
pub(crate) fn paused(db: &Connection) -> bool {
    db.query_row("SELECT paused FROM automation_pause WHERE id=1", [], |r| {
        r.get(0)
    })
    .unwrap_or(true)
}
pub(crate) fn snapshot(db: &Connection, kind: &str, target: &str) -> Result<Value, ApiError> {
    match kind {
        "health_settings" | "guide_settings" => {
            let table = if kind == "health_settings" {
                "health_settings"
            } else {
                "guide_settings"
            };
            let raw: String = db
                .query_row(&format!("SELECT data FROM {table} WHERE id=1"), [], |r| {
                    r.get(0)
                })
                .map_err(db_error)?;
            serde_json::from_str(&raw).map_err(|_| "Settings unavailable".into())
        }
        "candidate_exclusion" => db
            .query_row(
                "SELECT disabled,excluded_until FROM candidate_health WHERE live_id=?1",
                [target],
                |r| Ok(json!({"disabled":r.get::<_,bool>(0)?,"excluded_until":r.get::<_,i64>(1)?})),
            )
            .optional()
            .map(|v| v.unwrap_or(json!({"disabled":false,"excluded_until":0})))
            .map_err(db_error),
        "guide_source" => db
            .query_row(
                "SELECT enabled FROM guide_sources WHERE id=?1",
                [target],
                |r| Ok(json!({"enabled":r.get::<_,bool>(0)?})),
            )
            .map_err(db_error),
        "automation_pause" => Ok(json!({"paused":paused(db)})),
        _ => Err("Unsupported reversible setting".into()),
    }
}
pub(crate) fn record(
    db: &Connection,
    kind: &str,
    target: &str,
    before: Value,
) -> Result<(), ApiError> {
    let after = snapshot(db, kind, target)?;
    if before == after {
        return Ok(());
    }
    db.execute(
        "INSERT INTO owner_changes(at,kind,target,before_data,after_data) VALUES(?1,?2,?3,?4,?5)",
        params![
            util::now(),
            kind,
            target,
            before.to_string(),
            after.to_string()
        ],
    )
    .map_err(db_error)?;
    db.execute("DELETE FROM owner_changes WHERE id NOT IN (SELECT id FROM owner_changes ORDER BY id DESC LIMIT 200)",[]).map_err(db_error)?;
    Ok(())
}
pub(crate) fn playback_event(a: &App, channel: &str, reason: &str, generation: u64) {
    if let Ok(db) = a.db.try_lock() {
        let _ = db.execute(
            "INSERT INTO playback_events(at,channel_id,reason,generation) VALUES(?1,?2,?3,?4)",
            params![util::now(), channel, reason, generation],
        );
        let _=db.execute("DELETE FROM playback_events WHERE id NOT IN (SELECT id FROM playback_events ORDER BY id DESC LIMIT 200)",[]);
    }
}
fn pause_jobs(a: &App, db: &Connection) -> Result<(), ApiError> {
    crate::automation::pause(db, &a.catalog_control)?;
    crate::health::pause(a);
    crate::guides::pause(a, db)?;
    Ok(())
}
pub(crate) async fn pause(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    axum::Json(v): axum::Json<Value>,
) -> ApiResult {
    let paused = v["paused"].as_bool().ok_or("Set paused to true or false")?;
    blocking(move || {
        let db = a.db.lock().unwrap();
        provider::accounts::owner(&lease, &db)?;
        let before = snapshot(&db, "automation_pause", "")?;
        db.execute("UPDATE automation_pause SET paused=?1 WHERE id=1", [paused])
            .map_err(db_error)?;
        if paused {
            pause_jobs(&a, &db)?;
        }
        record(&db, "automation_pause", "", before)?;
        Ok(axum::Json(json!({"paused":paused})))
    })
    .await
}
pub(crate) async fn undo(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<i64>,
) -> ApiResult {
    blocking(move||{let mut db=a.db.lock().unwrap();provider::accounts::owner(&lease,&db)?;let(kind,target,before,after):(String,String,String,String)=db.query_row("SELECT kind,target,before_data,after_data FROM owner_changes WHERE id=?1 AND undone=0",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).map_err(db_error)?;let before:Value=serde_json::from_str(&before).map_err(|_|"Previous setting unavailable")?;let after:Value=serde_json::from_str(&after).map_err(|_|"Previous setting unavailable")?;if snapshot(&db,&kind,&target)?!=after{return Err(ApiError(StatusCode::CONFLICT,"This setting changed again; review its current controls".into()));}
    let tx=db.transaction().map_err(db_error)?;match kind.as_str(){
        "health_settings"|"guide_settings"=>{let table=if kind=="health_settings"{"health_settings"}else{"guide_settings"};let auth::Principal::Account{account_id,..}=lease.principal;tx.execute(&format!("UPDATE {table} SET data=?1,owner_id=?2 WHERE id=1"),params![before.to_string(),account_id]).map_err(db_error)?;},
        "candidate_exclusion"=>{tx.execute("UPDATE candidate_health SET disabled=?2,excluded_until=?3 WHERE live_id=?1",params![target,before["disabled"].as_bool(),before["excluded_until"].as_i64()]).map_err(db_error)?;},
        "guide_source"=>{tx.execute("UPDATE guide_sources SET enabled=?2,next_refresh=0 WHERE id=?1",params![target,before["enabled"].as_bool()]).map_err(db_error)?;},
        "automation_pause"=>{tx.execute("UPDATE automation_pause SET paused=?1 WHERE id=1",[before["paused"].as_bool()]).map_err(db_error)?;},_=>return Err("Unsupported reversible setting".into())
    };tx.execute("UPDATE owner_changes SET undone=1 WHERE id=?1",[id]).map_err(db_error)?;tx.commit().map_err(db_error)?;if paused(&db){pause_jobs(&a,&db)?;}
if kind=="health_settings"&&before["enabled"]==false{crate::health::pause(&a);}
if kind=="guide_settings"&&before["enabled"]==false{crate::guides::pause(&a,&db)?;}Ok(axum::Json(json!({"undone":true})))}).await
}
pub(crate) async fn list(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
) -> ApiResult {
    let sharing = session::shared::diagnostics(&a);
    let pools = provider::pools::list(State(a.clone()), Extension(lease.clone()))
        .await?
        .0;
    blocking(move||{let db=a.db.lock().unwrap();provider::accounts::owner(&lease,&db)?;let mut attention=Vec::new();
        let mut q=db.prepare("SELECT p.id,p.name,b.reason,b.until FROM providers p JOIN (SELECT provider_id,reason,until FROM catalog_backoff UNION SELECT provider_id,reason,until FROM health_accounts) b ON b.provider_id=p.id WHERE b.until>?1 GROUP BY p.id ORDER BY p.id LIMIT 100").map_err(db_error)?;
        for row in q.query_map([util::now()],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,i64>(3)?))).map_err(db_error)?{let(id,name,reason,until)=row.map_err(db_error)?;attention.push(json!({"id":format!("account:{id}"),"title":name,"reason":reason,"retry_at":until,"action":"accounts"}));}
        let mut q=db.prepare("SELECT f.id,f.data,(SELECT COUNT(*) FROM family_match_results m WHERE m.channel_id=f.id AND m.status='review') FROM family_channels f WHERE json_extract(f.data,'$.enabled')=1 LIMIT 1000").map_err(db_error)?;
        let rows=q.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?))).map_err(db_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
        for(id,data,review)in rows{let channel:Value=serde_json::from_str(&data).map_err(|_|"Channel unavailable")?;if review>0{attention.push(json!({"id":format!("matching:{id}"),"title":channel["name"],"reason":"uncertain_matches","count":review,"action":"matching","channel_id":id}));}let candidates=crate::lineup::candidates(&db,&id)?;let eligible=candidates.iter().filter(|candidate|crate::lineup::eligible(&db,&id,candidate).unwrap_or(false)).count();if eligible<2{attention.push(json!({"id":format!("backups:{id}"),"title":channel["name"],"reason":"insufficient_backups","count":eligible,"action":"health","channel_id":id}));}let guide=crate::guides::read(&db,&id)?;if guide["coverage"]["needs_attention"]==true{attention.push(json!({"id":format!("guide:{id}"),"title":channel["name"],"reason":"guide_coverage_below_24_hours","action":"guides","channel_id":id}));}}
        let mut q=db.prepare("SELECT h.live_id,l.name,h.reason,h.failures FROM candidate_health h LEFT JOIN provider_live l ON l.id=h.live_id WHERE h.failures>=2 AND h.state='cooling_down' AND h.disabled=0 ORDER BY h.failures DESC LIMIT 100").map_err(db_error)?;for row in q.query_map([],|r|Ok(json!({"id":format!("failure:{}",r.get::<_,String>(0)?),"title":r.get::<_,Option<String>>(1)?.unwrap_or_else(||"Missing candidate".into()),"reason":r.get::<_,Option<String>>(2)?,"count":r.get::<_,i64>(3)?,"action":"health"}))).map_err(db_error)?{attention.push(row.map_err(db_error)?);}
        let mut q=db.prepare("SELECT id,at,kind,target,undone FROM owner_changes ORDER BY id DESC LIMIT 50").map_err(db_error)?;let changes=q.query_map([],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"at":r.get::<_,i64>(1)?,"kind":r.get::<_,String>(2)?,"target":r.get::<_,String>(3)?,"undone":r.get::<_,bool>(4)?}))).map_err(db_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
        let mut q=db.prepare("SELECT at,channel_id,reason,generation FROM playback_events ORDER BY id DESC LIMIT 50").map_err(db_error)?;let events=q.query_map([],|r|Ok(json!({"at":r.get::<_,i64>(0)?,"channel_id":r.get::<_,Option<String>>(1)?,"reason":r.get::<_,String>(2)?,"generation":r.get::<_,Option<i64>>(3)?}))).map_err(db_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
        let mut q=db.prepare("SELECT channel_id,data FROM family_startups ORDER BY json_extract(data,'$.at') DESC LIMIT 20").map_err(db_error)?;let selections=q.query_map([],|r|Ok(json!({"channel_id":r.get::<_,String>(0)?,"details":serde_json::from_str::<Value>(&r.get::<_,String>(1)?).unwrap_or(Value::Null)}))).map_err(db_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
        let catalog=crate::automation::summary(&db)?;let guide=db.query_row("SELECT state,last_start,last_finish,reason FROM guide_runs WHERE id=1",[],|r|Ok(json!({"state":r.get::<_,String>(0)?,"last_start":r.get::<_,Option<i64>>(1)?,"last_finish":r.get::<_,Option<i64>>(2)?,"reason":r.get::<_,Option<String>>(3)?}))).map_err(db_error)?;
        attention.truncate(2000);Ok(axum::Json(json!({"paused":paused(&db),"attention":attention,"sharing":sharing,"pools":pools["pools"],"changes":changes,"recovery_events":events,"selections":selections,"catalog":catalog,"guide":guide})))
    }).await
}
