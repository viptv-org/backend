//! Durable, account-owned background catalog work; old snapshots stay readable.
use super::*;
use crate::{
    account_api::{self, Error},
    App, ResourceLease,
};
use axum::{
    extract::{Path, State},
    Extension, Json,
};
use rusqlite::OptionalExtension;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS provider_refresh_v2(
      provider_id INTEGER PRIMARY KEY REFERENCES providers(id) ON DELETE CASCADE,
      account_id INTEGER NOT NULL,token TEXT NOT NULL,state TEXT NOT NULL,
      requested_at INTEGER NOT NULL,started_at INTEGER,finished_at INTEGER,
      next_at INTEGER,error_code TEXT,counts TEXT);
      CREATE INDEX IF NOT EXISTS provider_refresh_due_v2 ON provider_refresh_v2(state,next_at,requested_at);")
}
fn storage(_: rusqlite::Error) -> &'static str {
    "provider_storage_unavailable"
}
fn available(db: &Connection, account: i64, id: i64) -> Result<bool, &'static str> {
    db.query_row("SELECT EXISTS(SELECT 1 FROM providers p JOIN provider_ownership o ON o.provider_id=p.id JOIN auth_accounts a ON a.id=o.account_id WHERE p.id=?1 AND o.account_id=?2 AND a.disabled=0 AND p.enabled=1 AND p.credentials_version=1 AND (p.enable_live=1 OR p.enable_movies=1 OR p.enable_series=1))",params![id,account],|r|r.get(0)).map_err(storage)
}
pub(super) fn enqueue(
    db: &Connection,
    account: i64,
    id: i64,
    replace: bool,
) -> Result<(), &'static str> {
    let token = uuid::Uuid::new_v4().to_string();
    if !available(db, account, id)? {
        db.execute("UPDATE provider_refresh_v2 SET token=?2,state='cancelled',finished_at=?3,next_at=NULL,error_code=NULL WHERE provider_id=?1",params![id,token,crate::util::now()]).map_err(storage)?;
        return Ok(());
    }
    db.execute("INSERT INTO provider_refresh_v2(provider_id,account_id,token,state,requested_at) VALUES(?1,?2,?3,'queued',?4)
      ON CONFLICT(provider_id) DO UPDATE SET account_id=excluded.account_id,token=excluded.token,state='queued',requested_at=excluded.requested_at,started_at=NULL,finished_at=NULL,next_at=NULL,error_code=NULL
      WHERE ?5 OR provider_refresh_v2.state NOT IN ('queued','running')",params![id,account,token,crate::util::now(),replace]).map_err(storage)?;
    Ok(())
}
pub(crate) fn prepare(db: &Connection) -> Result<(), &'static str> {
    // Newly migrated encrypted sources need their first refresh after restart.
    db.execute("INSERT OR IGNORE INTO provider_refresh_v2(provider_id,account_id,token,state,requested_at)
      SELECT p.id,o.account_id,'initial','queued',?1 FROM providers p JOIN provider_ownership o ON o.provider_id=p.id JOIN auth_accounts a ON a.id=o.account_id WHERE p.credentials_version=1 AND p.enabled=1 AND a.disabled=0 AND (p.enable_live=1 OR p.enable_movies=1 OR p.enable_series=1)",[crate::util::now()]).map_err(storage)?;
    Ok(())
}
#[derive(Clone)]
pub(super) struct Guard {
    pub provider: i64,
    account: i64,
    token: String,
    configuration: [u8; 32],
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
}
impl Guard {
    pub(super) fn validate(&self, db: &Connection) -> Result<(), String> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err("provider_refresh_cancelled".into());
        }
        if Instant::now() >= self.deadline {
            return Err("provider_refresh_timeout".into());
        }
        let current:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM provider_refresh_v2 WHERE provider_id=?1 AND account_id=?2 AND token=?3 AND state='running')",params![self.provider,self.account,self.token],|r|r.get(0)).map_err(storage)?;
        if !current || !available(db, self.account, self.provider)? {
            return Err("provider_refresh_cancelled".into());
        }
        if crate::sources::source_configuration(db, &format!("iptv:{}", self.provider))
            .map_err(|_| "provider_storage_unavailable")?
            != Some(self.configuration)
        {
            return Err("source_configuration_changed".into());
        }
        Ok(())
    }
    pub(super) fn complete(&self, db: &Connection, counts: &Value) -> Result<(), String> {
        self.validate(db)?;
        let now = crate::util::now();
        db.execute("UPDATE provider_refresh_v2 SET state='succeeded',finished_at=?3,next_at=?3+21600,error_code=NULL,counts=?4 WHERE provider_id=?1 AND token=?2 AND state='running'",params![self.provider,self.token,now,counts.to_string()]).map_err(storage)?;
        Ok(())
    }
}
fn claim(db: &Connection) -> Result<Option<Guard>, &'static str> {
    let tx = db.unchecked_transaction().map_err(storage)?;
    let now = crate::util::now();
    // A crashed process cannot hold a catalog indefinitely. Expired jobs get a
    // new token on claim, so a late old worker cannot publish into the new run.
    tx.execute("UPDATE provider_refresh_v2 SET state='queued',error_code='provider_refresh_interrupted' WHERE state='running' AND started_at<=?1-125",[now]).map_err(storage)?;
    tx.execute("UPDATE provider_refresh_v2 SET state='queued' WHERE state IN ('succeeded','failed') AND next_at IS NOT NULL AND next_at<=?1",[now]).map_err(storage)?;
    let active: i64 = tx
        .query_row(
            "SELECT count(*) FROM provider_refresh_v2 WHERE state='running'",
            [],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if active >= 2 {
        tx.commit().map_err(storage)?;
        return Ok(None);
    }
    let next:Option<(i64,i64)>=tx.query_row("SELECT r.provider_id,r.account_id FROM provider_refresh_v2 r WHERE r.state='queued'
      ORDER BY EXISTS(SELECT 1 FROM provider_refresh_v2 a WHERE a.account_id=r.account_id AND a.state='running'),
      COALESCE((SELECT MAX(a.started_at) FROM provider_refresh_v2 a WHERE a.account_id=r.account_id),0),r.requested_at,r.provider_id LIMIT 1",[],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(storage)?;
    let Some((provider, account)) = next else {
        tx.commit().map_err(storage)?;
        return Ok(None);
    };
    if !available(&tx, account, provider)? {
        tx.execute("UPDATE provider_refresh_v2 SET state='cancelled',next_at=NULL,finished_at=?2,error_code='source_configuration_changed' WHERE provider_id=?1",params![provider,now]).map_err(storage)?;
        tx.commit().map_err(storage)?;
        return Ok(None);
    }
    let configuration = crate::sources::source_configuration(&tx, &format!("iptv:{provider}"))
        .map_err(|_| "provider_storage_unavailable")?
        .ok_or("source_not_found")?;
    let token = uuid::Uuid::new_v4().to_string();
    tx.execute("UPDATE provider_refresh_v2 SET state='running',token=?2,started_at=?3,finished_at=NULL,error_code=NULL WHERE provider_id=?1",params![provider,token,now]).map_err(storage)?;
    tx.commit().map_err(storage)?;
    Ok(Some(Guard {
        provider,
        account,
        token,
        configuration,
        cancelled: Arc::new(AtomicBool::new(false)),
        deadline: Instant::now() + Duration::from_secs(120),
    }))
}
fn safe_error(raw: &str) -> &'static str {
    match raw {
        "provider_refresh_cancelled" => "provider_refresh_cancelled",
        "provider_refresh_timeout" => "provider_refresh_timeout",
        "source_configuration_changed" => "source_configuration_changed",
        "provider_private_destination" => "provider_private_destination",
        "source_route_migration_required" => "source_route_migration_required",
        "provider_credentials_rejected" => "provider_credentials_rejected",
        "provider_rate_limited" => "provider_rate_limited",
        "provider_response_too_large" => "provider_response_too_large",
        "provider_redirect_rejected" => "provider_redirect_rejected",
        "provider_timeout" => "provider_timeout",
        "provider_dns_unavailable" => "provider_dns_unavailable",
        "provider_unavailable" => "provider_unavailable",
        "secret_store_not_configured" => "secret_store_not_configured",
        "secret_key_unavailable" => "secret_key_unavailable",
        "secret_authentication_failed" => "secret_authentication_failed",
        "invalid_secret_envelope" => "invalid_secret_envelope",
        "provider_protocol_invalid" | "Provider index contains invalid entries" => {
            "provider_protocol_invalid"
        }
        "invalid_provider_endpoint" => "invalid_provider_endpoint",
        _ => "provider_refresh_failed",
    }
}
async fn run(service: ProviderService, guard: Guard) {
    struct CancelOnDrop(Arc<AtomicBool>);
    impl Drop for CancelOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    let _cancel_on_drop = CancelOnDrop(guard.cancelled.clone());
    let result = tokio::time::timeout(
        Duration::from_secs(120),
        service
            .for_account(guard.account)
            .sync_account(guard.provider, guard.clone()),
    )
    .await;
    let code = match result {
        Ok(Ok(_)) => return,
        Ok(Err(error)) => safe_error(&error),
        Err(_) => "provider_refresh_timeout",
    };
    guard.cancelled.store(true, Ordering::Release);
    let _=service.blocking(move |s| {
        s.lock()?.execute("UPDATE provider_refresh_v2 SET state='failed',finished_at=?3,next_at=?3+300,error_code=?4 WHERE provider_id=?1 AND token=?2 AND state='running'",params![guard.provider,guard.token,crate::util::now(),code]).map_err(storage)?;
        Ok(())
    }).await;
}
/// The driver holds no strong application DB reference while idle. Work is
/// bounded to two catalogs globally and four HTTP requests by the shared gate.
pub(crate) fn start(service: &ProviderService) {
    if service.vault.is_none() {
        if let Ok(db) = service.db.lock() {
            let _=db.execute("UPDATE provider_refresh_v2 SET state='failed',finished_at=?1,next_at=?1,error_code='secret_store_not_configured' WHERE state<>'cancelled'",[crate::util::now()]);
        }
        return;
    }
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let db = Arc::downgrade(&service.db);
    let client = service.client.clone();
    let vault = service.vault.clone();
    let network = service.semaphore.clone();
    #[cfg(test)]
    let fixture = service.allow_test_loopback;
    runtime.spawn(async move {
        let mut jobs=tokio::task::JoinSet::new();
        loop {
            while jobs.try_join_next().is_some() {}
            if jobs.len()<2 {
                let Some(db)=db.upgrade() else {break;};
                let mut service=ProviderService::new(db,client.clone());service.vault=vault.clone();service.semaphore=network.clone();
                #[cfg(test)] {service.allow_test_loopback=fixture;}
                let next=service.blocking(|s|claim(&*s.lock()?).map_err(str::to_owned)).await;
                if let Ok(Some(guard))=next {jobs.spawn(run(service,guard));continue;}
            }
            if db.strong_count()==0 {break;}
            tokio::select! {_=tokio::time::sleep(Duration::from_millis(250))=>{},_=jobs.join_next(),if !jobs.is_empty()=>{}}
        }
    });
}
pub(super) fn status(db: &Connection, id: i64) -> Result<Value, &'static str> {
    db.query_row("SELECT state,requested_at,started_at,finished_at,error_code,counts,next_at FROM provider_refresh_v2 WHERE provider_id=?1",[id],|r| {
        let error:Option<String>=r.get(4)?;let counts:Option<String>=r.get(5)?;
        Ok(json!({"state":r.get::<_,String>(0)?,"requested_at":r.get::<_,i64>(1)?,"started_at":r.get::<_,Option<i64>>(2)?,"finished_at":r.get::<_,Option<i64>>(3)?,"error_code":error,"error":error.as_ref().map(|e|account_api::description(e)),"counts":counts.and_then(|s|serde_json::from_str::<Value>(&s).ok()),"next_at":r.get::<_,Option<i64>>(6)?}))
    }).optional().map_err(storage).map(|v|v.unwrap_or(json!({"state":"idle"})))
}
pub(crate) async fn get(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<i64>,
) -> Result<Json<Value>, Error> {
    account_api::work(app, lease, move |db, account| {
        super::connections_v2::managed(db, account, id)?;
        Ok(status(db, id)?)
    })
    .await
}
pub(crate) async fn request(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<i64>,
) -> Result<(axum::http::StatusCode, Json<Value>), Error> {
    let configured = app.secret_vault.is_some();
    let result = account_api::work(app, lease, move |db, account| {
        super::connections_v2::managed(db, account, id)?;
        if !configured {
            return Err(Error::Code("secret_store_not_configured"));
        }
        if !available(db, account, id)? {
            return Err(Error::Code("catalog_unavailable"));
        }
        enqueue(db, account, id, false)?;
        Ok(status(db, id)?)
    })
    .await?;
    Ok((axum::http::StatusCode::ACCEPTED, result))
}
pub(crate) async fn cancel(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<i64>,
) -> Result<Json<Value>, Error> {
    account_api::work(app,lease,move |db,account| {
        super::connections_v2::managed(db,account,id)?;
        db.execute("UPDATE provider_refresh_v2 SET state='cancelled',token=?2,finished_at=?3,next_at=NULL,error_code=NULL WHERE provider_id=?1",params![id,uuid::Uuid::new_v4().to_string(),crate::util::now()]).map_err(storage)?;
        Ok(status(db,id)?)
    }).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::request as http;
    use axum::{extract::Query, response::IntoResponse, routing::get, Router};
    use base64::Engine;
    use std::sync::atomic::AtomicUsize;
    struct Fixture {
        app: App,
        base: String,
        server: tokio::task::JoinHandle<()>,
        mode: Arc<AtomicUsize>,
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.server.abort();
        }
    }
    async fn fixture() -> Fixture {
        let mut app = crate::auth_integration_tests::fixture();
        let vault=Arc::new(crate::secret_store::Vault::from_json(&json!({"active":"fixture","keys":{"fixture":base64::engine::general_purpose::STANDARD.encode([7u8;32])}}).to_string()).unwrap());
        app.secret_vault = Some(vault.clone());
        app.providers.vault = Some(vault);
        app.providers.allow_test_loopback = true;
        let mode = Arc::new(AtomicUsize::new(0));
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let changing = mode.clone();
        let entering = entered.clone();
        let releasing = release.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener,Router::new().route("/player_api.php",get(move |Query(q):Query<HashMap<String,String>>| {
                let mode=changing.load(Ordering::Acquire);let entered=entering.clone();let release=releasing.clone();
                async move {
                    let Some(action)=q.get("action") else {return Json(json!({"user_info":{"auth":1,"status":"Active","max_connections":3}})).into_response();};
                    if mode==1 {return (axum::http::StatusCode::SERVICE_UNAVAILABLE,"private upstream diagnostic").into_response();}
                    if mode==2 {return Json(json!([])).into_response();}
                    if mode==3 && action=="get_live_streams" {entered.notify_one();release.notified().await;}
                    Json(match action.as_str() {
                        "get_live_categories"=>json!([{"category_id":"9","category_name":"Zulu"},{"category_id":"1","category_name":"Alpha"}]),
                        "get_live_streams"=>json!([{"stream_id":9,"name":"Z first","category_id":"9","stream_icon":"http://art.invalid/logo.png"},{"stream_id":1,"name":"A second","category_id":"1"}]),
                        "get_vod_streams"=>json!([{"stream_id":21,"name":"Film (2020)","imdb_id":"tt1234567","container_extension":"mp4"}]),
                        "get_series"=>json!([{"series_id":31,"name":"Show (2021)","imdb_id":"tt7654321"}]),
                        _=>json!({}),
                    }).into_response()
                }
            }))).await.unwrap();
        });
        Fixture {
            app,
            base,
            server,
            mode,
            entered,
            release,
        }
    }
    async fn create(f: &Fixture) -> i64 {
        let (status,value)=http(&f.app,"member-token-1","POST","/api/v2/iptv/connections",json!({"name":"Background fixture","url":f.base,"username":"private-user","password":"private-password"})).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{value}");
        value["id"].as_i64().unwrap()
    }
    async fn wait(f: &Fixture, id: i64, state: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let (code, value) = http(
                    &f.app,
                    "member-token-1",
                    "GET",
                    &format!("/api/v2/iptv/connections/{id}/refresh"),
                    Value::Null,
                )
                .await;
                assert_eq!(code, axum::http::StatusCode::OK, "{value}");
                if value["state"] == state {
                    break value;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }
    #[tokio::test]
    async fn initial_background_refresh_is_atomic_and_failed_refresh_retains_snapshot() {
        let f = fixture().await;
        start(&f.app.providers);
        let id = create(&f).await;
        let success = wait(&f, id, "succeeded").await;
        assert_eq!(success["counts"]["live"], 2);
        assert_eq!(success["counts"]["vod"], 1);
        assert_eq!(success["counts"]["series"], 1);
        assert!(success["next_at"].as_i64().unwrap() > crate::util::now() + 21000);
        let path = format!("/api/v2/iptv/connections/{id}/refresh");
        for method in ["GET", "POST", "DELETE"] {
            assert_eq!(
                http(&f.app, "member-token-2", method, &path, Value::Null)
                    .await
                    .0,
                axum::http::StatusCode::NOT_FOUND
            );
        }
        let (_, before) = http(
            &f.app,
            "member-token-1",
            "GET",
            "/api/v2/iptv/live/channels",
            Value::Null,
        )
        .await;
        assert_eq!(before["items"][0]["name"], "Z first");
        assert_eq!(before["items"][1]["name"], "A second");
        f.app
            .db
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO favorites(profile_id,id,type,name) VALUES(1,?1,'live','Saved')",
                [format!("iptv:{id}:9")],
            )
            .unwrap();
        f.mode.store(1, Ordering::Release);
        assert_eq!(
            http(&f.app, "member-token-1", "POST", &path, Value::Null)
                .await
                .0,
            axum::http::StatusCode::ACCEPTED
        );
        let failed = wait(&f, id, "failed").await;
        assert_eq!(failed["error_code"], "provider_unavailable");
        assert!(!failed.to_string().contains("private upstream"));
        let (_, after) = http(
            &f.app,
            "member-token-1",
            "GET",
            "/api/v2/iptv/live/channels",
            Value::Null,
        )
        .await;
        assert_eq!(before, after);
        f.mode.store(2, Ordering::Release);
        http(&f.app, "member-token-1", "POST", &path, Value::Null).await;
        wait(&f, id, "succeeded").await;
        let (_, empty) = http(
            &f.app,
            "member-token-1",
            "GET",
            "/api/v2/iptv/live/channels",
            Value::Null,
        )
        .await;
        assert!(empty["items"].as_array().unwrap().is_empty());
        assert_ne!(empty["generation"], before["generation"]);
        assert_eq!(
            f.app
                .db
                .lock()
                .unwrap()
                .query_row("SELECT count(*) FROM favorites", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    #[tokio::test]
    async fn cancelled_inflight_refresh_cannot_publish_late_catalog() {
        let f = fixture().await;
        let id = create(&f).await;
        f.app.db.lock().unwrap().execute("INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES(?1,?2,'77','Previous')",params![format!("iptv:{id}:77"),id]).unwrap();
        f.mode.store(3, Ordering::Release);
        let guard = claim(&f.app.db.lock().unwrap()).unwrap().unwrap();
        let job = tokio::spawn(run(f.app.providers.clone(), guard));
        tokio::time::timeout(Duration::from_secs(3), f.entered.notified())
            .await
            .unwrap();
        let (_, cancelled) = http(
            &f.app,
            "member-token-1",
            "DELETE",
            &format!("/api/v2/iptv/connections/{id}/refresh"),
            Value::Null,
        )
        .await;
        assert_eq!(cancelled["state"], "cancelled");
        f.release.notify_one();
        tokio::time::timeout(Duration::from_secs(3), job)
            .await
            .unwrap()
            .unwrap();
        let db = f.app.db.lock().unwrap();
        assert_eq!(status(&db, id).unwrap()["state"], "cancelled");
        assert_eq!(
            db.query_row(
                "SELECT name FROM provider_live WHERE provider_id=?1",
                [id],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "Previous"
        );
    }
    #[tokio::test]
    async fn expired_claims_and_configuration_changes_invalidate_old_workers() {
        let f = fixture().await;
        let id = create(&f).await;
        let db = f.app.db.lock().unwrap();
        let first = claim(&db).unwrap().unwrap();
        assert!(claim(&db).unwrap().is_none());
        db.execute(
            "UPDATE provider_refresh_v2 SET started_at=?2 WHERE provider_id=?1",
            params![id, crate::util::now() - 126],
        )
        .unwrap();
        let second = claim(&db).unwrap().unwrap();
        assert_ne!(first.token, second.token);
        assert!(first.validate(&db).is_err());
        second.validate(&db).unwrap();
        db.execute("UPDATE providers SET enable_movies=0 WHERE id=?1", [id])
            .unwrap();
        assert!(second.complete(&db, &json!({})).is_err());
        enqueue(&db, 1, id, true).unwrap();
        assert_eq!(status(&db, id).unwrap()["state"], "queued");
    }
    #[tokio::test]
    async fn queue_bounds_claims_and_gives_another_account_the_next_slot() {
        let f = fixture().await;
        let first = create(&f).await;
        let (status, _) = http(
            &f.app,
            "member-token-1",
            "POST",
            "/api/v2/iptv/connections",
            json!({"name":"Second","url":f.base,"username":"second","password":"password"}),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
        let (status,other)=http(&f.app,"member-token-2","POST","/api/v2/iptv/connections",json!({"name":"Other account","url":f.base,"username":"private-user","password":"private-password"})).await;
        assert_eq!(status, axum::http::StatusCode::OK);
        let db = f.app.db.lock().unwrap();
        assert_eq!(claim(&db).unwrap().unwrap().provider, first);
        assert_eq!(
            claim(&db).unwrap().unwrap().provider,
            other["id"].as_i64().unwrap()
        );
        assert!(claim(&db).unwrap().is_none());
    }
    #[tokio::test]
    async fn missing_keyring_reports_failure_instead_of_queueing_forever() {
        let mut f = fixture().await;
        let id = create(&f).await;
        f.app.secret_vault = None;
        f.app.providers.vault = None;
        start(&f.app.providers);
        assert_eq!(
            status(&f.app.db.lock().unwrap(), id).unwrap()["error_code"],
            "secret_store_not_configured"
        );
        let (status, error) = http(
            &f.app,
            "member-token-1",
            "POST",
            &format!("/api/v2/iptv/connections/{id}/refresh"),
            Value::Null,
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error["error_code"], "secret_store_not_configured");
    }
}
