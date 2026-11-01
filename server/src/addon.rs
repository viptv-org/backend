use crate::util::{json_get, now, validate_url};
use futures::{stream, StreamExt};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::sync::Semaphore;

pub(crate) mod credentials_v2;
mod discover;
mod engine;
mod extras;
pub(crate) mod http_v2;
#[cfg(test)]
mod http_v2_tests;
pub(crate) mod logo;
mod subtitles;
#[cfg(test)]
mod tests;

#[cfg(test)]
use discover::{enrich_episode_art, episode_art_url};
#[cfg(test)]
use extras::catalog_extra;
pub use extras::supports;
#[cfg(test)]
pub(super) use extras::MAX_EXTRA_OPTION;
use extras::{bounded_exact_text, bounded_text, catalog_extras, CatalogExtra};

type CachedResponses = Arc<Mutex<HashMap<String, (i64, Value, usize)>>>;
#[derive(Hash, PartialEq, Eq, Clone)]
struct MetaLookupKey {
    account: i64,
    revision: String,
    selected: bool,
    kind: String,
    id: String,
    scoped: String,
}
type CachedMetaResults = Arc<Mutex<HashMap<MetaLookupKey, (i64, Value, i64)>>>;
type CachedFailures = Arc<Mutex<HashMap<String, (i64, String)>>>;
type FetchFlight = tokio::sync::OnceCell<Result<Value, String>>;
pub use viptv_provider::discover::{DiscoveryPlan, DiscoveryRequest as DiscoverOptions};

