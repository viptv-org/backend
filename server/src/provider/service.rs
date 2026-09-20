use super::*;

#[derive(Clone)]
pub struct ProviderService {
    pub db: Arc<Mutex<Connection>>,
    pub client: reqwest::Client,
    pub semaphore: Arc<Semaphore>,
    pub(super) playback_gates: Arc<Mutex<HashMap<i64, PlaybackGate>>>,
}
pub(super) struct PlaybackGate {
    pub(super) issued: u64,
    pub(super) report_generation: u64,
    pub(super) semaphore: Arc<Semaphore>,
}

#[derive(Clone)]
pub(super) struct Provider {
    pub(super) id: i64,
    pub(super) name: String,
    pub(super) url: String,
    pub(super) username: String,
    pub(super) password: String,
}

impl Provider {
    pub(super) fn ensure_current(&self, db: &Connection) -> Result<(), String> {
        let current:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM providers WHERE id=?1 AND url=?2 AND username=?3 AND password=?4 AND enabled=1)",params![self.id,self.url,self.username,self.password],|r|r.get(0)).map_err(db_error)?;
        if !current {
            return Err(
                "Provider credentials changed or account became unavailable; retry the request"
                    .into(),
            );
        }
        Ok(())
    }
}

// Single enabled-provider row mapping, shared by locked and already-locked callers.
pub(super) fn provider_row(db: &Connection, id: i64) -> Result<Provider, String> {
    db.query_row(
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
    .map_err(|_| "Provider not found or disabled".into())
}

impl ProviderService {
    pub fn new(db: Arc<Mutex<Connection>>, client: reqwest::Client) -> Self {
        Self {
            db,
            client,
            semaphore: Arc::new(Semaphore::new(4)),
            playback_gates: Default::default(),
        }
    }

    pub async fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce(Self) -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        let service = self.clone();
        tokio::task::spawn_blocking(move || work(service))
            .await
            .map_err(|_| "Provider database worker stopped".to_string())?
    }

    pub(super) fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, String> {
        self.db
            .lock()
            .map_err(|_| "Provider database is unavailable".into())
    }

    pub(super) fn provider(&self, id: i64) -> Result<Provider, String> {
        let db = self.lock()?;
        provider_row(&db, id)
    }

    pub(super) fn scopes(&self, id: i64) -> Result<[bool; 3], String> {
        self.lock()?.query_row("SELECT enable_live,enable_movies,enable_series FROM providers WHERE id=?1 AND enabled=1", [id], |r| Ok([r.get(0)?,r.get(1)?,r.get(2)?])).map_err(|_| "Provider not found or disabled".into())
    }

    pub(super) fn provider_for_kind(&self, id: i64, kind: &str) -> Result<Provider, String> {
        self.lock()?.query_row("SELECT id,name,url,username,password FROM providers WHERE id=?1 AND enabled=1 AND CASE ?2 WHEN 'live' THEN enable_live WHEN 'movie' THEN enable_movies WHEN 'series' THEN enable_series ELSE 0 END=1", params![id,kind], |r| Ok(Provider {
            id:r.get(0)?,name:r.get(1)?,url:r.get(2)?,username:r.get(3)?,password:r.get(4)?,
        })).map_err(|_| "Provider not found or content scope disabled".into())
    }

    pub fn list(&self) -> Result<Value, String> {
        let db = self.lock()?;
        let mut stmt = db
            .prepare(
                "SELECT id,name,url,username,enabled,max_connections,enable_live,enable_movies,enable_series FROM providers ORDER BY id",
            )
            .map_err(db_error)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(json!({
                    "id":r.get::<_,i64>(0)?, "name":r.get::<_,String>(1)?,
                    "url":r.get::<_,String>(2)?,
                    "warp":egress::enabled(&db,r.get(0)?),"enabled":r.get::<_,bool>(4)?, "max_connections":r.get::<_,i64>(5)?,"enable_live":r.get::<_,bool>(6)?,"enable_movies":r.get::<_,bool>(7)?,"enable_series":r.get::<_,bool>(8)?
                }))
            })
            .map_err(db_error)?;
        Ok(Value::Array(
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(db_error)?,
        ))
    }

    pub fn add(&self, value: Value) -> Result<Value, String> {
        let name = required_string(&value, "name", 200)?;
        let username = required_string(&value, "username", 512)?;
        let password = required_string(&value, "password", 2048)?;
        let raw_url = required_string(&value, "url", 4096)?;
        let url = base_url(&raw_url)?;
        let max_connections = match value.get("max_connections") {
            None => 1,
            Some(v) => v
                .as_i64()
                .filter(|n| (1..=32).contains(n))
                .ok_or("max_connections must be an integer from 1 to 32")?,
        };
        let warp = optional_bool(&value, "warp")?.unwrap_or(false);
        if warp {
            egress::configured()?;
        }
        let live = optional_bool(&value, "enable_live")?.unwrap_or(true);
        let movies = optional_bool(&value, "enable_movies")?.unwrap_or(true);
        let series = optional_bool(&value, "enable_series")?.unwrap_or(true);
        let db = self.lock()?;
        if accounts::duplicate(
            &db,
            &json!({"url":url.as_str().trim_end_matches('/'),"username":username}),
            None,
        )?
        .is_some()
        {
            return Err("Provider login already configured".into());
        }
        db.execute(
            "INSERT INTO providers(name,url,username,password,max_connections,enable_live,enable_movies,enable_series) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![name, url.as_str().trim_end_matches('/'), username, password, max_connections,live,movies,series],
        )
        .map_err(db_error)?;
        let id = db.last_insert_rowid();
        egress::set(&db, id, warp)?;
        Ok(json!({"id": id, "name":name,"warp":warp,
            "url":url.as_str().trim_end_matches('/'),"enabled":true,"max_connections":max_connections,"enable_live":live,"enable_movies":movies,"enable_series":series}))
    }

    pub fn update(&self, id: i64, patch: Value) -> Result<Value, String> {
        let object = patch
            .as_object()
            .ok_or("Provider patch must be an object")?;
        if object.is_empty()
            || object.keys().any(|k| {
                ![
                    "enabled",
                    "warp",
                    "max_connections",
                    "enable_live",
                    "enable_movies",
                    "enable_series",
                ]
                .contains(&k.as_str())
            })
        {
            return Err("Provider patch accepts enabled, max_connections, enable_live, enable_movies, enable_series".into());
        }
        let warp = optional_bool(&patch, "warp")?;
        if warp == Some(true) {
            egress::configured()?;
        }
        let live = optional_bool(&patch, "enable_live")?;
        let movies = optional_bool(&patch, "enable_movies")?;
        let series = optional_bool(&patch, "enable_series")?;
        let enabled = object
            .get("enabled")
            .map(|v| v.as_bool().ok_or("enabled must be boolean"))
            .transpose()?;
        let limit = object
            .get("max_connections")
            .map(|v| {
                v.as_i64()
                    .filter(|n| (1..=32).contains(n))
                    .ok_or("max_connections must be an integer from 1 to 32")
            })
            .transpose()?;
        let mut db = self.lock()?;
        let pool = pools::ensure(&db, id)?;
        let old_limit: i64 = db
            .query_row(
                "SELECT max_connections FROM providers WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .map_err(|_| "Provider not found".to_string())?;
        let gates = self
            .playback_gates
            .lock()
            .map_err(|_| "Provider limiter unavailable")?;
        if limit.is_some_and(|limit| limit != old_limit) && pools::active(&gates, pool) > 0 {
            return Err("Stop provider playback before changing max_connections".into());
        }
        let tx = db.transaction().map_err(db_error)?;
        tx.execute("UPDATE providers SET enabled=COALESCE(?2,enabled),max_connections=COALESCE(?3,max_connections),enable_live=COALESCE(?4,enable_live),enable_movies=COALESCE(?5,enable_movies),enable_series=COALESCE(?6,enable_series) WHERE id=?1", params![id,enabled,limit,live,movies,series]).map_err(db_error)?;
        if let Some(warp) = warp {
            egress::set(&tx, id, warp)?;
        }
        if let Some(limit) = limit {
            pools::set_limit(&tx, pool, limit)?;
        }
        tx.commit().map_err(db_error)?;
        db.query_row("SELECT id,name,url,username,enabled,max_connections,enable_live,enable_movies,enable_series FROM providers WHERE id=?1", [id], |r| Ok(json!({
            "id":r.get::<_,i64>(0)?,"name":r.get::<_,String>(1)?,"url":r.get::<_,String>(2)?,"warp":egress::enabled(&db,id),"enabled":r.get::<_,bool>(4)?,"max_connections":r.get::<_,i64>(5)?,"enable_live":r.get::<_,bool>(6)?,"enable_movies":r.get::<_,bool>(7)?,"enable_series":r.get::<_,bool>(8)?
        }))).map_err(db_error)
    }

    pub fn delete(&self, id: i64) -> Result<(), String> {
        let mut db = self.lock()?;
        let pool = pools::ensure(&db, id)?;
        let gates = self
            .playback_gates
            .lock()
            .map_err(|_| "Account limiter unavailable")?;
        if pools::active(&gates, pool) > 0 {
            return Err("Stop pool playback before deleting an account".into());
        }
        let tx = db.transaction().map_err(db_error)?;
        tx.execute("DELETE FROM provider_pools WHERE provider_id=?1", [id])
            .map_err(db_error)?;
        // Explicit cleanup also works when a caller has not enabled SQLite foreign keys.
        tx.execute("DELETE FROM provider_matches WHERE vod_id IN (SELECT id FROM provider_vod WHERE provider_id=?1)", [id]).map_err(db_error)?;
        tx.execute("DELETE FROM provider_cache WHERE provider_id=?1", [id])
            .map_err(db_error)?;
        tx.execute("DELETE FROM provider_live WHERE provider_id=?1", [id])
            .map_err(db_error)?;
        tx.execute("DELETE FROM provider_vod WHERE provider_id=?1", [id])
            .map_err(db_error)?;
        let deleted = tx
            .execute("DELETE FROM providers WHERE id=?1", [id])
            .map_err(db_error)?;
        if deleted == 0 {
            return Err("Provider not found".into());
        }
        tx.commit().map_err(db_error)?;
        Ok(())
    }
}
