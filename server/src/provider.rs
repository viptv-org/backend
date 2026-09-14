//! Xtream indexes are internal stream candidates, never discovery catalogs.
//! Call `init` during database startup before constructing `ProviderService`.
use base64::{engine::general_purpose::STANDARD, Engine};
use futures::{stream, StreamExt};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::time::Duration;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};
use tokio::sync::Semaphore;
use unicode_normalization::{char::is_combining_mark, UnicodeNormalization};
use url::Url;

pub(crate) mod accounts;
pub(crate) mod egress;
pub(crate) mod pools;
pub(crate) mod selection;

const MAX_RESPONSE: usize = 64 * 1024 * 1024;
const MAX_ITEMS: usize = 300_000;
// Detail discovery is demand-driven, never a full-library metadata sync.
const MAX_LAZY_DETAILS: usize = 8;

pub fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS providers (
            id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL,
            url TEXT NOT NULL, username TEXT NOT NULL, password TEXT NOT NULL,
            enabled INTEGER NOT NULL DEFAULT 1
        );
        CREATE TABLE IF NOT EXISTS provider_live (
            id TEXT PRIMARY KEY, provider_id INTEGER NOT NULL,
            stream_id TEXT NOT NULL, name TEXT NOT NULL, logo TEXT,
            category TEXT, category_id TEXT, epg_channel_id TEXT,
            FOREIGN KEY(provider_id) REFERENCES providers(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS provider_live_provider ON provider_live(provider_id);
        CREATE TABLE IF NOT EXISTS provider_vod (
            id TEXT PRIMARY KEY, provider_id INTEGER NOT NULL, stream_id TEXT NOT NULL,
            kind TEXT NOT NULL, name TEXT NOT NULL, normalized TEXT NOT NULL,
            year INTEGER, imdb_id TEXT, tmdb_id TEXT, extension TEXT NOT NULL,
            poster TEXT,
            FOREIGN KEY(provider_id) REFERENCES providers(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS provider_vod_lookup ON provider_vod(kind, normalized, year);
        CREATE INDEX IF NOT EXISTS provider_vod_provider ON provider_vod(provider_id);
        CREATE INDEX IF NOT EXISTS provider_vod_imdb ON provider_vod(imdb_id);
        CREATE INDEX IF NOT EXISTS provider_vod_tmdb ON provider_vod(tmdb_id);
        CREATE TABLE IF NOT EXISTS provider_matches (
            vod_id TEXT PRIMARY KEY, metadata_id TEXT NOT NULL, kind TEXT NOT NULL,
            FOREIGN KEY(vod_id) REFERENCES provider_vod(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS provider_matches_metadata ON provider_matches(metadata_id);
        CREATE TABLE IF NOT EXISTS provider_cache (
            provider_id INTEGER NOT NULL, cache_key TEXT NOT NULL, expires_at INTEGER NOT NULL,
            payload TEXT NOT NULL, PRIMARY KEY(provider_id,cache_key),
            FOREIGN KEY(provider_id) REFERENCES providers(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS provider_cache_expiry ON provider_cache(expires_at);",
    )?;
    let has_limit = db
        .prepare("PRAGMA table_info(providers)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|name| name == "max_connections");
    if !has_limit {
        db.execute_batch(
            "ALTER TABLE providers ADD COLUMN max_connections INTEGER NOT NULL DEFAULT 1;",
        )?;
    }
    for column in ["enable_live", "enable_movies", "enable_series"] {
        let exists = db
            .prepare("PRAGMA table_info(providers)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .iter()
            .any(|name| name == column);
        if !exists {
            db.execute_batch(&format!(
                "ALTER TABLE providers ADD COLUMN {column} INTEGER NOT NULL DEFAULT 1;"
            ))?;
        }
    }
    egress::init(db)?;
    crate::live_policy::init(db)?;
    pools::init(db)?;
    selection::init(db)?;
    crate::activity::init(db)?;
    crate::health::init(db)?;
    crate::guides::init(db)?;
    crate::lineup::init(db)
}

#[derive(Clone)]
pub struct ProviderService {
    pub db: Arc<Mutex<Connection>>,
    pub client: reqwest::Client,
    pub semaphore: Arc<Semaphore>,
    playback_gates: Arc<Mutex<HashMap<i64, PlaybackGate>>>,
}
struct PlaybackGate {
    issued: u64,
    report_generation: u64,
    semaphore: Arc<Semaphore>,
}

#[derive(Clone)]
struct Provider {
    id: i64,
    name: String,
    url: String,
    username: String,
    password: String,
}

impl Provider {
    fn ensure_current(&self, db: &Connection) -> Result<(), String> {
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
fn provider_row(db: &Connection, id: i64) -> Result<Provider, String> {
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

#[derive(Clone, Debug)]
struct Candidate {
    id: String,
    provider_id: i64,
    stream_id: String,
    kind: String,
    name: String,
    normalized: String,
    year: Option<i64>,
    imdb_id: Option<String>,
    tmdb_id: Option<String>,
    extension: String,
    poster: Option<String>,
    override_id: Option<String>,
}

// Both candidate queries select the same leading columns in the same order.
fn candidate_row(
    r: &rusqlite::Row<'_>,
    override_id: Option<String>,
) -> rusqlite::Result<Candidate> {
    Ok(Candidate {
        id: r.get(0)?,
        provider_id: r.get(1)?,
        stream_id: r.get(2)?,
        kind: r.get(3)?,
        name: r.get(4)?,
        normalized: r.get(5)?,
        year: r.get(6)?,
        imdb_id: r.get(7)?,
        tmdb_id: r.get(8)?,
        extension: r.get(9)?,
        poster: r.get(10)?,
        override_id,
    })
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

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, String> {
        self.db
            .lock()
            .map_err(|_| "Provider database is unavailable".into())
    }

    fn provider(&self, id: i64) -> Result<Provider, String> {
        let db = self.lock()?;
        provider_row(&db, id)
    }

    fn scopes(&self, id: i64) -> Result<[bool; 3], String> {
        self.lock()?.query_row("SELECT enable_live,enable_movies,enable_series FROM providers WHERE id=?1 AND enabled=1", [id], |r| Ok([r.get(0)?,r.get(1)?,r.get(2)?])).map_err(|_| "Provider not found or disabled".into())
    }

    fn provider_for_kind(&self, id: i64, kind: &str) -> Result<Provider, String> {
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

    async fn api(
        &self,
        provider: &Provider,
        action: &str,
        extra: &[(&str, &str)],
    ) -> Result<Value, String> {
        self.api_bounded(provider, action, extra, MAX_RESPONSE)
            .await
    }
    async fn api_bounded(
        &self,
        provider: &Provider,
        action: &str,
        extra: &[(&str, &str)],
        response_limit: usize,
    ) -> Result<Value, String> {
        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|_| "Provider service is shutting down".to_string())?;
        let id = provider.id;
        let kind = action_kind(action);
        self.blocking(move |s| s.provider_for_kind(id, kind))
            .await?;
        let mut url = endpoint(provider, "player_api.php")?;
        url.query_pairs_mut()
            .append_pair("username", &provider.username)
            .append_pair("password", &provider.password)
            .append_pair("action", action)
            .extend_pairs(extra.iter().copied());
        // Never format reqwest errors: they can contain the credential-bearing URL.
        let proxy = egress::proxy(&*self.lock()?, provider.id)?;
        let client = if proxy.is_some() {
            egress::builder(
                reqwest::Client::builder()
                    .connect_timeout(Duration::from_secs(5))
                    .timeout(Duration::from_secs(30)),
                proxy.as_deref(),
            )?
            .build()
            .map_err(|_| "Provider route unavailable")?
        } else {
            self.client.clone()
        };
        let mut response = client
            .get(url)
            .send()
            .await
            .map_err(|_| "Provider request failed or timed out".to_string())?;
        if !response.status().is_success() {
            return Err(format!(
                "Provider returned HTTP {}",
                response.status().as_u16()
            ));
        }
        if response
            .content_length()
            .is_some_and(|n| n > response_limit as u64)
        {
            return Err("Provider response is too large".into());
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "Provider response could not be read".to_string())?
        {
            if body.len().saturating_add(chunk.len()) > response_limit {
                return Err("Provider response is too large".into());
            }
            body.extend_from_slice(&chunk);
        }
        tokio::task::spawn_blocking(move || {
            serde_json::from_slice(&body).map_err(|_| "Provider returned invalid JSON".to_string())
        })
        .await
        .map_err(|_| "Provider parser stopped".to_string())?
    }

    async fn cached_api(
        &self,
        provider: &Provider,
        action: &str,
        item_id: &str,
    ) -> Result<Value, String> {
        use rusqlite::OptionalExtension;
        let key = format!("{action}:{item_id}");
        let now = crate::util::now();
        let provider_id = provider.id;
        let kind = action_kind(action);
        let cache_key = key.clone();
        let snapshot = provider.clone();
        let cached = self.blocking(move |s| {
            s.provider_for_kind(provider_id, kind)?;
            let db=s.lock()?;
            snapshot.ensure_current(&db)?;
            let cached: Option<String> = db.query_row(
                "SELECT payload FROM provider_cache WHERE provider_id=?1 AND cache_key=?2 AND expires_at>?3",
                params![provider_id,cache_key,now], |r|r.get(0)
            ).optional().map_err(db_error)?;
            Ok(cached.and_then(|body| serde_json::from_str::<Value>(&body).ok()))
        }).await?;
        if let Some(value) = cached {
            return Ok(value);
        }
        let (parameter, field, ttl) = if action == "get_series_info" {
            ("series_id", "episodes", 300)
        } else if action == "get_vod_info" {
            ("vod_id", "info", 300)
        } else {
            ("stream_id", "epg_listings", 60)
        };
        let mut extra = vec![(parameter, item_id)];
        if action == "get_short_epg" {
            extra.push(("limit", "100"));
        }
        let value = self.api(provider, action, &extra).await?;
        if !value
            .get(field)
            .is_some_and(|v| v.is_array() || v.is_object())
        {
            return Err("Provider returned invalid episode or guide data".into());
        }
        let snapshot = provider.clone();
        // Cache failures must not hide successfully fetched streams. Deleted providers
        // cannot be resurrected by an in-flight fetch, even with foreign keys disabled.
        self.blocking(move |s| {
            s.provider_for_kind(provider_id, kind)?;
            let payload = value.to_string();
            let db=s.lock()?;
            snapshot.ensure_current(&db)?;
            {
                let _ = db.execute("DELETE FROM provider_cache WHERE expires_at<=?1", [now]);
                let _=db.execute("INSERT INTO provider_cache(provider_id,cache_key,expires_at,payload)
                    SELECT ?1,?2,?3,?4 WHERE EXISTS(SELECT 1 FROM providers WHERE id=?1 AND enabled=1 AND CASE ?5 WHEN 'live' THEN enable_live WHEN 'movie' THEN enable_movies WHEN 'series' THEN enable_series ELSE 0 END=1)
                    ON CONFLICT(provider_id,cache_key) DO UPDATE SET expires_at=excluded.expires_at,payload=excluded.payload",
                    params![provider_id,key,crate::util::now()+ttl,payload,kind]);
            }
            Ok(value)
        }).await
    }

    /// Fetch everything before starting a transaction: failed syncs preserve the old index.
    pub async fn sync(&self, id: i64) -> Result<Value, String> {
        self.sync_with_guard(id, None).await
    }
    pub(crate) async fn sync_catalog(
        &self,
        id: i64,
        guard: crate::automation::CatalogLease,
    ) -> Result<Value, String> {
        self.sync_with_guard(id, Some(guard)).await
    }
    async fn sync_with_guard(
        &self,
        id: i64,
        guard: Option<crate::automation::CatalogLease>,
    ) -> Result<Value, String> {
        let access = guard.clone();
        let provider = self
            .blocking(move |s| {
                if let Some(guard) = access {
                    guard.validate(&*s.lock()?)?;
                }
                s.provider(id)
            })
            .await?;
        if guard.is_some() {
            accounts::login_report(self,&json!({"url":provider.url,"username":provider.username,"password":provider.password})).await?;
        }
        let scopes = self.blocking(move |s| s.scopes(id)).await?;
        let actions = [
            "get_live_categories",
            "get_live_streams",
            "get_vod_streams",
            "get_series",
        ];
        let enabled = [scopes[0], scopes[0], scopes[1], scopes[2]];
        let automated = guard.is_some();
        let fetch = |i: usize| {
            let provider = &provider;
            async move {
                if !enabled[i] {
                    return Ok(Value::Null);
                }
                let value = self
                    .api_bounded(
                        provider,
                        actions[i],
                        &[],
                        if automated {
                            16 * 1024 * 1024
                        } else {
                            MAX_RESPONSE
                        },
                    )
                    .await?;
                array(&value)?;
                Ok::<_, String>(value)
            }
        };
        let index = if automated {
            // Bound simultaneous response buffers within each automated account.
            [
                fetch(0).await?,
                fetch(1).await?,
                fetch(2).await?,
                fetch(3).await?,
            ]
        } else {
            let (a, b, c, d) = tokio::try_join!(fetch(0), fetch(1), fetch(2), fetch(3))?;
            [a, b, c, d]
        };
        self.blocking(move |s| s.store_index_guarded(id, index, Some(&provider), guard.as_ref()))
            .await
    }

    #[cfg(test)]
    fn store_index(
        &self,
        id: i64,
        categories: Value,
        live: Value,
        movies: Value,
        series: Value,
    ) -> Result<Value, String> {
        self.store_index_guarded(id, [categories, live, movies, series], None, None)
    }
    fn store_index_guarded(
        &self,
        id: i64,
        index: [Value; 4],
        expected: Option<&Provider>,
        guard: Option<&crate::automation::CatalogLease>,
    ) -> Result<Value, String> {
        let [categories, live, movies, series] = index;
        // Null means deliberately not fetched, not an empty index. Preserve that scope.
        let fetched = [!live.is_null(), !movies.is_null(), !series.is_null()];
        let empty = json!([]);
        let categories = array(if categories.is_null() {
            &empty
        } else {
            &categories
        })?;
        let live = array(if live.is_null() { &empty } else { &live })?;
        let movies = array(if movies.is_null() { &empty } else { &movies })?;
        let series = array(if series.is_null() { &empty } else { &series })?;
        let category_names: HashMap<String, String> = categories
            .iter()
            .filter_map(|v| Some((scalar(v.get("category_id")?)?, text(v, "category_name")?)))
            .collect();
        if guard.is_some()
            && (category_names.len() != categories.len()
                || live.iter().any(|v| {
                    v.get("category_id")
                        .and_then(scalar)
                        .filter(|id| !id.is_empty() && id != "0")
                        .is_some_and(|id| !category_names.contains_key(&id))
                }))
        {
            return Err("Provider index contains invalid entries".into());
        }
        let mut candidates = Vec::new();
        for (items, kind) in [(movies, "movie"), (series, "series")] {
            for value in items {
                if let Some(candidate) = candidate_from_json(id, kind, value) {
                    candidates.push(candidate);
                }
            }
        }
        // An authenticated-looking error object or malformed rows must not erase good data.
        if candidates.len() != movies.len() + series.len()
            || live
                .iter()
                .any(|v| stream_id(v, "stream_id").is_none() || text(v, "name").is_none())
        {
            return Err("Provider index contains invalid entries".into());
        }
        let mut db = self.lock()?;
        if let Some(expected) = expected {
            expected.ensure_current(&db)?;
        }
        if let Some(guard) = guard {
            guard.validate(&db)?;
            if fetched[0] && categories.is_empty() {
                let had_categories:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM provider_live WHERE provider_id=?1 AND category_id IS NOT NULL AND category_id NOT IN ('','0'))",[id],|r|r.get(0)).map_err(db_error)?;
                if had_categories {
                    return Err(
                        "Provider returned an empty catalog; previous metadata retained".into(),
                    );
                }
            }
            for (scope, items) in [("live", live), ("movie", movies), ("series", series)] {
                let enabled = match scope {
                    "live" => fetched[0],
                    "movie" => fetched[1],
                    _ => fetched[2],
                };
                if enabled && items.is_empty() {
                    let prior:bool=if scope=="live" {db.query_row("SELECT EXISTS(SELECT 1 FROM provider_live WHERE provider_id=?1)",[id],|r|r.get(0))}else{db.query_row("SELECT EXISTS(SELECT 1 FROM provider_vod WHERE provider_id=?1 AND kind=?2)",params![id,scope],|r|r.get(0))}.map_err(db_error)?;
                    if prior {
                        return Err(
                            "Provider returned an empty catalog; previous metadata retained".into(),
                        );
                    }
                }
            }
        }
        let tx = db.transaction().map_err(db_error)?;
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM providers WHERE id=?1 AND enabled=1)",
                [id],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        if !exists {
            return Err("Provider was deleted or disabled during sync".into());
        }
        let scopes: [bool; 3] = tx
            .query_row(
                "SELECT enable_live,enable_movies,enable_series FROM providers WHERE id=?1",
                [id],
                |r| Ok([r.get(0)?, r.get(1)?, r.get(2)?]),
            )
            .map_err(db_error)?;
        let active = [
            fetched[0] && scopes[0],
            fetched[1] && scopes[1],
            fetched[2] && scopes[2],
        ];
        tx.execute("DELETE FROM provider_cache WHERE provider_id=?1 AND ((?2 AND cache_key LIKE 'get_short_epg:%') OR (?3 AND cache_key LIKE 'get_vod_info:%') OR (?4 AND cache_key LIKE 'get_series_info:%'))", params![id,active[0],active[1],active[2]]).map_err(db_error)?;
        if active[0] {
            tx.execute("DELETE FROM provider_live WHERE provider_id=?1", [id])
                .map_err(db_error)?;
            let mut stmt = tx.prepare("INSERT INTO provider_live(id,provider_id,stream_id,name,logo,category,category_id,epg_channel_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)").map_err(db_error)?;
            for value in live {
                let stream = stream_id(value, "stream_id").ok_or("Invalid live stream ID")?;
                let category_id = value.get("category_id").and_then(scalar);
                let category = category_id
                    .as_ref()
                    .and_then(|c| category_names.get(c))
                    .cloned()
                    .or_else(|| category_id.clone());
                stmt.execute(params![
                    format!("iptv:{id}:{stream}"),
                    id,
                    stream,
                    text(value, "name"),
                    text(value, "stream_icon"),
                    category,
                    category_id,
                    text(value, "epg_channel_id")
                ])
                .map_err(db_error)?;
            }
        }
        // UPSERT instead of REPLACE preserves manual overrides for stable candidate IDs.
        let mut current = HashSet::new();
        {
            let mut stmt = tx.prepare("INSERT INTO provider_vod(id,provider_id,stream_id,kind,name,normalized,year,imdb_id,tmdb_id,extension,poster)
                VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
                ON CONFLICT(id) DO UPDATE SET name=excluded.name,normalized=excluded.normalized,
                year=excluded.year,imdb_id=excluded.imdb_id,tmdb_id=excluded.tmdb_id,
                extension=excluded.extension,poster=excluded.poster").map_err(db_error)?;
            for c in &candidates {
                if !(if c.kind == "movie" {
                    active[1]
                } else {
                    active[2]
                }) {
                    continue;
                }
                current.insert(c.id.clone());
                stmt.execute(params![
                    c.id,
                    c.provider_id,
                    c.stream_id,
                    c.kind,
                    c.name,
                    c.normalized,
                    c.year,
                    c.imdb_id,
                    c.tmdb_id,
                    c.extension,
                    c.poster
                ])
                .map_err(db_error)?;
            }
        }
        let stale: Vec<String> = {
            let mut stmt = tx
                .prepare("SELECT id FROM provider_vod WHERE provider_id=?1 AND ((kind='movie' AND ?2) OR (kind='series' AND ?3))")
                .map_err(db_error)?;
            let ids = stmt
                .query_map(params![id, active[1], active[2]], |r| r.get::<_, String>(0))
                .map_err(db_error)?;
            ids.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(db_error)?
                .into_iter()
                .filter(|s| !current.contains(s))
                .collect()
        };
        for stale_id in stale {
            tx.execute("DELETE FROM provider_matches WHERE vod_id=?1", [&stale_id])
                .map_err(db_error)?;
            tx.execute("DELETE FROM provider_vod WHERE id=?1", [&stale_id])
                .map_err(db_error)?;
        }
        if let Some(guard) = guard {
            guard.validate(&tx)?;
        }
        tx.commit().map_err(db_error)?;
        Ok(
            json!({"provider_id":id,"live":if active[0] {live.len()} else {0},"vod":if active[1] {movies.len()} else {0},"series":if active[2] {series.len()} else {0}}),
        )
    }

    /// Categories come from the full enabled live inventory, never the first channel page.
    /// The opaque `category:` prefix selects the exact canonical display group; legacy
    /// unprefixed category-name/category-id queries remain compatible.
    pub fn live_categories(&self, offset: usize, limit: usize) -> Result<Value, String> {
        let db = self.lock()?;
        if crate::lineup::enabled(&db)? {
            return crate::lineup::categories(&db, offset, limit);
        }
        let groups = "SELECT COALESCE(NULLIF(TRIM(l.category),''),NULLIF(TRIM(l.category_id),''),'') AS category_name, COUNT(*) AS channel_count
            FROM provider_live l JOIN providers p ON p.id=l.provider_id
            WHERE p.enabled=1 AND p.enable_live=1 GROUP BY category_name";
        let total: i64 = db
            .query_row(&format!("SELECT COUNT(*) FROM ({groups})"), [], |r| {
                r.get(0)
            })
            .map_err(db_error)?;
        let mut stmt = db
            .prepare(&format!("SELECT category_name,channel_count FROM ({groups}) ORDER BY category_name COLLATE NOCASE,category_name LIMIT ?1 OFFSET ?2"))
            .map_err(db_error)?;
        let rows = stmt
            .query_map(params![limit.min(100) as i64, offset.min(i64::MAX as usize) as i64], |r| {
                let name: String = r.get(0)?;
                let count: i64 = r.get(1)?;
                Ok(json!({"id":format!("category:{name}"),"name":if name.is_empty() {"Uncategorized"} else {&name},"count":count}))
            })
            .map_err(db_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db_error)?;
        Ok(json!({"categories":rows,"total":total}))
    }

    pub fn live(
        &self,
        category: Option<String>,
        search: Option<String>,
        offset: usize,
        limit: usize,
    ) -> Result<Value, String> {
        let db = self.lock()?;
        if crate::lineup::enabled(&db)? {
            return crate::lineup::live(&db, category, search, offset, limit);
        }
        let search = search
            .filter(|s| !s.trim().is_empty())
            .map(|s| format!("%{}%", escape_like(s.trim())));
        let category = category.filter(|s| !s.is_empty());
        let exact_category = category
            .as_deref()
            .and_then(|s| s.strip_prefix("category:"))
            .map(str::to_owned);
        let category = if exact_category.is_some() {
            None
        } else {
            category
        };
        let filter = "FROM provider_live l JOIN providers p ON p.id=l.provider_id WHERE p.enabled=1 AND p.enable_live=1
            AND (?1 IS NULL OR l.category=?1 OR l.category_id=?1)
            AND (?2 IS NULL OR l.name LIKE ?2 ESCAPE '\\')
            AND (?3 IS NULL OR COALESCE(NULLIF(TRIM(l.category),''),NULLIF(TRIM(l.category_id),''),'')=?3)";
        let total: i64 = db
            .query_row(
                &format!("SELECT count(*) {filter}"),
                params![category, search, exact_category],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        let mut stmt = db.prepare(&format!("SELECT l.id,l.name,l.logo,l.category,l.epg_channel_id {filter} ORDER BY l.name COLLATE NOCASE,l.id LIMIT ?4 OFFSET ?5")).map_err(db_error)?;
        let rows = stmt.query_map(params![category,search,exact_category,limit.min(500) as i64,offset.min(i64::MAX as usize) as i64], |r| Ok(json!({
            "id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"logo":r.get::<_,Option<String>>(2)?,
            "category":r.get::<_,Option<String>>(3)?,"epg_channel_id":r.get::<_,Option<String>>(4)?
        }))).map_err(db_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
        Ok(json!({"channels":rows,"total":total}))
    }

    fn channel(&self, id: &str) -> Result<(Provider, String), String> {
        let id = crate::lineup::source(&*self.lock()?, id)?;
        self.raw_channel(&id)
    }

    fn raw_channel(&self, id: &str) -> Result<(Provider, String), String> {
        let (provider_id, stream): (i64, String) = self
            .lock()?
            .query_row(
                "SELECT provider_id,stream_id FROM provider_live WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| "Live channel not found".to_string())?;
        Ok((self.provider_for_kind(provider_id, "live")?, stream))
    }

    /// Compatibility admission without a content kind. Stored-source playback must use
    /// `acquire_playback_for_kind` before ffprobe and hold its permit until teardown.
    /// Fail fast rather than creating an unbounded playback queue.
    pub async fn acquire_playback(
        &self,
        provider_id: i64,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, String> {
        self.acquire_playback_scoped(provider_id, None).await
    }

    /// Admit a stored source using its trusted discovery kind, not caller-supplied media hints.
    /// Rechecks current scopes even if the opaque source was registered before an admin update.
    pub async fn acquire_playback_for_kind(
        &self,
        id: i64,
        kind: &str,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, String> {
        if !matches!(kind, "live" | "movie" | "series") {
            return Err("Invalid provider playback kind".into());
        }
        self.acquire_playback_scoped(id, Some(kind.to_owned()))
            .await
    }

    async fn acquire_playback_scoped(
        &self,
        provider_id: i64,
        kind: Option<String>,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, String> {
        self.blocking(move |s| {
            // Check after the blocking-worker queue, under the same DB lock as admission.
            // Shared lock order with update: DB then limiter. No stale-limit/scope race.
            let db = s.lock()?;
            let allowed:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM providers WHERE id=?1 AND enabled=1 AND (?2 IS NULL OR CASE ?2 WHEN 'live' THEN enable_live WHEN 'movie' THEN enable_movies WHEN 'series' THEN enable_series ELSE 0 END=1))",params![provider_id,kind],|r|r.get(0)).map_err(db_error)?;
            if !allowed {return Err("Provider not found or disabled".into());}
            let mut gates=s.playback_gates.lock().map_err(|_|"Provider limiter unavailable")?;
            pools::acquire(&db,&mut gates,provider_id)
        })
        .await
    }

    pub fn channel_source(&self, id: &str) -> Result<(String, i64), String> {
        let (provider, stream) = self.channel(id)?;
        Ok((media_url(&provider, "live", &stream, "ts")?, provider.id))
    }

    pub fn family_candidate_source(
        &self,
        channel: &str,
        candidate: &str,
    ) -> Result<(String, i64), String> {
        if !crate::lineup::eligible(&*self.lock()?, channel, candidate)? {
            return Err("Candidate unavailable or requires verification".into());
        }
        let (provider, stream) = self.raw_channel(candidate)?;
        Ok((media_url(&provider, "live", &stream, "ts")?, provider.id))
    }

    pub(crate) fn probe_source(&self, id: &str) -> Result<(String, i64), String> {
        let (provider, stream) = self.raw_channel(id)?;
        Ok((media_url(&provider, "live", &stream, "ts")?, provider.id))
    }

    pub fn channel_url(&self, id: &str) -> Result<String, String> {
        let (provider, stream) = self.channel(id)?;
        media_url(&provider, "live", &stream, "ts")
    }

    pub async fn guide(&self, channel_id: String) -> Result<Value, String> {
        {
            let db = self.lock()?;
            if let Some(id) = crate::lineup::family_id(&db, &channel_id)? {
                return crate::guides::read(&db, &id).map_err(|e| e.1);
            }
        }
        let lookup_id = channel_id.clone();
        let (provider, stream) = self.blocking(move |s| s.channel(&lookup_id)).await?;
        let result = self.cached_api(&provider, "get_short_epg", &stream).await?;
        let listings = result
            .get("epg_listings")
            .and_then(Value::as_array)
            .ok_or("Provider returned invalid guide data")?;
        let programs: Vec<Value> = listings.iter().take(100).filter_map(|item| {
            let start = timestamp(item.get("start_timestamp")?)?;
            let end = item.get("stop_timestamp").or_else(|| item.get("end_timestamp")).and_then(timestamp)?;
            if end <= start { return None; }
            Some(json!({"id":item.get("id").and_then(scalar).unwrap_or_else(|| format!("{channel_id}:{start}")),
                "title":decode_epg(item.get("title")), "description":decode_epg(item.get("description")),
                "start":start,"end":end}))
        }).collect();
        Ok(json!({"programs":programs}))
    }

    fn candidates(&self, kind: Option<&str>) -> Result<Vec<Candidate>, String> {
        self.candidates_filtered(kind, None)
    }

    fn candidates_filtered(
        &self,
        kind: Option<&str>,
        request: Option<&MatchRequest>,
    ) -> Result<Vec<Candidate>, String> {
        let db = self.lock()?;
        let ids = request.map(|r| json!(r.ids).to_string());
        let title = request.and_then(|r| r.normalized.as_deref());
        let year = request.and_then(|r| r.year);
        // Each UNION arm uses its lookup index, avoiding materializing entire IPTV libraries.
        let mut stmt = db.prepare("SELECT v.id,v.provider_id,v.stream_id,v.kind,v.name,v.normalized,v.year,v.imdb_id,v.tmdb_id,v.extension,v.poster,m.metadata_id
            FROM provider_vod v JOIN providers p ON p.id=v.provider_id
            LEFT JOIN provider_matches m ON m.vod_id=v.id AND m.kind=v.kind
            WHERE p.enabled=1 AND ((v.kind='movie' AND p.enable_movies=1) OR (v.kind='series' AND p.enable_series=1)) AND (?1 IS NULL OR v.kind=?1) AND (?2 IS NULL OR v.id IN (
                SELECT id FROM provider_vod WHERE imdb_id IN (SELECT value FROM json_each(?2))
                UNION SELECT id FROM provider_vod WHERE tmdb_id IN (SELECT value FROM json_each(?2))
                UNION SELECT vod_id FROM provider_matches WHERE metadata_id IN (SELECT value FROM json_each(?2))
                UNION SELECT id FROM provider_vod WHERE kind=?1 AND normalized=?3 AND year=?4
            )) ORDER BY v.provider_id,v.id").map_err(db_error)?;
        let rows = stmt
            .query_map(params![kind, ids, title, year], |r| {
                candidate_row(r, r.get(11)?)
            })
            .map_err(db_error)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)
    }

    fn sparse_candidates(
        &self,
        kind: &str,
        request: &MatchRequest,
        only_provider: Option<i64>,
    ) -> Result<Vec<Candidate>, String> {
        let Some(title) = request.normalized.as_deref() else {
            return Ok(Vec::new());
        };
        let db = self.lock()?;
        // Indexed exact title lookup, bounded before materialization. Never use a LIKE scan.
        let mut stmt = db.prepare("SELECT v.id,v.provider_id,v.stream_id,v.kind,v.name,v.normalized,v.year,v.imdb_id,v.tmdb_id,v.extension,v.poster
            FROM provider_vod v JOIN providers p ON p.id=v.provider_id
            WHERE v.kind=?1 AND v.normalized=?2 AND p.enabled=1 AND ((v.kind='movie' AND p.enable_movies=1) OR (v.kind='series' AND p.enable_series=1))
            AND (v.year IS NULL OR (?3 IS NULL AND (v.imdb_id IS NULL OR v.tmdb_id IS NULL)))
            AND NOT EXISTS(SELECT 1 FROM provider_matches m WHERE m.vod_id=v.id)
            AND (?5 IS NULL OR v.provider_id=?5)
            LIMIT ?4").map_err(db_error)?;
        let rows = stmt
            .query_map(
                params![
                    kind,
                    title,
                    request.year,
                    MAX_LAZY_DETAILS as i64,
                    only_provider
                ],
                |r| candidate_row(r, None),
            )
            .map_err(db_error)?;
        Ok(rows
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db_error)?
            .into_iter()
            .filter(|c| !evidence_conflicts(c, request))
            .collect())
    }

    async fn enrich_candidate(&self, c: Candidate) -> Result<Option<Candidate>, String> {
        let provider_id = c.provider_id;
        let kind = c.kind.clone();
        let provider = self
            .blocking(move |s| s.provider_for_kind(provider_id, &kind))
            .await?;
        let action = if c.kind == "movie" {
            "get_vod_info"
        } else {
            "get_series_info"
        };
        let details = self.cached_api(&provider, action, &c.stream_id).await?;
        let Some(enriched) = validated_details(&c, &details) else {
            return Ok(None);
        };
        self.blocking(move |s| {
            // Compare-and-set: do not overwrite a concurrent sync, edit, or manual mapping.
            let changed = s
                .lock()?
                .execute(
                    "UPDATE provider_vod SET year=?1,imdb_id=?2,tmdb_id=?3
                WHERE id=?4 AND normalized=?5 AND year IS ?6 AND imdb_id IS ?7 AND tmdb_id IS ?8
                AND EXISTS(SELECT 1 FROM providers p WHERE p.id=provider_id AND p.enabled=1 AND ((provider_vod.kind='movie' AND p.enable_movies=1) OR (provider_vod.kind='series' AND p.enable_series=1)))
                AND NOT EXISTS(SELECT 1 FROM provider_matches m WHERE m.vod_id=provider_vod.id)",
                    params![
                        enriched.year,
                        enriched.imdb_id,
                        enriched.tmdb_id,
                        c.id,
                        c.normalized,
                        c.year,
                        c.imdb_id,
                        c.tmdb_id
                    ],
                )
                .map_err(db_error)?;
            Ok((changed == 1).then_some(enriched))
        })
        .await
    }

    pub fn matches(&self) -> Result<Value, String> {
        Ok(Value::Array(self.candidates(None)?.into_iter()
            .filter(|c| c.override_id.is_none() && c.imdb_id.is_none() && c.tmdb_id.is_none())
            .map(|c| json!({"vod_id":c.id,"provider_id":c.provider_id,"type":c.kind,"name":c.name,"year":c.year,"poster":c.poster})).collect()))
    }

    pub fn override_match(&self, value: Value) -> Result<(), String> {
        let vod_id = required_string(&value, "vod_id", 256)?;
        let metadata_id = required_string(&value, "metadata_id", 256)?;
        if metadata_id.chars().any(char::is_whitespace) {
            return Err("Invalid metadata ID".into());
        }
        let kind = required_string(&value, "type", 16)?;
        if kind != "movie" && kind != "series" {
            return Err("Match type must be movie or series".into());
        }
        let db = self.lock()?;
        let valid: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM provider_vod WHERE id=?1 AND kind=?2)",
                params![vod_id, kind],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        if !valid {
            return Err("Stream candidate not found for this type".into());
        }
        db.execute(
            "INSERT INTO provider_matches(vod_id,metadata_id,kind) VALUES(?1,?2,?3)
            ON CONFLICT(vod_id) DO UPDATE SET metadata_id=excluded.metadata_id,kind=excluded.kind",
            params![vod_id, canonical_id(&metadata_id), kind],
        )
        .map_err(db_error)?;
        Ok(())
    }

    /// Compatibility collector; production discovery publishes each bounded batch immediately.
    pub async fn streams(&self, request: Value) -> Result<Vec<Value>, String> {
        let mut streams = Vec::new();
        let mut error = None;
        self.stream_batches(request, |_, result| match result {
            Ok(batch) => streams.extend(batch),
            Err(e) => error = Some(e),
        })
        .await?;
        if streams.is_empty() {
            if let Some(error) = error {
                return Err(error);
            }
        }
        let mut seen = HashSet::new();
        streams.retain(|s| seen.insert(s["url"].as_str().unwrap_or_default().to_owned()));
        Ok(streams)
    }

    /// At most 100 candidates, eight concurrent candidate lookups, four network requests.
    /// Each timeout covers its own candidate including queued permits, never earlier results.
    pub async fn stream_batches(
        &self,
        request: Value,
        mut publish: impl FnMut(String, Result<Vec<Value>, String>) + Send,
    ) -> Result<(), String> {
        let kind = request
            .get("type")
            .and_then(Value::as_str)
            .ok_or("Missing stream type")?;
        if kind == "live" {
            let id = required_string(&request, "id", 256)?;
            let (provider, stream) = self.blocking(move |s| s.channel(&id)).await?;
            publish(
                format!("iptv:{}", provider.id),
                Ok(vec![
                    json!({"url":media_url(&provider,"live",&stream,"ts")?,"name":provider.name,"source":format!("iptv:{}",provider.id)}),
                ]),
            );
            return Ok(());
        }
        if kind != "movie" && kind != "series" {
            return Ok(());
        }
        let only_provider = request
            .get("only_provider_id")
            .map(|value| {
                value
                    .as_i64()
                    .filter(|id| *id > 0)
                    .ok_or("Invalid provider scope")
            })
            .transpose()?;
        let parsed = MatchRequest::parse(&request, kind)?;
        if kind == "series" && (parsed.season.is_none() || parsed.episode.is_none()) {
            return Err("Series streams require season and episode".into());
        }
        let season = parsed.season.unwrap_or(0);
        let episode = parsed.episode.unwrap_or(0);
        let kind = kind.to_owned();
        let matching = parsed.clone();
        let chosen = self
            .blocking(move |s| {
                let candidates: Vec<_> = s
                    .candidates_filtered(Some(&kind), Some(&parsed))?
                    .into_iter()
                    .filter(|c| only_provider.is_none_or(|id| c.provider_id == id))
                    .collect();
                // Round robin providers so duplicates from one cannot occupy every work slot.
                let mut groups =
                    std::collections::BTreeMap::<i64, std::collections::VecDeque<Candidate>>::new();
                for c in select_candidates(&candidates, &parsed) {
                    groups
                        .entry(c.provider_id)
                        .or_default()
                        .push_back(c.clone());
                }
                let mut chosen = Vec::new();
                while chosen.len() < 100 {
                    let before = chosen.len();
                    for group in groups.values_mut() {
                        if chosen.len() == 100 {
                            break;
                        }
                        if let Some(c) = group.pop_front() {
                            chosen.push(c);
                        }
                    }
                    if chosen.len() == before {
                        break;
                    }
                }
                let selected: HashSet<_> = chosen.iter().map(|c| c.id.clone()).collect();
                let sparse = s.sparse_candidates(&kind, &parsed, only_provider)?;
                let mut work: Vec<_> = chosen.into_iter().map(|c| (c, false)).collect();
                let remaining = 100 - work.len();
                work.extend(
                    sparse
                        .into_iter()
                        .filter(|c| {
                            only_provider.is_none_or(|id| c.provider_id == id)
                                && !selected.contains(&c.id)
                        })
                        .take(remaining)
                        .map(|c| (c, true)),
                );
                Ok(work)
            })
            .await?;
        let mut results = stream::iter(chosen.into_iter().map(|(c, lazy)| {
            let service = self.clone();
            let matching = &matching;
            async move {
                let source = format!("iptv:{}", c.provider_id);
                let result = tokio::time::timeout(Duration::from_secs(30), async move {
                    let c = if lazy {
                        let Some(c) = service.enrich_candidate(c).await? else {
                            return Ok(Vec::new());
                        };
                        if evidence_conflicts(&c, matching)
                            || select_candidates(std::slice::from_ref(&c), matching).is_empty()
                        {
                            return Ok(Vec::new());
                        }
                        c
                    } else {
                        c
                    };
                    service.resolve_candidate(c, season, episode).await
                })
                .await
                .unwrap_or_else(|_| Err("IPTV candidate timed out".into()));
                (source, result)
            }
        }))
        .buffer_unordered(8);
        while let Some((source, result)) = results.next().await {
            publish(source, result);
        }
        Ok(())
    }

    async fn resolve_candidate(
        &self,
        c: Candidate,
        season: i64,
        episode: i64,
    ) -> Result<Vec<Value>, String> {
        let provider_id = c.provider_id;
        let kind = c.kind.clone();
        let provider = self
            .blocking(move |s| s.provider_for_kind(provider_id, &kind))
            .await?;
        if c.kind == "movie" {
            return Ok(vec![
                json!({"url":media_url(&provider,"movie",&c.stream_id,&c.extension)?,"name":format!("{} · {}",provider.name,c.name),"source":format!("iptv:{}",provider.id)}),
            ]);
        }
        let info = self
            .cached_api(&provider, "get_series_info", &c.stream_id)
            .await?;
        let mut streams = Vec::new();
        for item in episode_rows(&info, season, episode).into_iter().take(100) {
            let Some(id) = stream_id(item, "id") else {
                continue;
            };
            let ext = episode_extension(item.get("container_extension"));
            streams.push(json!({"url":media_url(&provider,"series",&id,&ext)?,"name":format!("{} · {} S{:02}E{:02}",provider.name,c.name,season,episode),"source":format!("iptv:{}",provider.id)}));
        }
        Ok(streams)
    }
}

fn db_error(_: rusqlite::Error) -> String {
    "Provider database operation failed".into()
}

fn optional_bool(value: &Value, key: &str) -> Result<Option<bool>, String> {
    value
        .get(key)
        .map(|v| v.as_bool().ok_or_else(|| format!("{key} must be boolean")))
        .transpose()
}

fn action_kind(action: &str) -> &'static str {
    match action {
        "get_vod_streams" | "get_vod_info" => "movie",
        "get_series" | "get_series_info" => "series",
        _ => "live",
    }
}

fn required_string(value: &Value, key: &str, max: usize) -> Result<String, String> {
    let s = value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Missing {key}"))?;
    if s.trim().is_empty() || s.len() > max || s.chars().any(char::is_control) {
        return Err(format!("Invalid {key}"));
    }
    Ok(s.to_owned())
}

fn base_url(raw: &str) -> Result<Url, String> {
    // Validation errors are deliberately redacted, too: user input can contain secrets.
    let mut url =
        crate::util::validate_url(raw).map_err(|_| "Invalid provider HTTP(S) URL".to_string())?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Provider URL must not contain credentials, a query, or a fragment".into());
    }
    let path = url
        .path()
        .trim_end_matches('/')
        .trim_end_matches("/player_api.php")
        .to_owned();
    url.set_path(&format!("{path}/"));
    Ok(url)
}

fn endpoint(provider: &Provider, endpoint: &str) -> Result<Url, String> {
    base_url(&provider.url)?
        .join(endpoint)
        .map_err(|_| "Invalid provider URL".into())
}

fn media_url(provider: &Provider, kind: &str, id: &str, ext: &str) -> Result<String, String> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return Err("Invalid provider stream ID".into());
    }
    let mut url = base_url(&provider.url)?;
    {
        let mut path = url
            .path_segments_mut()
            .map_err(|_| "Invalid provider URL".to_string())?;
        path.pop_if_empty()
            .push(kind)
            .push(&provider.username)
            .push(&provider.password)
            .push(&format!("{id}.{}", extension(Some(ext))));
    }
    Ok(url.to_string())
}

fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}
fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_owned()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}
fn stream_id(value: &Value, key: &str) -> Option<String> {
    scalar(value.get(key)?)
        .filter(|s| !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_digit()))
}
fn array(value: &Value) -> Result<&Vec<Value>, String> {
    let result = value
        .as_array()
        .ok_or("Provider returned an invalid index (check credentials)")?;
    if result.len() > MAX_ITEMS {
        return Err("Provider index exceeds the item limit".into());
    }
    Ok(result)
}
fn episode_extension(value: Option<&Value>) -> String {
    // Xtream episodes without a declared container use the transport route. This
    // does not change the trusted series kind or infer the media's actual container.
    match value {
        None | Some(Value::Null) => "ts".into(),
        Some(Value::String(value)) if value.trim().is_empty() => "ts".into(),
        _ => extension(value.and_then(Value::as_str)),
    }
}

fn extension(value: Option<&str>) -> String {
    let ext = value
        .unwrap_or("mp4")
        .trim()
        .trim_start_matches('.')
        .to_ascii_lowercase();
    if !ext.is_empty() && ext.len() <= 10 && ext.bytes().all(|b| b.is_ascii_alphanumeric()) {
        ext
    } else {
        "mp4".into()
    }
}
fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}
fn timestamp(v: &Value) -> Option<i64> {
    scalar(v)?.parse::<i64>().ok().filter(|n| *n >= 0)
}
fn decode_epg(v: Option<&Value>) -> String {
    let s = v.and_then(Value::as_str).unwrap_or_default();
    STANDARD
        .decode(s)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .filter(|s| {
            !s.chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t' && c != '\r')
        })
        .unwrap_or_else(|| s.to_owned())
}
fn valid_year(v: &Value) -> Option<i64> {
    let s = scalar(v)?;
    let year: i64 = s.get(..4)?.parse().ok()?;
    // Only a standalone year or a recognizable release date, never arbitrary digits.
    if s.len() != 4 && !s.get(4..5).is_some_and(|c| c == "-") {
        return None;
    }
    (1870..=2200).contains(&year).then_some(year)
}

/// Remove only a trailing year. Do not guess away language, resolution, or editions.
fn title_year(name: &str) -> (String, Option<i64>) {
    let name = name.trim();
    for (open, close) in [('(', ')'), ('[', ']')] {
        if name.ends_with(close) {
            if let Some(start) = name.rfind(open) {
                let year_text = &name[start + 1..name.len() - 1];
                if year_text.len() == 4 {
                    if let Some(year) = valid_year(&Value::String(year_text.to_string())) {
                        return (name[..start].trim().to_owned(), Some(year));
                    }
                }
            }
        }
    }
    if let Some((title, last)) = name.rsplit_once(' ') {
        if !title.trim().is_empty() && last.len() == 4 {
            if let Some(year) = valid_year(&Value::String(last.to_owned())) {
                return (title.trim().to_owned(), Some(year));
            }
        }
    }
    (name.to_owned(), None)
}
fn normalize(name: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for c in name
        .nfkd()
        .filter(|c| !is_combining_mark(*c))
        .flat_map(char::to_lowercase)
    {
        if c.is_alphanumeric() {
            if space && !out.is_empty() {
                out.push(' ');
            }
            out.push(c);
            space = false;
        } else {
            space = true;
        }
    }
    out
}
fn imdb_id(v: Option<&Value>) -> Option<String> {
    let s = scalar(v?)?.to_ascii_lowercase();
    let digits = s.strip_prefix("imdb:").unwrap_or(&s).strip_prefix("tt")?;
    if (5..=12).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit()) {
        Some(format!("tt{digits}"))
    } else {
        None
    }
}
fn tmdb_id(v: Option<&Value>) -> Option<String> {
    let s = scalar(v?)?;
    let s = s.strip_prefix("tmdb:").unwrap_or(&s);
    if s.is_empty() || s.len() > 12 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let number = s.parse::<u64>().ok()?;
    (number > 0).then(|| format!("tmdb:{number}"))
}
fn canonical_id(s: &str) -> String {
    let value = Value::String(s.trim().to_owned());
    imdb_id(Some(&value))
        .or_else(|| {
            s.trim()
                .starts_with("tmdb:")
                .then(|| tmdb_id(Some(&value)))
                .flatten()
        })
        .unwrap_or_else(|| s.trim().to_owned())
}
fn candidate_from_json(provider_id: i64, kind: &str, value: &Value) -> Option<Candidate> {
    let stream_id = stream_id(
        value,
        if kind == "series" {
            "series_id"
        } else {
            "stream_id"
        },
    )?;
    let name = value.get("name")?.as_str()?.trim();
    let (title, suffix_year) = title_year(name);
    // Keep the provider title (not the display fallback) as the matching input.
    let name = if name.is_empty() {
        format!("Untitled {kind} #{stream_id}")
    } else {
        name.to_owned()
    };
    let year = ["year", "releaseDate", "release_date"]
        .iter()
        .find_map(|k| value.get(*k).and_then(valid_year))
        .or(suffix_year);
    Some(Candidate {
        id: format!("iptv:{provider_id}:{kind}:{stream_id}"),
        provider_id,
        stream_id,
        kind: kind.into(),
        name,
        normalized: normalize(&title),
        year,
        imdb_id: imdb_id(value.get("imdb_id")).or_else(|| imdb_id(value.get("imdb"))),
        tmdb_id: tmdb_id(value.get("tmdb_id")).or_else(|| tmdb_id(value.get("tmdb"))),
        extension: extension(value.get("container_extension").and_then(Value::as_str)),
        poster: text(value, "stream_icon").or_else(|| text(value, "cover")),
        override_id: None,
    })
}

// Lazy discovery must not use an ID match to erase contradictory year/namespace evidence.
fn evidence_conflicts(c: &Candidate, r: &MatchRequest) -> bool {
    c.year.zip(r.year).is_some_and(|(a, b)| a != b)
        || [(&c.imdb_id, "tt"), (&c.tmdb_id, "tmdb:")]
            .iter()
            .any(|(id, prefix)| {
                id.as_ref().is_some_and(|id| {
                    r.ids.iter().any(|v| v.starts_with(prefix)) && !r.ids.contains(id)
                })
            })
}

fn validated_details(c: &Candidate, details: &Value) -> Option<Candidate> {
    let info = details.get("info")?.as_object()?;
    if details.get("movie_data").is_some_and(|v| !v.is_object()) {
        return None;
    }
    let mut enriched = c.clone();
    let mut named = false;
    for object in [
        Some(info),
        details.get("movie_data").and_then(Value::as_object),
    ]
    .into_iter()
    .flatten()
    {
        for key in ["stream_id", "vod_id", "series_id"] {
            if let Some(value) = object.get(key) {
                if scalar(value).as_deref() != Some(c.stream_id.as_str()) {
                    return None;
                }
            }
        }
        for key in ["name", "title"] {
            if let Some(value) = object.get(key).filter(|v| !v.is_null()) {
                let name = value.as_str()?.trim();
                if name.is_empty() {
                    continue;
                }
                let (title, year) = title_year(name);
                if normalize(&title) != c.normalized || c.normalized.is_empty() {
                    return None;
                }
                named = true;
                if let Some(year) = year {
                    if enriched.year.is_some_and(|old| old != year) {
                        return None;
                    }
                    enriched.year = Some(year);
                }
            }
        }
        for key in ["year", "releasedate", "releaseDate", "release_date"] {
            if let Some(value) = object
                .get(key)
                .filter(|v| !v.is_null() && v.as_str() != Some(""))
            {
                let raw = scalar(value)?;
                if raw.len() != 4 {
                    let bytes = raw.as_bytes();
                    if bytes.len() != 10
                        || bytes[4] != b'-'
                        || bytes[7] != b'-'
                        || !bytes
                            .iter()
                            .enumerate()
                            .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
                        || !(1..=12).contains(&raw[5..7].parse::<u32>().ok()?)
                        || !(1..=31).contains(&raw[8..10].parse::<u32>().ok()?)
                    {
                        return None;
                    }
                }
                let year = valid_year(value)?;
                if enriched.year.is_some_and(|old| old != year) {
                    return None;
                }
                enriched.year = Some(year);
            }
        }
        for (keys, target, parse) in [
            (
                ["imdb_id", "imdb"],
                &mut enriched.imdb_id,
                imdb_id as fn(Option<&Value>) -> Option<String>,
            ),
            (
                ["tmdb_id", "tmdb"],
                &mut enriched.tmdb_id,
                tmdb_id as fn(Option<&Value>) -> Option<String>,
            ),
        ] {
            for key in keys {
                if let Some(value) = object
                    .get(key)
                    .filter(|v| !v.is_null() && v.as_str() != Some(""))
                {
                    let id = parse(Some(value))?;
                    if target.as_ref().is_some_and(|old| old != &id) {
                        return None;
                    }
                    *target = Some(id);
                }
            }
        }
    }
    named.then_some(enriched)
}

#[derive(Clone)]
struct MatchRequest {
    ids: HashSet<String>,
    normalized: Option<String>,
    year: Option<i64>,
    season: Option<i64>,
    episode: Option<i64>,
}
impl MatchRequest {
    fn parse(value: &Value, kind: &str) -> Result<Self, String> {
        let mut id = required_string(value, "id", 256)?;
        let number = |key: &str| -> Result<Option<i64>, String> {
            match value.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(v) => timestamp(v)
                    .map(Some)
                    .ok_or_else(|| format!("Invalid {key}")),
            }
        };
        let mut season = number("season")?;
        let mut episode = number("episode")?;
        if kind == "series" {
            // Strip only two numeric suffixes, preserving namespaced IDs such as tmdb:123.
            let pieces: Vec<&str> = id.rsplitn(3, ':').collect();
            if pieces.len() == 3 {
                if let (Ok(e), Ok(s)) = (pieces[0].parse::<i64>(), pieces[1].parse::<i64>()) {
                    if s < 0 || e < 0 {
                        return Err("Invalid season or episode".into());
                    }
                    if season.is_some_and(|v| v != s) || episode.is_some_and(|v| v != e) {
                        return Err("Episode ID conflicts with season or episode".into());
                    }
                    season = Some(s);
                    episode = Some(e);
                    id = pieces[2].to_owned();
                }
            }
        }
        let mut ids = HashSet::from([canonical_id(&id)]);
        if let Some(id) = imdb_id(value.get("imdb_id")) {
            ids.insert(id);
        }
        if let Some(id) = tmdb_id(value.get("tmdb_id")) {
            ids.insert(id);
        }
        let title = text(value, "name").map(|s| title_year(&s));
        let year = value
            .get("year")
            .and_then(valid_year)
            .or_else(|| title.as_ref().and_then(|t| t.1));
        let normalized = title.map(|t| normalize(&t.0)).filter(|t| !t.is_empty());
        Ok(Self {
            ids,
            normalized,
            year,
            season,
            episode,
        })
    }
}

fn select_candidates<'a>(
    candidates: &'a [Candidate],
    request: &MatchRequest,
) -> Vec<&'a Candidate> {
    candidates
        .iter()
        .filter(|c| {
            // A manual mapping is authoritative, including when it rules a candidate out.
            if let Some(id) = &c.override_id {
                return request.ids.contains(id);
            }
            if c.imdb_id
                .as_ref()
                .is_some_and(|id| request.ids.contains(id))
                || c.tmdb_id
                    .as_ref()
                    .is_some_and(|id| request.ids.contains(id))
            {
                return true;
            }
            // Do not override contradictory IDs from the same metadata namespace with a title.
            if c.imdb_id.is_some() && request.ids.iter().any(|id| id.starts_with("tt")) {
                return false;
            }
            if c.tmdb_id.is_some() && request.ids.iter().any(|id| id.starts_with("tmdb:")) {
                return false;
            }
            request.year.is_some()
                && request.year == c.year
                && request
                    .normalized
                    .as_ref()
                    .is_some_and(|n| n == &c.normalized)
        })
        .collect()
}

fn episode_rows(info: &Value, season: i64, episode: i64) -> Vec<&Value> {
    let Some(episodes) = info.get("episodes") else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    if let Some(seasons) = episodes.as_object() {
        if let Some(items) = seasons.get(&season.to_string()).and_then(Value::as_array) {
            rows.extend(items.iter().filter(|v| {
                v.get("episode_num").and_then(timestamp) == Some(episode)
                    && v.get("season").is_none_or(|s| timestamp(s) == Some(season))
            }));
        }
    } else if let Some(items) = episodes.as_array() {
        rows.extend(items.iter().filter(|v| {
            v.get("season").and_then(timestamp) == Some(season)
                && v.get("episode_num").and_then(timestamp) == Some(episode)
        }));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service() -> ProviderService {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        init(&db).unwrap();
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .unwrap();
        ProviderService::new(Arc::new(Mutex::new(db)), client)
    }
    fn add_provider(s: &ProviderService) -> i64 {
        s.add(json!({"name":"Test IPTV","url":"https://example.com/base/","username":"user","password":"SUPER_SECRET"})).unwrap()["id"].as_i64().unwrap()
    }
    fn insert_candidate(s: &ProviderService, provider: i64, stream: &str, kind: &str) -> String {
        let id = format!("iptv:{provider}:{kind}:{stream}");
        s.lock().unwrap().execute("INSERT INTO provider_vod(id,provider_id,stream_id,kind,name,normalized,year,extension) VALUES(?1,?2,?3,?4,'Amélie (2001)','amelie',2001,'mkv')",params![id,provider,stream,kind]).unwrap();
        id
    }
    fn request(v: Value) -> MatchRequest {
        MatchRequest::parse(&v, v["type"].as_str().unwrap_or("movie")).unwrap()
    }
    fn candidate(v: Value) -> Candidate {
        candidate_from_json(1, "movie", &v).unwrap()
    }

    #[test]
    fn scope_migration_defaults_and_api_validation_preserve_settings() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE providers(id INTEGER PRIMARY KEY,name TEXT NOT NULL,url TEXT NOT NULL,username TEXT NOT NULL,password TEXT NOT NULL,enabled INTEGER NOT NULL DEFAULT 1);
            INSERT INTO providers VALUES(1,'Legacy','https://example.com','u','p',1);").unwrap();
        init(&db).unwrap();
        init(&db).unwrap();
        let s = ProviderService::new(Arc::new(Mutex::new(db)), reqwest::Client::new());
        assert_eq!(s.scopes(1).unwrap(), [true, true, true]);
        assert_eq!(s.list().unwrap()[0]["enable_live"], true);
        let changed = s
            .update(1, json!({"enable_live":false,"enable_series":false}))
            .unwrap();
        assert_eq!(changed["enable_live"], false);
        assert_eq!(changed["enable_movies"], true);
        s.update(1, json!({"max_connections":2})).unwrap();
        assert_eq!(s.scopes(1).unwrap(), [false, true, false]);
        init(&s.lock().unwrap()).unwrap();
        assert_eq!(s.scopes(1).unwrap(), [false, true, false]);
        for field in ["enable_live", "enable_movies", "enable_series"] {
            for invalid in [json!("false"), json!(0), Value::Null, json!([])] {
                assert!(s.update(1, json!({field:invalid.clone()})).is_err());
                let mut add =
                    json!({"name":"Bad","url":"https://example.com","username":"u","password":"p"});
                add[field] = invalid;
                assert!(s.add(add).is_err());
            }
        }
        assert_eq!(s.scopes(1).unwrap(), [false, true, false]);
        let added = s.add(json!({"name":"New","url":"https://example.com","username":"new-u","password":"p","enable_live":false})).unwrap();
        assert_eq!(added["enable_live"], false);
        assert_eq!(added["enable_movies"], true);
        assert_eq!(added["enable_series"], true);
    }

    #[tokio::test]
    async fn scopes_hide_queens_live_but_allow_both_vod_providers() {
        let s = service();
        let queens = add_provider(&s);
        let other = s.add(json!({"name":"Other live provider","url":"https://other.example.com","username":"u","password":"p"})).unwrap()["id"].as_i64().unwrap();
        for p in [queens, other] {
            s.store_index(
                p,
                json!([]),
                json!([{"stream_id":11,"name":"World News"}]),
                json!([{"stream_id":2318,"name":"Inception (2010)"}]),
                json!([{"series_id":9562,"name":"Breaking Bad (2008)"}]),
            )
            .unwrap();
        }
        s.update(queens, json!({"enable_live":false})).unwrap();
        let hidden = format!("iptv:{queens}:11");
        let visible = format!("iptv:{other}:11");
        assert_eq!(s.live(None, None, 0, 100).unwrap()["total"], 1);
        assert_eq!(
            s.live(None, Some("World".into()), 0, 100).unwrap()["channels"][0]["id"],
            visible
        );
        assert!(s.channel_source(&hidden).is_err());
        assert!(s.channel_url(&hidden).is_err());
        assert!(s.guide(hidden.clone()).await.is_err());
        assert!(s.streams(json!({"type":"live","id":hidden})).await.is_err());
        assert!(s.channel_source(&visible).is_ok());
        assert_eq!(
            s.streams(json!({"type":"movie","id":"tt1375666","name":"Inception","year":2010}))
                .await
                .unwrap()
                .len(),
            2
        );
        let req = request(
            json!({"type":"series","id":"tt0903747:1:1","name":"Breaking Bad","year":2008}),
        );
        assert_eq!(
            s.candidates_filtered(Some("series"), Some(&req))
                .unwrap()
                .len(),
            2
        );
        let queued = s
            .candidates(Some("movie"))
            .unwrap()
            .into_iter()
            .find(|c| c.provider_id == queens)
            .unwrap();
        s.update(queens, json!({"enable_movies":false,"enable_series":false}))
            .unwrap();
        assert!(s.resolve_candidate(queued.clone(), 0, 0).await.is_err());
        assert!(s.enrich_candidate(queued).await.is_err());
        assert_eq!(s.candidates(Some("movie")).unwrap().len(), 1);
        assert_eq!(
            s.candidates_filtered(Some("series"), Some(&req))
                .unwrap()
                .len(),
            1
        );
        // Even a sync result already in flight cannot delete disabled scopes or manual mappings.
        s.override_match(json!({"type":"movie","vod_id":format!("iptv:{queens}:movie:2318"),"metadata_id":"tt1375666"})).unwrap();
        s.store_index(queens, json!([]), json!([]), json!([]), json!([]))
            .unwrap();
        s.update(
            queens,
            json!({"enable_live":true,"enable_movies":true,"enable_series":true}),
        )
        .unwrap();
        assert!(s.channel_source(&hidden).is_ok());
        assert_eq!(s.candidates(Some("movie")).unwrap().len(), 2);
        assert_eq!(s.candidates(Some("series")).unwrap().len(), 2);
        assert!(s
            .candidates(Some("movie"))
            .unwrap()
            .iter()
            .any(|c| c.provider_id == queens && c.override_id.as_deref() == Some("tt1375666")));
    }

    #[tokio::test]
    async fn scoped_sync_skips_live_and_queued_details_recheck_scope() {
        let (url, _, actions, task) = mock_xtream().await;
        let s = service();
        let p = s.add(json!({"name":"VOD only","url":url,"username":"u/+","password":"SECRET&?","enable_live":false})).unwrap()["id"].as_i64().unwrap();
        s.sync(p).await.unwrap();
        assert_eq!(actions.lock().unwrap().len(), 2);
        assert!(actions
            .lock()
            .unwrap()
            .iter()
            .all(|a| a == "get_vod_streams" || a == "get_series"));
        let provider = s.provider(p).unwrap();
        let permit = s.semaphore.acquire_many(4).await.unwrap();
        let mut future = Box::pin(s.cached_api(&provider, "get_vod_info", "2318"));
        assert!(tokio::time::timeout(Duration::from_millis(20), &mut future)
            .await
            .is_err());
        s.update(p, json!({"enable_movies":false})).unwrap();
        drop(permit);
        assert!(future.await.is_err());
        assert_eq!(actions.lock().unwrap().len(), 2);
        s.update(p, json!({"enable_movies":true})).unwrap();
        s.cached_api(&provider, "get_vod_info", "2318")
            .await
            .unwrap();
        s.update(p, json!({"enable_movies":false})).unwrap();
        assert!(s
            .cached_api(&provider, "get_vod_info", "2318")
            .await
            .is_err());
        s.update(p, json!({"enable_movies":true})).unwrap();
        let (other_url, _, _, other_task) = mock_xtream().await;
        let other = s
            .add(json!({"name":"Live and VOD","url":other_url,"username":"u/+","password":"SECRET&?"}))
            .unwrap()["id"]
            .as_i64()
            .unwrap();
        s.sync(other).await.unwrap();
        assert_eq!(s.live(None, None, 0, 100).unwrap()["total"], 1);
        for kind in ["movie", "series"] {
            let rows = s.candidates(Some(kind)).unwrap();
            assert_eq!(rows.len(), 2);
            for row in rows {
                assert_eq!(s.resolve_candidate(row, 1, 2).await.unwrap().len(), 1);
            }
        }
        other_task.abort();
        let _ = other_task.await;
        task.abort();
        let _ = task.await;
    }

    #[tokio::test]
    async fn stored_source_admission_rechecks_exact_kind_and_shares_permits() {
        let s = service();
        let p = add_provider(&s);
        for (kind, field) in [
            ("live", "enable_live"),
            ("movie", "enable_movies"),
            ("series", "enable_series"),
        ] {
            // A caller can hold an opaque source issued while the kind was enabled.
            let first = s.acquire_playback_for_kind(p, kind).await.unwrap();
            assert!(s.acquire_playback(p).await.is_err());
            assert!(s.acquire_playback_for_kind(p, kind).await.is_err());
            drop(first);
            s.update(p, json!({field:false})).unwrap();
            assert!(s.acquire_playback_for_kind(p, kind).await.is_err());
            let other = if kind == "movie" { "series" } else { "movie" };
            let permit = s.acquire_playback_for_kind(p, other).await.unwrap();
            drop(permit);
            s.update(p, json!({field:true})).unwrap();
            let permit = s.acquire_playback_for_kind(p, kind).await.unwrap();
            drop(permit);
        }
        for kind in ["", "movies", "Movie", "unknown"] {
            assert!(s.acquire_playback_for_kind(p, kind).await.is_err());
        }
        s.update(p, json!({"enabled":false})).unwrap();
        for kind in ["live", "movie", "series"] {
            assert!(s.acquire_playback_for_kind(p, kind).await.is_err());
        }
        s.update(p, json!({"enabled":true})).unwrap();
        let permit = s.acquire_playback_for_kind(p, "movie").await.unwrap();
        drop(permit);
        assert!(s.acquire_playback_for_kind(p + 999, "movie").await.is_err());
    }

    #[test]
    fn scoped_admission_rechecks_after_blocking_worker_queue() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let s = service();
            let p = add_provider(&s);
            let (release, blocked) = std::sync::mpsc::channel();
            let (started, ready) = tokio::sync::oneshot::channel();
            let worker = tokio::task::spawn_blocking(move || {
                let _ = started.send(());
                let _ = blocked.recv();
            });
            ready.await.unwrap();
            let mut admission = Box::pin(s.acquire_playback_for_kind(p, "movie"));
            assert!(
                tokio::time::timeout(Duration::from_millis(20), &mut admission)
                    .await
                    .is_err()
            );
            s.update(p, json!({"enable_movies":false})).unwrap();
            release.send(()).unwrap();
            worker.await.unwrap();
            assert!(admission.await.is_err());
            let permit = s.acquire_playback_for_kind(p, "series").await.unwrap();
            drop(permit);
        });
    }

    #[tokio::test]
    async fn unspecified_episode_containers_use_transport_route_not_live_kind() {
        let s = service();
        let p = add_provider(&s);
        let id = insert_candidate(&s, p, "9562", "series");
        s.override_match(json!({"vod_id":id,"type":"series","metadata_id":"opaque:series:bb"}))
            .unwrap();
        let info = json!({"episodes":{"1":[
            {"id":"2008185","episode_num":1,"container_extension":null},
            {"id":"2008186","episode_num":2},
            {"id":"2008187","episode_num":3,"container_extension":""},
            {"id":"2008188","episode_num":4,"container_extension":"  "},
            {"id":"2008189","episode_num":5,"container_extension":"mp4"},
            {"id":"2008190","episode_num":6,"container_extension":"mkv"},
            {"id":"2008191","episode_num":7,"container_extension":false}
        ]}});
        s.lock().unwrap().execute("INSERT INTO provider_cache(provider_id,cache_key,expires_at,payload) VALUES(?1,'get_series_info:9562',?2,?3)",params![p,crate::util::now()+300,info.to_string()]).unwrap();
        s.update(p, json!({"enable_live":false})).unwrap();
        for (episode, stream, ext) in [
            (1, "2008185", "ts"),
            (2, "2008186", "ts"),
            (3, "2008187", "ts"),
            (4, "2008188", "ts"),
            (5, "2008189", "mp4"),
            (6, "2008190", "mkv"),
            (7, "2008191", "mp4"),
        ] {
            let request = json!({"type":"series","id":format!("opaque:series:bb:1:{episode}")});
            let rows = s.streams(request).await.unwrap();
            assert_eq!(rows.len(), 1);
            assert!(rows[0]["url"]
                .as_str()
                .unwrap()
                .ends_with(&format!("/series/user/SUPER_SECRET/{stream}.{ext}")));
        }
        let movie =
            candidate_from_json(p, "movie", &json!({"stream_id":2318,"name":"Inception"})).unwrap();
        assert_eq!(movie.extension, "mp4");
        assert!(s.resolve_candidate(movie, 0, 0).await.unwrap()[0]["url"]
            .as_str()
            .unwrap()
            .ends_with("/movie/user/SUPER_SECRET/2318.mp4"));
        s.update(p, json!({"enable_live":true,"enable_series":false}))
            .unwrap();
        assert!(s
            .streams(json!({"type":"series","id":"opaque:series:bb:1:1"}))
            .await
            .unwrap()
            .is_empty());
        assert!(s.acquire_playback_for_kind(p, "series").await.is_err());
    }

    #[tokio::test]
    async fn configured_connection_limits_are_shared_and_release() {
        let s = service();
        let p = add_provider(&s);
        assert_eq!(s.list().unwrap()[0]["max_connections"], 1);
        let permit = s.acquire_playback(p).await.unwrap();
        assert!(s.clone().acquire_playback(p).await.is_err());
        assert!(s.update(p, json!({"max_connections":2})).is_err());
        s.update(p, json!({"enabled":false})).unwrap();
        assert!(s.acquire_playback(p).await.is_err());
        s.update(p, json!({"enabled":true})).unwrap();
        let p2 = s.add(json!({"name":"Two","url":"https://example.com","username":"u","password":"p","max_connections":2})).unwrap()["id"].as_i64().unwrap();
        let one = s.acquire_playback(p2).await.unwrap();
        let two = s.acquire_playback(p2).await.unwrap();
        assert!(s.acquire_playback(p2).await.is_err());
        drop(permit);
        assert_eq!(
            s.update(p, json!({"max_connections":2})).unwrap()["max_connections"],
            2
        );
        let resized_one = s.acquire_playback(p).await.unwrap();
        let resized_two = s.acquire_playback(p).await.unwrap();
        assert!(s.acquire_playback(p).await.is_err());
        drop((resized_one, resized_two));
        assert!(s.update(p, json!({"enabled":"false"})).is_err());
        assert!(s.update(p, json!({"unexpected":1})).is_err());
        assert!(s.update(p, json!({})).is_err());
        drop((one, two));
        s.delete(p).unwrap();
        assert!(s.acquire_playback(p).await.is_err());
        for limit in [
            json!(0),
            json!(-1),
            json!(33),
            json!(1.5),
            json!("2"),
            Value::Null,
        ] {
            assert!(s.add(json!({"name":"Bad","url":"https://example.com","username":"u","password":"p","max_connections":limit})).is_err());
        }
    }

    #[test]
    fn schema_is_idempotent_and_credentials_are_not_returned() {
        let s = service();
        init(&s.lock().unwrap()).unwrap();
        let id = add_provider(&s);
        let list = s.list().unwrap();
        assert_eq!(list[0]["id"], id);
        assert_eq!(list[0]["enabled"], true);
        assert!(list[0].get("password").is_none());
        assert!(!list.to_string().contains("SUPER_SECRET"));
        assert_eq!(s.list().unwrap()[0]["url"], "https://example.com/base");
    }
    #[test]
    fn validates_provider_fields_and_urls_without_echoing_secrets() {
        let s = service();
        for url in [
            "file:///tmp/test",
            "https://user:SUPER_SECRET@example.com",
            "https://example.com/?password=SUPER_SECRET",
            "https://example.com/#SUPER_SECRET",
        ] {
            let error = s
                .add(json!({"name":"n","url":url,"username":"u","password":"p"}))
                .unwrap_err();
            assert!(!error.contains("SUPER_SECRET"));
        }
        assert!(s
            .add(json!({"name":"","url":"https://example.com","username":"u","password":"p"}))
            .is_err());
        assert!(s
            .add(json!({"name":"n","url":"https://example.com","username":"u"}))
            .is_err());
    }
    #[test]
    fn normalization_is_conservative_and_unicode_aware() {
        assert_eq!(normalize("  Amélie: THE   Film! "), "amelie the film");
        assert_eq!(title_year("Amélie (2001)"), ("Amélie".into(), Some(2001)));
        assert_eq!(title_year("Amélie [2001]"), ("Amélie".into(), Some(2001)));
        assert_eq!(title_year("Amélie 2001"), ("Amélie".into(), Some(2001)));
        assert_eq!(title_year("2001"), ("2001".into(), None));
        assert_ne!(normalize("Film 4K"), normalize("Film"));
        assert_eq!(
            title_year("Film (Extended)"),
            ("Film (Extended)".into(), None)
        );
    }
    #[test]
    fn empty_vod_names_sync_without_synthetic_title_matches() {
        let s = service();
        let p = add_provider(&s);
        s.store_index(
            p,
            json!([]),
            json!([{"stream_id":1,"name":"Live"}]),
            json!([
                {"stream_id":10,"name":"Named (2001)"},
                {"stream_id":11,"name":"","year":2001,"imdb_id":"tt1234567","tmdb_id":42}
            ]),
            json!([
                {"series_id":20,"name":"Named series (2001)"},
                {"series_id":21,"name":" \t\n ","year":2001,"imdb":"tt7654321","tmdb":43}
            ]),
        )
        .unwrap();
        assert_eq!(s.candidates(None).unwrap().len(), 4);
        for (kind, stream, imdb, tmdb) in [
            ("movie", "11", "tt1234567", "tmdb:42"),
            ("series", "21", "tt7654321", "tmdb:43"),
        ] {
            let cs = s.candidates(Some(kind)).unwrap();
            let c = cs.iter().find(|c| c.stream_id == stream).unwrap();
            assert_eq!(c.name, format!("Untitled {kind} #{stream}"));
            assert!(c.normalized.is_empty());
            assert_eq!(c.imdb_id.as_deref(), Some(imdb));
            assert_eq!(c.tmdb_id.as_deref(), Some(tmdb));
            for id in [imdb, tmdb] {
                let req = request(json!({"id":id,"type":kind}));
                let filtered = s.candidates_filtered(Some(kind), Some(&req)).unwrap();
                assert_eq!(select_candidates(&filtered, &req).len(), 1);
            }
            for name in [c.name.as_str(), "", "!!!"] {
                let req = request(json!({"id":"unmapped","type":kind,"name":name,"year":2001}));
                assert!(select_candidates(&cs, &req).is_empty());
                assert!(s
                    .candidates_filtered(Some(kind), Some(&req))
                    .unwrap()
                    .is_empty());
            }
        }
    }

    #[test]
    fn malformed_vod_rows_still_reject_index_atomically() {
        let s = service();
        let p = add_provider(&s);
        let original = insert_candidate(&s, p, "99", "movie");
        s.store_index(
            p,
            json!([]),
            json!([{"stream_id":1,"name":"Live"}]),
            json!([{"stream_id":99,"name":"Amélie (2001)"}]),
            json!([]),
        )
        .unwrap();
        for kind in ["movie", "series"] {
            let key = if kind == "series" {
                "series_id"
            } else {
                "stream_id"
            };
            let mut invalid = vec![json!({key:10})];
            for name in [Value::Null, json!(123), json!(false), json!([]), json!({})] {
                invalid.push(json!({key:10,"name":name}));
            }
            invalid.push(json!({"name":""}));
            for id in [
                Value::Null,
                json!(""),
                json!("bad"),
                json!(-1),
                json!(1.5),
                json!(true),
                json!([]),
                json!({}),
                json!("123456789012345678901234567890123"),
            ] {
                invalid.push(json!({key:id,"name":""}));
            }
            for row in invalid {
                assert!(candidate_from_json(p, kind, &row).is_none(), "{row}");
                let rows = json!([{key:10,"name":""}, row]);
                let (movies, series) = if kind == "movie" {
                    (rows, json!([]))
                } else {
                    (json!([]), rows)
                };
                assert!(s
                    .store_index(p, json!([]), json!([]), movies, series)
                    .is_err());
                let cs = s.candidates(None).unwrap();
                assert_eq!(cs.len(), 1);
                assert_eq!(cs[0].id, original);
                let live: i64 = s
                    .lock()
                    .unwrap()
                    .query_row("SELECT COUNT(*) FROM provider_live", [], |r| r.get(0))
                    .unwrap();
                assert_eq!(live, 1);
            }
        }
        assert!(s
            .store_index(
                p,
                json!([]),
                json!([{"stream_id":1,"name":""}]),
                json!([]),
                json!([])
            )
            .is_err());
        assert_eq!(s.candidates(None).unwrap()[0].id, original);
    }

    #[test]
    fn metadata_ids_take_priority_and_conflicts_do_not_title_match() {
        let c = candidate(
            json!({"stream_id":10,"name":"Other (1999)","imdb_id":"tt1234567","tmdb":42}),
        );
        let cs = vec![c];
        assert_eq!(
            select_candidates(&cs, &request(json!({"id":"tt1234567"}))).len(),
            1
        );
        assert_eq!(
            select_candidates(&cs, &request(json!({"id":"tmdb:42"}))).len(),
            1
        );
        assert_eq!(
            select_candidates(
                &cs,
                &request(json!({"id":"tt7654321","name":"Other","year":1999}))
            )
            .len(),
            0
        );
    }
    #[test]
    fn fallback_requires_both_title_and_year() {
        let cs = vec![candidate(json!({"stream_id":"10","name":"Amélie (2001)"}))];
        assert_eq!(
            select_candidates(
                &cs,
                &request(json!({"id":"tt1234567","name":"Amelie","year":"2001"}))
            )
            .len(),
            1
        );
        for v in [
            json!({"id":"tt1234567","name":"Amelie"}),
            json!({"id":"tt1234567","name":"Amelie","year":2002}),
            json!({"id":"tt1234567","name":"Amelie 4K","year":2001}),
        ] {
            assert!(select_candidates(&cs, &request(v)).is_empty());
        }
    }
    #[test]
    fn overrides_are_authoritative_and_type_checked() {
        let s = service();
        let p = add_provider(&s);
        let id = insert_candidate(&s, p, "10", "movie");
        assert_eq!(s.matches().unwrap().as_array().unwrap().len(), 1);
        assert!(s
            .override_match(json!({"vod_id":id,"metadata_id":"tt1234567","type":"series"}))
            .is_err());
        s.override_match(json!({"vod_id":id,"metadata_id":"imdb:tt1234567","type":"movie"}))
            .unwrap();
        let cs = s.candidates(Some("movie")).unwrap();
        assert_eq!(
            select_candidates(&cs, &request(json!({"id":"tt1234567"}))).len(),
            1
        );
        assert!(select_candidates(
            &cs,
            &request(json!({"id":"tt7654321","name":"Amelie","year":2001}))
        )
        .is_empty());
        assert!(s.matches().unwrap().as_array().unwrap().is_empty());
    }
    #[test]
    fn deleting_provider_removes_all_owned_rows() {
        let s = service();
        let p = add_provider(&s);
        let id = insert_candidate(&s, p, "10", "movie");
        s.override_match(json!({"vod_id":id,"metadata_id":"tt1234567","type":"movie"}))
            .unwrap();
        s.delete(p).unwrap();
        assert_eq!(s.list().unwrap(), json!([]));
        assert_eq!(s.matches().unwrap(), json!([]));
        assert_eq!(
            s.lock()
                .unwrap()
                .query_row("SELECT count(*) FROM provider_matches", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert!(s.delete(p).is_err());
    }
    #[test]
    fn live_filter_escapes_wildcards_and_paginates() {
        let s = service();
        let p = add_provider(&s);
        for (id, name) in [(1, "News 100%"), (2, "News Other"), (3, "Sports")] {
            s.lock().unwrap().execute("INSERT INTO provider_live(id,provider_id,stream_id,name,category,category_id) VALUES(?1,?2,?3,?4,'General','7')",params![format!("iptv:{p}:{id}"),p,id.to_string(),name]).unwrap();
        }
        let data = s.live(None, Some("%".into()), 0, 100).unwrap();
        assert_eq!(data["total"], 1);
        assert_eq!(data["channels"][0]["name"], "News 100%");
        let data = s.live(Some("7".into()), None, 1, 1).unwrap();
        assert_eq!(data["total"], 3);
        assert_eq!(data["channels"].as_array().unwrap().len(), 1);
        assert!(s.channel_url("iptv:999:1").is_err());
        assert!(s
            .channel_url(&format!("iptv:{p}:1"))
            .unwrap()
            .ends_with("/live/user/SUPER_SECRET/1.ts"));
    }
    #[test]
    fn live_categories_use_complete_scoped_groups_and_exact_ids() {
        let s = service();
        let first = add_provider(&s);
        let second = s.add(json!({"name":"Second","url":"https://second.example.com/base","username":"user","password":"SUPER_SECRET"})).unwrap()["id"].as_i64().unwrap();
        let hidden = s.add(json!({"name":"hidden","url":"https://hidden.example.com/base","username":"user","password":"SUPER_SECRET"})).unwrap()["id"].as_i64().unwrap();
        let disabled = s.add(json!({"name":"disabled","url":"https://disabled.example.com/base","username":"user","password":"SUPER_SECRET"})).unwrap()["id"].as_i64().unwrap();
        {
            let db = s.lock().unwrap();
            db.execute("UPDATE providers SET enable_live=0 WHERE id=?1", [hidden])
                .unwrap();
            db.execute("UPDATE providers SET enabled=0 WHERE id=?1", [disabled])
                .unwrap();
            for (provider, stream, category, category_id) in [
                (first, 1, Some("News"), Some("7")),
                (first, 2, Some(" News "), Some("7")),
                (second, 3, Some("News"), Some("91")),
                (first, 4, Some("7"), Some("99")),
                (first, 5, None, None),
                (first, 6, Some(""), Some("29")),
                (first, 7, Some("Actualités / UK"), Some("30")),
                (hidden, 8, Some("Hidden"), Some("8")),
                (disabled, 9, Some("Disabled"), Some("9")),
            ] {
                db.execute("INSERT INTO provider_live(id,provider_id,stream_id,name,category,category_id) VALUES(?1,?2,?3,?4,?5,?6)",
                    params![format!("iptv:{provider}:{stream}"),provider,stream.to_string(),format!("Channel {stream}"),category,category_id]).unwrap();
            }
        }
        let categories = s.live_categories(0, 100).unwrap();
        assert_eq!(categories["total"], 5);
        let rows = categories["categories"].as_array().unwrap();
        for category in rows {
            let channels = s
                .live(Some(category["id"].as_str().unwrap().into()), None, 0, 100)
                .unwrap();
            assert_eq!(channels["total"], category["count"]);
        }
        let news = rows.iter().find(|r| r["name"] == "News").unwrap();
        assert_eq!(news["count"], 3);
        assert_eq!(
            s.live(Some("category:7".into()), None, 0, 100).unwrap()["total"],
            1
        );
        assert_eq!(
            s.live(Some("category:".into()), None, 0, 100).unwrap()["total"],
            1
        );
        assert_eq!(
            s.live(Some("category:Actualités / UK".into()), None, 0, 100)
                .unwrap()["total"],
            1
        );
        // Existing clients can still filter by upstream category ID.
        assert_eq!(s.live(Some("7".into()), None, 0, 100).unwrap()["total"], 3);
        assert_eq!(
            s.live(
                Some("category:News".into()),
                Some("Channel 3".into()),
                0,
                100
            )
            .unwrap()["total"],
            1
        );
    }

    #[test]
    fn live_category_pagination_is_stable_and_capped() {
        let s = service();
        let provider = add_provider(&s);
        {
            let db = s.lock().unwrap();
            for i in 0..105 {
                db.execute("INSERT INTO provider_live(id,provider_id,stream_id,name,category) VALUES(?1,?2,?3,'Channel',?4)",
                    params![format!("iptv:{provider}:{i}"),provider,i.to_string(),format!("Category {i:03}")]).unwrap();
            }
        }
        let all = s.live_categories(0, usize::MAX).unwrap();
        assert_eq!(all["total"], 105);
        assert_eq!(all["categories"].as_array().unwrap().len(), 100);
        let page = s.live_categories(100, 20).unwrap();
        assert_eq!(page["total"], 105);
        assert_eq!(page["categories"].as_array().unwrap().len(), 5);
        assert_eq!(page["categories"][0]["id"], "category:Category 100");
        assert_eq!(page["categories"][4]["name"], "Category 104");
        assert!(s.live_categories(usize::MAX, 100).unwrap()["categories"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn credential_path_segments_are_encoded() {
        let p = Provider {
            id: 1,
            name: "p".into(),
            url: "https://example.com/prefix/player_api.php".into(),
            username: "a/b".into(),
            password: "p?# /".into(),
        };
        let url = media_url(&p, "movie", "12", "mkv").unwrap();
        assert_eq!(
            url,
            "https://example.com/prefix/movie/a%2Fb/p%3F%23%20%2F/12.mkv"
        );
        assert!(media_url(&p, "movie", "../evil", "mp4").is_err());
        assert_eq!(extension(Some("../../evil")), "mp4");
    }
    #[test]
    fn series_episode_ids_support_namespaces_and_reject_conflicts() {
        let r = request(json!({"type":"series","id":"tmdb:42:0:3"}));
        assert!(r.ids.contains("tmdb:42"));
        assert_eq!((r.season, r.episode), (Some(0), Some(3)));
        assert!(MatchRequest::parse(&json!({"id":"tt1234567:1:2","season":3}), "series").is_err());
        assert!(MatchRequest::parse(&json!({"id":"tt1234567:1:2","season":-1}), "series").is_err());
        assert!(
            MatchRequest::parse(&json!({"id":"tt1234567","episode":"invalid"}), "series").is_err()
        );
        let r = request(json!({"type":"series","id":"tmdb:42","season":1,"episode":2}));
        assert!(r.ids.contains("tmdb:42"));
    }
    #[test]
    fn lazy_episode_selection_uses_numbers_not_array_position() {
        let v = json!({"episodes":{"1":[{"id":91,"episode_num":2,"season":1},{"id":90,"episode_num":1},{"id":92,"episode_num":2,"season":2}],"2":[{"id":100,"episode_num":2}]}});
        let rows = episode_rows(&v, 1, 2);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["id"], 91);
        assert!(episode_rows(&v, 3, 1).is_empty());
        let v = json!({"episodes":[{"id":5,"episode_num":"3","season":"0"}]});
        assert_eq!(episode_rows(&v, 0, 3).len(), 1);
    }
    #[test]
    fn epg_decoding_and_timestamps_are_safe() {
        assert_eq!(decode_epg(Some(&json!("TmV3cw=="))), "News");
        assert_eq!(decode_epg(Some(&json!("Breaking news!"))), "Breaking news!");
        assert_eq!(decode_epg(None), "");
        assert_eq!(timestamp(&json!("1700000000")), Some(1700000000));
        assert_eq!(timestamp(&json!(-1)), None);
    }
    async fn mock_xtream() -> (
        String,
        Arc<std::sync::atomic::AtomicBool>,
        Arc<Mutex<Vec<String>>>,
        tokio::task::JoinHandle<()>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let fail = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let actions = Arc::new(Mutex::new(Vec::new()));
        let fail_task = fail.clone();
        let actions_task = actions.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0u8; 4096];
                loop {
                    let n = socket.read(&mut buffer).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                    if bytes.windows(4).any(|b| b == b"\r\n\r\n") || bytes.len() > 16384 {
                        break;
                    }
                }
                // Clients can abandon an unused/speculative connection without
                // sending a request. EOF is not a malformed HTTP request.
                if bytes.is_empty() {
                    continue;
                }
                let request = String::from_utf8_lossy(&bytes);
                let path = request
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap();
                let url = Url::parse(&format!("http://localhost{path}")).unwrap();
                let query: HashMap<String, String> = url.query_pairs().into_owned().collect();
                assert_eq!(query.get("username").map(String::as_str), Some("u/+"));
                assert_eq!(query.get("password").map(String::as_str), Some("SECRET&?"));
                let action = query.get("action").unwrap().clone();
                actions_task.lock().unwrap().push(action.clone());
                let response=match action.as_str() {
                    "get_live_categories"=>json!([{"category_id":"7","category_name":"News"}]),
                    "get_live_streams"=>json!([{"stream_id":11,"name":"World News","category_id":"7","epg_channel_id":"world"}]),
                    "get_vod_streams" if fail_task.load(std::sync::atomic::Ordering::SeqCst)=>json!({"error":"SECRET&? invalid credentials"}),
                    "get_vod_streams"=>json!([{"stream_id":22,"name":"Amélie (2001)","container_extension":"mkv"}]),
                    "get_series"=>json!([{"series_id":33,"name":"Example (2020)","imdb_id":"tt1234567"}]),
                    "get_vod_info"=> {
                        assert!(!query.contains_key("stream_id"));
                        match query["vod_id"].as_str() {
                            "2318" => json!({"info":{"name":"Inception","releasedate":"2010-07-15","tmdb_id":"27205"},"movie_data":{"name":"Inception","container_extension":"mp4"}}),
                            "4000" => json!({"info":{"name":"Other Film","releasedate":"2010-07-15"}}),
                            "4001" => json!({"info":{"name":"Inception","releasedate":"2010-bogus"}}),
                            "4002" => json!({"info":{"name":"Inception","releasedate":"2010-07-15","imdb_id":"tt9999999"}}),
                            "4003" => json!({"error":"failed"}),
                            _ => json!({"info":{"name":"Inception"}}),
                        }
                    },
                    "get_series_info" if query["series_id"] == "9562" => json!({"info":{"name":"Breaking Bad","releaseDate":"2008-01-20"},"episodes":{"1":[{"id":"942671","title":"Pilot","season":1,"episode_num":1,"container_extension":"mp4"}]}}),
                    "get_series_info"=> { assert_eq!(query["series_id"],"33"); json!({"episodes":{"1":[{"id":44,"episode_num":2,"container_extension":"mp4"}]}}) },
                    "get_short_epg"=> { assert_eq!(query["stream_id"],"11"); json!({"epg_listings":[{"id":"epg1","title":"TmV3cw==","description":"SGVsbG8=","start_timestamp":"1700000000","stop_timestamp":"1700003600"},{"id":"bad","start_timestamp":5,"stop_timestamp":3}]}) },
                    _=>panic!("Unexpected Xtream action"),
                }.to_string();
                let response=format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",response.len(),response);
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        (format!("http://{address}"), fail, actions, task)
    }

    #[tokio::test]
    async fn mock_xtream_survives_abandoned_connection() {
        let (url, _, actions, task) = mock_xtream().await;
        let socket = tokio::net::TcpStream::connect(url.trim_start_matches("http://"))
            .await
            .unwrap();
        drop(socket);
        let response = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
            .get(format!("{url}/player_api.php"))
            .query(&[
                ("username", "u/+"),
                ("password", "SECRET&?"),
                ("action", "get_live_categories"),
            ])
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        assert_eq!(actions.lock().unwrap().as_slice(), ["get_live_categories"]);
        task.abort();
    }

    fn sparse_row(s: &ProviderService, p: i64, id: &str, kind: &str, name: &str) -> String {
        let id = insert_candidate(s, p, id, kind);
        s.lock().unwrap().execute("UPDATE provider_vod SET name=?1,normalized=?2,year=NULL,extension='mp4' WHERE id=?3", params![name,normalize(name),id]).unwrap();
        id
    }

    #[tokio::test]
    async fn lazy_queens_details_match_sparse_movies_and_series() {
        let (url, _, actions, task) = mock_xtream().await;
        let s = service();
        let p = s
            .add(json!({"name":"Queens fixture","url":url,"username":"u/+","password":"SECRET&?"}))
            .unwrap()["id"]
            .as_i64()
            .unwrap();
        let id = sparse_row(&s, p, "2318", "movie", "Inception");
        let req = json!({"type":"movie","id":"tt1375666","name":"Inception","year":2010});
        assert!(s
            .candidates_filtered(Some("movie"), Some(&request(req.clone())))
            .unwrap()
            .is_empty());
        // Details are fetched but a wrong requested year cannot become an ID shortcut.
        assert!(s
            .streams(json!({"type":"movie","id":"tmdb:27205","name":"Inception","year":2011}))
            .await
            .unwrap()
            .is_empty());
        let streams = s.streams(req.clone()).await.unwrap();
        assert_eq!(streams.len(), 1);
        assert!(streams[0]["url"].as_str().unwrap().ends_with("/2318.mp4"));
        let row = s.candidates(Some("movie")).unwrap().remove(0);
        assert_eq!(row.year, Some(2010));
        assert_eq!(row.tmdb_id.as_deref(), Some("tmdb:27205"));
        assert!(row.imdb_id.is_none()); // Never manufacture an IMDb/TMDB crosswalk.
        assert_eq!(
            s.streams(json!({"type":"movie","id":"tt1375666","tmdb_id":"27205"}))
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(s
            .streams(json!({"type":"movie","id":"tmdb:999","name":"Inception","year":2010}))
            .await
            .unwrap()
            .is_empty());
        // Simulate index refresh losing metadata: the detail cache still prevents another fetch.
        s.lock()
            .unwrap()
            .execute(
                "UPDATE provider_vod SET year=NULL,tmdb_id=NULL WHERE id=?1",
                [&id],
            )
            .unwrap();
        assert_eq!(s.streams(req.clone()).await.unwrap().len(), 1);
        assert_eq!(
            actions
                .lock()
                .unwrap()
                .iter()
                .filter(|a| *a == "get_vod_info")
                .count(),
            1
        );
        s.override_match(json!({"vod_id":id,"metadata_id":"tt9999999","type":"movie"}))
            .unwrap();
        assert!(s.streams(req).await.unwrap().is_empty());
        sparse_row(&s, p, "9562", "series", "Breaking Bad");
        let req = json!({"type":"series","id":"tt0903747:1:1","name":"Breaking Bad","year":2008});
        let streams = s.streams(req.clone()).await.unwrap();
        assert_eq!(streams.len(), 1);
        assert!(streams[0]["url"].as_str().unwrap().ends_with("/942671.mp4"));
        assert_eq!(s.streams(req).await.unwrap().len(), 1);
        assert_eq!(
            actions
                .lock()
                .unwrap()
                .iter()
                .filter(|a| *a == "get_series_info")
                .count(),
            1
        );
        task.abort();
        let _ = task.await;
    }

    #[test]
    fn lazy_detail_validation_rejects_mismatches_and_preserves_known_evidence() {
        let c = candidate(json!({"stream_id":2318,"name":"Inception"}));
        for detail in [
            json!({"info": []}),
            json!({"info":{"releasedate":"2010-07-15"}}),
            json!({"info":{"name":"Inception","releasedate":"2010-99-99"}}),
            json!({"info":{"name":"Inception","releasedate":"2010-07-15","year":2011}}),
            json!({"info":{"name":"Inception","tmdb_id":"not-an-id"}}),
            json!({"info":{"name":"Inception"},"movie_data":{"name":"Another movie"}}),
            json!({"info":{"name":"Inception"},"movie_data":{"stream_id":999}}),
            json!({"info":{"name":"Inception"},"movie_data":[]}),
        ] {
            assert!(validated_details(&c, &detail).is_none(), "{detail}");
        }
        let known =
            candidate(json!({"stream_id":2318,"name":"Inception","year":2011,"tmdb_id":999}));
        assert!(validated_details(
            &known,
            &json!({"info":{"name":"Inception","releasedate":"2010-07-15","tmdb_id":27205}})
        )
        .is_none());
        let empty = candidate(json!({"stream_id":2318,"name":""}));
        assert!(
            validated_details(&empty, &json!({"info":{"name":"Inception","year":2010}})).is_none()
        );
    }

    #[tokio::test]
    async fn lazy_details_are_bounded_cached_and_fail_closed() {
        let (url, _, actions, task) = mock_xtream().await;
        let s = service();
        let p = s
            .add(json!({"name":"Queens fixture","url":url,"username":"u/+","password":"SECRET&?"}))
            .unwrap()["id"]
            .as_i64()
            .unwrap();
        for id in 4000..4040 {
            sparse_row(&s, p, &id.to_string(), "movie", "Inception");
        }
        let req = json!({"type":"movie","id":"tt1375666","name":"Inception","year":2010});
        assert_eq!(
            s.sparse_candidates("movie", &request(req.clone()), None)
                .unwrap()
                .len(),
            MAX_LAZY_DETAILS
        );
        let mut emitted = Vec::new();
        s.stream_batches(req.clone(), |_, result| {
            if let Ok(rows) = result {
                emitted.extend(rows);
            }
        })
        .await
        .unwrap();
        assert!(emitted.is_empty());
        assert_eq!(actions.lock().unwrap().len(), MAX_LAZY_DETAILS);
        s.stream_batches(req, |_, result| {
            if let Ok(rows) = result {
                emitted.extend(rows);
            }
        })
        .await
        .unwrap();
        assert!(emitted.is_empty());
        // Structurally valid malformed/mismatched details are cached, transport/schema failures are not.
        assert!(actions.lock().unwrap().len() <= MAX_LAZY_DETAILS + 2);
        assert!(s
            .candidates(Some("movie"))
            .unwrap()
            .iter()
            .filter(|c| c.stream_id != "4002")
            .all(|c| c.year.is_none()));
        task.abort();
        let _ = task.await;
    }

    #[tokio::test]
    async fn sync_is_atomic_preserves_overrides_and_resolves_series_lazily() {
        let (url, fail, actions, task) = mock_xtream().await;
        let s = service();
        let p = s
            .add(json!({"name":"Fixture","url":url,"username":"u/+","password":"SECRET&?"}))
            .unwrap()["id"]
            .as_i64()
            .unwrap();
        let result = s.sync(p).await.unwrap();
        assert_eq!(result, json!({"provider_id":p,"live":1,"vod":1,"series":1}));
        assert!(!actions
            .lock()
            .unwrap()
            .iter()
            .any(|a| a == "get_series_info"));
        let vod_id = format!("iptv:{p}:movie:22");
        s.override_match(json!({"vod_id":vod_id,"type":"movie","metadata_id":"tt7654321"}))
            .unwrap();
        s.sync(p).await.unwrap();
        assert_eq!(
            s.candidates(Some("movie")).unwrap()[0]
                .override_id
                .as_deref(),
            Some("tt7654321")
        );
        fail.store(true, std::sync::atomic::Ordering::SeqCst);
        let error = s.sync(p).await.unwrap_err();
        assert!(!error.contains("SECRET"));
        assert_eq!(s.live(None, None, 0, 100).unwrap()["total"], 1);
        assert_eq!(s.candidates(Some("movie")).unwrap().len(), 1);
        let streams = s
            .streams(json!({"type":"series","id":"tt1234567:1:2"}))
            .await
            .unwrap();
        assert_eq!(streams.len(), 1);
        assert!(streams[0]["url"]
            .as_str()
            .unwrap()
            .ends_with("/series/u%2F+/SECRET&%3F/44.mp4"));
        let cached = s
            .streams(json!({"type":"series","id":"tt1234567:1:2"}))
            .await
            .unwrap();
        assert_eq!(streams, cached);
        assert_eq!(
            actions
                .lock()
                .unwrap()
                .iter()
                .filter(|a| a.as_str() == "get_series_info")
                .count(),
            1
        );
        let guide = s.guide(format!("iptv:{p}:11")).await.unwrap();
        assert_eq!(guide["programs"].as_array().unwrap().len(), 1);
        assert_eq!(guide["programs"][0]["title"], "News");
        assert_eq!(guide["programs"][0]["start"], 1700000000i64);
        assert_eq!(s.guide(format!("iptv:{p}:11")).await.unwrap(), guide);
        assert_eq!(
            actions
                .lock()
                .unwrap()
                .iter()
                .filter(|a| a.as_str() == "get_short_epg")
                .count(),
            1
        );
        s.lock()
            .unwrap()
            .execute("UPDATE provider_cache SET expires_at=0", [])
            .unwrap();
        assert_eq!(s.guide(format!("iptv:{p}:11")).await.unwrap(), guide);
        assert_eq!(
            actions
                .lock()
                .unwrap()
                .iter()
                .filter(|a| a.as_str() == "get_short_epg")
                .count(),
            2
        );
        task.abort();
        let _ = task.await;
    }

    #[tokio::test]
    async fn network_errors_never_contain_credential_urls() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let s = service();
        let p=s.add(json!({"name":"Unavailable","url":format!("http://{address}"),"username":"PRIVATE_USER","password":"PRIVATE_PASSWORD"})).unwrap()["id"].as_i64().unwrap();
        let error = s.sync(p).await.unwrap_err();
        assert!(!error.contains("PRIVATE"));
        assert!(!error.contains("http"));
    }

    #[test]
    fn sqlite_lookup_filters_types_and_honors_metadata_and_overrides() {
        let s = service();
        let p = add_provider(&s);
        let movie = insert_candidate(&s, p, "10", "movie");
        insert_candidate(&s, p, "11", "series");
        let r = request(json!({"id":"tt1234567","name":"Amelie","year":2001}));
        let matches = s.candidates_filtered(Some("movie"), Some(&r)).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, movie);
        s.lock()
            .unwrap()
            .execute(
                "UPDATE provider_vod SET imdb_id='tt1234567' WHERE id=?1",
                [&movie],
            )
            .unwrap();
        let r = request(json!({"id":"tt1234567"}));
        assert_eq!(
            s.candidates_filtered(Some("movie"), Some(&r))
                .unwrap()
                .len(),
            1
        );
        s.override_match(json!({"vod_id":movie,"metadata_id":"tt7654321","type":"movie"}))
            .unwrap();
        let rows = s.candidates_filtered(Some("movie"), Some(&r)).unwrap();
        assert!(select_candidates(&rows, &r).is_empty());
        let r = request(json!({"id":"tt7654321"}));
        assert_eq!(
            s.candidates_filtered(Some("movie"), Some(&r))
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn movie_streams_are_raw_internal_urls() {
        let s = service();
        let p = add_provider(&s);
        insert_candidate(&s, p, "10", "movie");
        let streams = s
            .streams(json!({"type":"movie","id":"tt1234567","name":"Amelie","year":2001}))
            .await
            .unwrap();
        assert_eq!(streams.len(), 1);
        assert!(streams[0]["url"]
            .as_str()
            .unwrap()
            .ends_with("/movie/user/SUPER_SECRET/10.mkv"));
        assert!(streams[0].get("id").is_none());
        assert_eq!(streams[0]["source"], format!("iptv:{p}"));
    }
}