const NEGATIVE_TTL: i64 = 60;
#[derive(Clone)]
pub struct Addons {
    db: Arc<Mutex<Connection>>,
    client: reqwest::Client,
    cache: CachedResponses,
    flights: Arc<Mutex<HashMap<String, Weak<FetchFlight>>>>,
    gate: Arc<Semaphore>,
    manifest_gate: Arc<Semaphore>,
    account_id: i64,
    engine_ids: Option<Arc<std::collections::HashSet<i64>>>,
    pub(crate) vault: Option<Arc<crate::secret_store::Vault>>,
    pub(crate) logos: Arc<logo::Cache>,
    protected_fetch: bool,
    meta_results: CachedMetaResults,
    fetch_failures: CachedFailures,
    #[cfg(test)]
    pub(crate) allow_test_loopback: bool,
}
impl Addons {
    pub fn new(db: Arc<Mutex<Connection>>, client: reqwest::Client) -> Result<Self, String> {
        {
            let mut db = db.lock().map_err(|_| "Database unavailable")?;
            let tx = db
                .transaction()
                .map_err(|_| "Database initialization failed")?;
            let existed: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='addons')", [], |r| r.get(0)).map_err(|_| "Database initialization failed")?;
            tx.execute_batch("CREATE TABLE IF NOT EXISTS addons(id INTEGER PRIMARY KEY,name TEXT NOT NULL,manifest_url TEXT UNIQUE NOT NULL,enabled INTEGER NOT NULL DEFAULT 1,manifest TEXT NOT NULL,priority INTEGER NOT NULL DEFAULT 0);").map_err(|_| "Database initialization failed")?;
            let has_priority = {
                let mut stmt = tx
                    .prepare("PRAGMA table_info(addons)")
                    .map_err(|_| "Database initialization failed")?;
                let columns = stmt
                    .query_map([], |r| r.get::<_, String>(1))
                    .map_err(|_| "Database initialization failed")?;
                columns
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| "Database initialization failed")?
                    .iter()
                    .any(|c| c == "priority")
            };
            if !has_priority {
                tx.execute_batch(
                    "ALTER TABLE addons ADD COLUMN priority INTEGER NOT NULL DEFAULT 0",
                )
                .map_err(|_| "Database initialization failed")?;
            }
            if !existed {
                let manifest = json!({"id":"com.linvo.cinemeta","name":"Cinemeta","resources":["catalog","meta"],"types":["movie","series"],"catalogs":[{"type":"movie","id":"top","name":"Popular movies","extra":[{"name":"search"},{"name":"skip"}]},{"type":"series","id":"top","name":"Popular series","extra":[{"name":"search"},{"name":"skip"}]}]});
                tx.execute("INSERT INTO addons(name,manifest_url,manifest) VALUES('Cinemeta','https://v3-cinemeta.strem.io/manifest.json',?1)", [manifest.to_string()]).map_err(|_| "Database initialization failed")?;
            }
            let scoped: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('addons') WHERE name='account_id')", [], |r| r.get(0)).map_err(|_| "Database initialization failed")?;
            if !scoped {
                credentials_v2::scope_legacy(&tx)?;
            }
            credentials_v2::init(&tx)?;
            crate::engine_v3::origins::init(&tx).map_err(|_| "Metadata origin initialization failed")?;
            tx.commit().map_err(|_| "Database initialization failed")?;
        }
        Ok(Self {
            db,
            client,
            cache: Default::default(),
            flights: Default::default(),
            gate: Arc::new(Semaphore::new(12)),
            manifest_gate: Arc::new(Semaphore::new(2)),
            account_id: 0,
            engine_ids: None,
            vault: None,
            logos: Arc::new(logo::Cache::new()),
            protected_fetch: false,
            meta_results: Default::default(),
            fetch_failures: Default::default(),
            #[cfg(test)]
            allow_test_loopback: false,
        })
    }
    pub fn for_account(mut self, account_id: i64) -> Self {
        self.account_id = account_id;
        self
    }
    pub(crate) fn with_protected_fetch(mut self) -> Self {
        self.protected_fetch = true;
        self
    }
    fn protected(&self) -> bool {
        self.protected_fetch || self.vault.is_some()
    }
    pub(crate) fn fixture_transport(&self) -> bool {
        #[cfg(test)]
        {
            self.allow_test_loopback
        }
        #[cfg(not(test))]
        {
            false
        }
    }
    #[cfg(test)]
    pub(crate) fn forget_responses(&self) {
        self.cache.lock().unwrap().clear();
        self.meta_results.lock().unwrap().clear();
        self.fetch_failures.lock().unwrap().clear();
    }
    pub(crate) fn revision(&self) -> Result<String, String> {
        let db = self.db.lock().map_err(|_| "Database unavailable")?;
        self.revision_db(&db)
    }
    pub(crate) fn revision_db(&self, db: &Connection) -> Result<String, String> {
        let revision: String = db.query_row(
            "SELECT COALESCE((SELECT revision FROM addon_config_revisions WHERE account_id=?1),'00000000000000000000000000000000')",
            [self.account_id],
            |row| row.get(0),
        ).map_err(|_| "Database query failed")?;
        Ok(revision)
    }
    fn cache_key(&self, url: &str) -> Result<String, String> {
        if self.protected() {
            Ok(format!(
                "secure:{}:{}:{url}",
                self.account_id,
                self.revision()?
            ))
        } else {
            Ok(url.to_owned())
        }
    }
    fn checked_url(&self, raw: &str) -> Result<url::Url, String> {
        if raw.chars().any(char::is_control) {
            return Err("invalid_addon_endpoint".into());
        }
        let url = validate_url(raw).map_err(|_| "invalid_addon_endpoint")?;
        if self.protected() {
            crate::source_http::validate(&url, self.fixture_transport())
                .map_err(|e| e.addon_code())?;
        }
        Ok(url)
    }
    fn manifest_url(&self, url: &str) -> Result<url::Url, String> {
        if url.len() > 4096 {
            return Err("invalid_addon_endpoint".into());
        }
        let url = self.checked_url(url)?;
        if !url.path().ends_with("/manifest.json") || url.fragment().is_some() {
            return Err("invalid_addon_endpoint".into());
        }
        Ok(url)
    }
    pub(crate) async fn prepare_manifest(&self, url: &str) -> Result<(String, Value), String> {
        let url = self.manifest_url(url)?;
        let _permit = self
            .manifest_gate
            .clone()
            .try_acquire_owned()
            .map_err(|_| "addon_checks_busy")?;
        self.fetch_manifest(url).await
    }
    pub(crate) async fn prepare_manifest_within(
        &self,
        url: &str,
        deadline: tokio::time::Instant,
    ) -> Result<(String, Value), String> {
        let url = self.manifest_url(url)?;
        let _permit = tokio::time::timeout_at(deadline, self.manifest_gate.clone().acquire_owned())
            .await
            .map_err(|_| "addon_checks_busy")?
            .map_err(|_| "addon_checks_busy")?;
        tokio::time::timeout_at(deadline, self.fetch_manifest(url))
            .await
            .map_err(|_| "addon_timeout")?
    }
    async fn fetch_manifest(&self, url: url::Url) -> Result<(String, Value), String> {
        let manifest = if self.protected() {
            crate::source_http::json(
                url.clone(),
                32 * 1024 * 1024,
                Duration::from_secs(25),
                self.fixture_transport(),
                true,
            )
            .await
            .map_err(|e| e.addon_code().to_owned())?
        } else {
            tokio::time::timeout(
                Duration::from_secs(25),
                json_get(&self.client, url.as_str()),
            )
            .await
            .map_err(|_| "Upstream timed out")??
        };
        if !manifest["name"]
            .as_str()
            .is_some_and(|n| !n.trim().is_empty() && n.len() <= 256)
            || !manifest["id"]
                .as_str()
                .is_some_and(|id| !id.is_empty() && id.len() <= 256)
            || !manifest["resources"].is_array()
        {
            return Err("Invalid addon manifest".into());
        }
        Ok((url.to_string(), manifest))
    }
    pub fn entries(&self) -> Result<Vec<(i64, String, Value)>, String> {
        let (entries, errors) = self.entries_with_errors()?;
        if entries.is_empty() {
            if let Some((_, error)) = errors.into_iter().next() {
                return Err(error);
            }
        }
        Ok(entries)
    }
    #[allow(clippy::type_complexity)]
    pub(crate) fn entries_with_errors(
        &self,
    ) -> Result<(Vec<(i64, String, Value)>, Vec<(i64, String)>), String> {
        let db = self.db.lock().map_err(|_| "Database unavailable")?;
        let mut q = db
            .prepare(
                "SELECT id,manifest_url,manifest,credentials_version FROM addons WHERE enabled=1 AND account_id=?1 ORDER BY id",
            )
            .map_err(|_| "Database query failed")?;
        let rows = q
            .query_map([self.account_id], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })
            .map_err(|_| "Database query failed")?;
        let mut entries = vec![];
        let mut errors = vec![];
        for r in rows {
            let (i, u, m, version) = r.map_err(|_| "Database query failed")?;
            if self
                .engine_ids
                .as_ref()
                .is_some_and(|ids| !ids.contains(&i))
            {
                continue;
            }
            match credentials_v2::read(
                &db,
                self.vault.as_deref(),
                self.account_id,
                i,
                u,
                m,
                version,
            ) {
                Ok((u, m)) => entries.push((i, u, m)),
                Err(error) => errors.push((i, error.into())),
            }
        }
        Ok((entries, errors))
    }
    async fn async_entries(&self) -> Result<Vec<(i64, String, Value)>, String> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.entries())
            .await
            .map_err(|_| "Database task failed")?
    }
    pub fn catalogs(&self) -> Result<Value, String> {
        let mut out = vec![];
        'addons: for (id, _, m) in self.entries()? {
            for c in m["catalogs"].as_array().into_iter().flatten().take(256) {
                if out.len() == 2048 {
                    break 'addons;
                }
                let Some(catalog_id) = bounded_exact_text(&c["id"], 256) else {
                    continue;
                };
                let Some(kind) = bounded_exact_text(&c["type"], 64) else {
                    continue;
                };
                let name = bounded_text(&c["name"], 256).unwrap_or_else(|| catalog_id.clone());
                let extras = catalog_extras(c);
                let normalized = extras.iter().map(CatalogExtra::wire).collect::<Vec<_>>();
                let genres = extras
                    .iter()
                    .find(|extra| extra.name == "genre")
                    .map(|extra| extra.options.clone())
                    .unwrap_or_default();
                out.push(json!({
                    "addon_id":id,
                    "addon_name":m["name"],
                    "id":catalog_id,
                    "type":kind,
                    "name":name,
                    "extra":normalized,
                    "supports_search":extras.iter().any(|extra| extra.name == "search"),
                    "supports_skip":extras.iter().any(|extra| extra.name == "skip"),
                    "genres":genres,
                }));
            }
        }
        Ok(Value::Array(out))
    }
    pub(crate) fn available(db: &Connection, account: i64, id: i64) -> bool {
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM addons WHERE id=?1 AND account_id=?2 AND enabled=1)",
            params![id, account],
            |r| r.get::<_, bool>(0),
        )
        .unwrap_or(false)
    }
    async fn fetch(&self, url: &str, ttl: i64) -> Result<Value, String> {
        if self.protected() {
            self.checked_url(url)?;
        }
        let key = self.cache_key(url)?;
        if let Some((expiry, v, _)) = self.cache.lock().unwrap().get(&key) {
            if *expiry > now() {
                return Ok(v.clone());
            }
        }
        if let Some((retry, error)) = self.fetch_failures.lock().unwrap().get(&key) {
            if *retry > now() {
                return Err(error.clone());
            }
        }
        let flight = {
            let mut flights = self.flights.lock().unwrap();
            flights.retain(|_, flight| flight.strong_count() > 0);
            match flights.get(&key).and_then(Weak::upgrade) {
                Some(flight) => flight,
                None => {
                    let flight = Arc::new(FetchFlight::new());
                    flights.insert(key.clone(), Arc::downgrade(&flight));
                    flight
                }
            }
        };
        flight
            .get_or_init(|| self.fetch_uncached(url, ttl, &key))
            .await
            .clone()
    }
    async fn fetch_uncached(&self, url: &str, ttl: i64, key: &str) -> Result<Value, String> {
        let _permit = self.gate.acquire().await.map_err(|_| "Service stopping")?;
        if let Some((expiry, value, _)) = self.cache.lock().unwrap().get(key) {
            if *expiry > now() {
                return Ok(value.clone());
            }
        }
        let v = if self.protected() {
            crate::source_http::json(
                self.checked_url(url)?,
                32 * 1024 * 1024,
                Duration::from_secs(25),
                self.fixture_transport(),
                true,
            )
            .await
            .map_err(|e| e.addon_code().to_owned())
        } else {
            match tokio::time::timeout(Duration::from_secs(25), json_get(&self.client, url)).await {
                Err(_) => Err("Upstream timed out".to_owned()),
                Ok(result) => result,
            }
        };
        let v = match v {
            Err(error) => {
                let mut failures = self.fetch_failures.lock().unwrap();
                failures.retain(|_, (retry, _)| *retry > now());
                failures.insert(key.to_owned(), (now() + NEGATIVE_TTL, error.clone()));
                return Err(error);
            }
            Ok(v) => v,
        };
        let mut cache = self.cache.lock().unwrap();
        cache.retain(|_, (e, _, _)| *e > now());
        let size = v.to_string().len();
        let mut bytes = cache.values().map(|(_, _, size)| *size).sum::<usize>();
        while cache.len() >= 256 || bytes + size > 64 * 1024 * 1024 {
            let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, (expiry, _, _))| *expiry)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            if let Some((_, _, removed)) = cache.remove(&oldest) {
                bytes -= removed;
            }
        }
        cache.insert(key.into(), (now() + ttl, v.clone(), size));
        Ok(v)
    }
    pub(crate) async fn metadata_from(
        &self,
        manifest_url: &str,
        kind: &str,
        id: &str,
    ) -> Result<Value, String> {
        let endpoint = Self::endpoint(manifest_url, &["meta", kind, &format!("{id}.json")])?;
        self.fetch(&endpoint, 0).await
    }
    pub fn endpoint(base: &str, parts: &[&str]) -> Result<String, String> {
        viptv_provider::discover::addon_endpoint(base, parts)
    }
}
