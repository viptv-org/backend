use crate::util::{json_get, now, validate_url};
use futures::{stream, StreamExt};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Semaphore;

mod discover;
mod extras;
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
pub use viptv_provider::discover::{DiscoveryPlan, DiscoveryRequest as DiscoverOptions};
#[derive(Clone)]
pub struct Addons {
    db: Arc<Mutex<Connection>>,
    client: reqwest::Client,
    cache: CachedResponses,
    gate: Arc<Semaphore>,
    account_id: i64,
}
impl Addons {
    pub fn new(db: Arc<Mutex<Connection>>, client: reqwest::Client) -> Result<Self, String> {
        {
            let mut db = db.lock().map_err(|_| "Database unavailable")?;
            let tx = db
                .transaction()
                .map_err(|_| "Database initialization failed")?;
            // The table itself is the persistent initialization marker, including for
            // legacy databases whose user deliberately removed every addon.
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
            // Preserve legacy addon IDs/configuration for the owner, once only.
            let scoped: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('addons') WHERE name='account_id')", [], |r| r.get(0)).map_err(|_| "Database initialization failed")?;
            if !scoped {
                tx.execute_batch("ALTER TABLE addons RENAME TO addons_legacy; CREATE TABLE addons(id INTEGER PRIMARY KEY,name TEXT NOT NULL,manifest_url TEXT NOT NULL,enabled INTEGER NOT NULL DEFAULT 1,manifest TEXT NOT NULL,priority INTEGER NOT NULL DEFAULT 0,account_id INTEGER NOT NULL DEFAULT 0,UNIQUE(account_id,manifest_url)); INSERT INTO addons(id,name,manifest_url,enabled,manifest,priority) SELECT id,name,manifest_url,enabled,manifest,priority FROM addons_legacy; DROP TABLE addons_legacy;").map_err(|_| "Addon account migration failed")?;
                let auth_exists: bool = tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='auth_accounts')",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(|_| "Database initialization failed")?;
                if auth_exists {
                    tx.execute("UPDATE addons SET account_id=COALESCE((SELECT id FROM auth_accounts WHERE role='owner' ORDER BY id LIMIT 1),0)", []).map_err(|_| "Addon ownership migration failed")?;
                }
            }
            tx.commit().map_err(|_| "Database initialization failed")?;
        }
        Ok(Self {
            db,
            client,
            cache: Default::default(),
            gate: Arc::new(Semaphore::new(12)),
            account_id: 0,
        })
    }
    pub fn for_account(mut self, account_id: i64) -> Self {
        self.account_id = account_id;
        self
    }
    pub fn entries(&self) -> Result<Vec<(i64, String, Value)>, String> {
        let db = self.db.lock().map_err(|_| "Database unavailable")?;
        let mut q = db
            .prepare(
                "SELECT id,manifest_url,manifest FROM addons WHERE enabled=1 AND account_id=?1 ORDER BY id",
            )
            .map_err(|_| "Database query failed")?;
        let rows = q
            .query_map([self.account_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get::<_, String>(2)?))
            })
            .map_err(|_| "Database query failed")?;
        rows.map(|r| {
            let (i, u, m) = r.map_err(|_| "Database query failed")?;
            Ok((
                i,
                u,
                serde_json::from_str(&m).map_err(|_| "Invalid stored manifest")?,
            ))
        })
        .collect()
    }
    pub fn list(&self) -> Result<Value, String> {
        let db = self.db.lock().map_err(|_| "Database unavailable")?;
        let mut stmt = db
            .prepare(
                "SELECT id,name,manifest_url,enabled,priority FROM addons WHERE account_id=?1 ORDER BY id",
            )
            .map_err(|_| "Database query failed")?;
        let rows = stmt
            .query_map([self.account_id], Self::config_row)
            .map_err(|_| "Database query failed")?;
        Ok(Value::Array(
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|_| "Database query failed")?,
        ))
    }
    fn config_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
        Ok(
            json!({"id":r.get::<_,i64>(0)?,"name":r.get::<_,String>(1)?,"manifest_url":r.get::<_,String>(2)?,"enabled":r.get::<_,bool>(3)?}),
        )
    }
    /// Patch only enabled, preserving omitted fields. The complete saved
    /// configuration is returned. Call from a blocking worker in async handlers.
    pub fn update(&self, id: i64, patch: Value) -> Result<Value, String> {
        let fields = patch.as_object().ok_or("Addon patch must be an object")?;
        if fields.is_empty() || fields.keys().any(|k| k != "enabled") {
            return Err("Patch accepts enabled only".into());
        }
        let enabled = fields
            .get("enabled")
            .map(|v| v.as_bool().ok_or("enabled must be a boolean"))
            .transpose()?;
        let db = self.db.lock().map_err(|_| "Database unavailable")?;
        let changed = db
            .execute(
                "UPDATE addons SET enabled=COALESCE(?1,enabled) WHERE id=?2 AND account_id=?3",
                params![enabled, id, self.account_id],
            )
            .map_err(|_| "Database update failed")?;
        if changed == 0 {
            return Err("Addon not found".into());
        }
        db.query_row(
            "SELECT id,name,manifest_url,enabled,priority FROM addons WHERE id=?1 AND account_id=?2",
            [id,self.account_id],
            Self::config_row,
        )
        .map_err(|_| "Database query failed".into())
    }
    async fn async_entries(&self) -> Result<Vec<(i64, String, Value)>, String> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.entries())
            .await
            .map_err(|_| "Database task failed")?
    }
    pub async fn add(&self, url: &str) -> Result<Value, String> {
        validate_url(url)?;
        if !url
            .split('?')
            .next()
            .unwrap_or("")
            .ends_with("/manifest.json")
        {
            return Err("Manifest URL must end in /manifest.json".into());
        }
        self.cache.lock().unwrap().remove(url);
        let m = self.fetch(url, 300).await?;
        if !m["name"].is_string() || !m["id"].is_string() || !m["resources"].is_array() {
            return Err("Invalid addon manifest".into());
        }
        let this = self.clone();
        let url = url.to_owned();
        tokio::task::spawn_blocking(move || {
            let db = this.db.lock().map_err(|_| "Database unavailable")?;
            db.execute("INSERT INTO addons(name,manifest_url,manifest,account_id) VALUES(?1,?2,?3,?4) ON CONFLICT(account_id,manifest_url) DO UPDATE SET name=excluded.name,manifest=excluded.manifest",params![m["name"].as_str(),url,m.to_string(),this.account_id]).map_err(|_|"Could not save addon")?;
            db.query_row("SELECT id,name,manifest_url,enabled,priority FROM addons WHERE manifest_url=?1 AND account_id=?2", params![url,this.account_id], Self::config_row).map_err(|_| "Database query failed".into())
        }).await.map_err(|_| "Database task failed")?
    }
    pub fn delete(&self, id: i64) -> Result<(), String> {
        self.db
            .lock()
            .map_err(|_| "Database unavailable")?
            .execute(
                "DELETE FROM addons WHERE id=?1 AND account_id=?2",
                [id, self.account_id],
            )
            .map_err(|_| "Database update failed")?;
        Ok(())
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
    async fn fetch(&self, url: &str, ttl: i64) -> Result<Value, String> {
        if let Some((expiry, v, _)) = self.cache.lock().unwrap().get(url) {
            if *expiry > now() {
                return Ok(v.clone());
            }
        }
        let _permit = self.gate.acquire().await.map_err(|_| "Service stopping")?;
        let v = tokio::time::timeout(Duration::from_secs(25), json_get(&self.client, url))
            .await
            .map_err(|_| "Upstream timed out")??;
        let mut cache = self.cache.lock().unwrap();
        cache.retain(|_, (e, _, _)| *e > now());
        let size = v.to_string().len();
        if cache.len() >= 256
            || cache.values().map(|(_, _, size)| *size).sum::<usize>() + size > 64 * 1024 * 1024
        {
            cache.clear();
        }
        cache.insert(url.into(), (now() + ttl, v.clone(), size));
        Ok(v)
    }
    pub fn endpoint(base: &str, parts: &[&str]) -> Result<String, String> {
        viptv_provider::discover::addon_endpoint(base, parts)
    }
}
