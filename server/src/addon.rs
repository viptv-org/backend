use crate::util::{json_get, now, validate_url};
use futures::{stream, StreamExt};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Semaphore;
type CachedResponses = Arc<Mutex<HashMap<String, (i64, Value, usize)>>>;
pub struct DiscoverOptions {
    pub kind: String,
    pub catalog: Option<String>,
    pub addon: Option<i64>,
    pub skip: usize,
    pub search: Option<String>,
    pub genre: Option<String>,
    pub extras: HashMap<String, String>,
}
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
        let mut u = validate_url(base)?;
        {
            let mut path = u
                .path_segments_mut()
                .map_err(|_| "Invalid addon base URL")?;
            path.pop();
            for p in parts {
                path.push(p);
            }
        }
        Ok(u.to_string())
    }
    fn extra_endpoint(
        base: &str,
        kind: &str,
        catalog: &str,
        encoded_extras: &str,
    ) -> Result<String, String> {
        let mut url = validate_url(&Self::endpoint(base, &["catalog", kind, catalog])?)?;
        // Values were encoded exactly once above. Unlike path_segments_mut.push,
        // set_path preserves '%' escapes, while the fixed '&'/'=' delimiters remain intact.
        url.set_path(&format!("{}/{}.json", url.path(), encoded_extras));
        Ok(url.into())
    }
    /// Browse the first applicable enabled catalog in installation order, or
    /// aggregate a single bounded search across at most 32 matching catalogs.
    /// Search never paginates. Browsing advances by the raw upstream page length,
    /// not by the deduplicated/capped (200 item) response length.
    pub async fn discover(
        &self,
        kind: String,
        catalog: Option<String>,
        addon: Option<i64>,
        skip: usize,
        search: Option<String>,
        genre: Option<String>,
    ) -> Result<Value, String> {
        self.discover_with_options(DiscoverOptions {
            kind,
            catalog,
            addon,
            skip,
            search,
            genre,
            extras: HashMap::new(),
        })
        .await
    }
    pub async fn discover_with_options(&self, request: DiscoverOptions) -> Result<Value, String> {
        let DiscoverOptions {
            kind,
            catalog,
            addon,
            skip,
            search,
            genre,
            extras: custom,
        } = request;
        if kind.is_empty()
            || kind.len() > 64
            || !kind
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        {
            return Err("Unsupported catalog type".into());
        }
        if custom.len() > 16
            || custom.iter().any(|(k, v)| {
                extra_name(&json!(k)).is_none()
                    || ["search", "genre", "skip"].contains(&k.as_str())
                    || v.chars().count() > 1024
            })
        {
            return Err("Invalid catalog options".into());
        }
        if !custom.is_empty() && (catalog.is_none() || addon.is_none()) {
            return Err("Choose a catalog for custom options".into());
        }
        let search = search
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let genre = genre
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        if genre
            .as_ref()
            .is_some_and(|value| value.chars().count() > MAX_EXTRA_OPTION)
        {
            return Err("Genre too long".into());
        }
        let aggregated = search.is_some() && catalog.is_none();
        if aggregated && skip > 0 {
            return Err("Aggregated search does not support pagination".into());
        }
        if skip > 10000 {
            return Err("Catalog skip must not exceed 10000".into());
        }
        let mut tasks = vec![];
        let mut pageable = false;
        let mut genre_unsupported = false;
        let mut genre_invalid = false;
        'addons: for (id, u, m) in self.async_entries().await? {
            if addon.is_some_and(|a| a != id) {
                continue;
            }
            for c in m["catalogs"].as_array().into_iter().flatten() {
                if c["type"] != kind {
                    continue;
                }
                let cid = c["id"].as_str().unwrap_or("");
                if cid.is_empty() || catalog.as_deref().is_some_and(|wanted| wanted != cid) {
                    continue;
                }
                let declared_extras = catalog_extras(c);
                if search.is_some() && !declared_extras.iter().any(|extra| extra.name == "search") {
                    continue;
                }
                if let Some(requested) = &genre {
                    let Some(capability) =
                        declared_extras.iter().find(|extra| extra.name == "genre")
                    else {
                        genre_unsupported = true;
                        continue;
                    };
                    if !capability.options.is_empty()
                        && !capability.options.iter().any(|option| option == requested)
                    {
                        genre_invalid = true;
                        continue;
                    }
                }
                if custom.iter().any(|(key, value)| {
                    !declared_extras.iter().any(|e| {
                        e.name == *key && (e.options.is_empty() || e.options.contains(value))
                    })
                }) {
                    return Err("Option is not advertised by the selected catalog".into());
                }
                // Every required extra must be one this request can actually supply.
                if declared_extras.iter().any(|extra| {
                    extra.required
                        && match extra.name.as_str() {
                            "skip" => false,
                            "search" => search.is_none(),
                            "genre" => genre.is_none(),
                            _ => custom.get(&extra.name).is_none_or(|v| v.trim().is_empty()),
                        }
                }) {
                    continue;
                }
                let mut extras = Vec::new();
                let supports_skip = declared_extras.iter().any(|extra| extra.name == "skip");
                if supports_skip {
                    extras.push(format!("skip={}", skip.min(10000)));
                } else if skip > 0 {
                    return Err("Selected catalog does not support pagination".into());
                }
                if let Some(s) = &search {
                    extras.push(format!(
                        "search={}",
                        url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>()
                    ));
                }
                if let Some(value) = &genre {
                    extras.push(format!(
                        "genre={}",
                        url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>()
                    ));
                }
                for (name, value) in &custom {
                    if !value.trim().is_empty() {
                        extras.push(format!(
                            "{}={}",
                            name,
                            url::form_urlencoded::byte_serialize(value.as_bytes())
                                .collect::<String>()
                        ));
                    }
                }
                let endpoint = if extras.is_empty() {
                    Self::endpoint(&u, &["catalog", &kind, &format!("{cid}.json")])?
                } else {
                    Self::extra_endpoint(&u, &kind, cid, &extras.join("&"))?
                };
                tasks.push(endpoint);
                pageable = supports_skip;
                if !aggregated || tasks.len() == 32 {
                    break 'addons;
                }
            }
        }
        if tasks.is_empty() {
            if genre_invalid {
                return Err("Genre is not one of the catalog's advertised options".into());
            }
            if genre_unsupported {
                return Err("Selected catalog does not advertise genre filtering".into());
            }
        }
        let single_catalog = !aggregated && tasks.len() == 1;
        let results = stream::iter(tasks.into_iter().take(32).map(|u| {
            let s = self.clone();
            async move { s.fetch(&u, 300).await }
        }))
        .buffered(8)
        .collect::<Vec<_>>()
        .await;
        let mut metas = vec![];
        let mut seen = HashSet::new();
        let mut success = false;
        let mut raw_count = 0;
        for v in results.into_iter().flatten() {
            let Some(raw) = v["metas"].as_array() else {
                continue;
            };
            success = true;
            raw_count += raw.len();
            for m in raw {
                if metas.len() == 200 {
                    break;
                }
                let key = format!("{}:{}", m["type"], m["id"]);
                if seen.insert(key) {
                    metas.push(m.clone());
                }
            }
        }
        if !success {
            return Err("No catalog source succeeded or matched this request".into());
        }
        // Stremio supplies no total/page-size contract: a nonempty raw page is
        // potentially followed by another page; an empty page terminates traversal.
        let has_more =
            single_catalog && pageable && raw_count > 0 && skip.saturating_add(raw_count) <= 10000;
        let next_skip = has_more.then(|| skip + raw_count);
        Ok(
            json!({"metas":metas,"has_more":has_more,"next_skip":next_skip,
            "aggregated":aggregated,"max_catalogs":if aggregated {32} else {1},"max_results":200}),
        )
    }
    pub async fn meta(&self, kind: &str, id: &str) -> Result<Value, String> {
        let endpoints = self
            .async_entries()
            .await?
            .into_iter()
            .filter(|(_, _, m)| supports(m, "meta", kind, id))
            .take(32)
            .map(|(_, u, _)| Self::endpoint(&u, &["meta", kind, &format!("{id}.json")]))
            .collect::<Result<Vec<_>, _>>()?;
        let mut results = stream::iter(endpoints.into_iter().map(|u| {
            let this = self.clone();
            async move { this.fetch(&u, 3600).await }
        }))
        .buffered(8);
        while let Some(result) = results.next().await {
            if let Ok(v) = result {
                if v["meta"].is_object() {
                    let mut primary = v;
                    if kind == "series" && primary["meta"]["videos"].is_array() {
                        // Alternate addons may have working stills where the preferred
                        // metadata uses broken generated URLs. Keep playback identities.
                        let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
                        while let Ok(Some(next)) =
                            tokio::time::timeout_at(deadline, results.next()).await
                        {
                            if let Ok(other) = next {
                                enrich_episode_art(&mut primary["meta"], &other["meta"]);
                            }
                        }
                        if let Some(videos) = primary["meta"]["videos"].as_array_mut() {
                            for video in videos.iter_mut().take(2000) {
                                if let Some(image) = episode_art_url(&video["thumbnail"]) {
                                    video["thumbnail"] = json!(image);
                                }
                            }
                        }
                    }
                    return Ok(primary);
                }
            }
        }
        Err("Metadata not found".into())
    }
    pub async fn streams(&self, u: &str, kind: &str, id: &str) -> Result<Vec<Value>, String> {
        let endpoint = Self::endpoint(u, &["stream", kind, &format!("{id}.json")])?;
        let v = self.fetch(&endpoint, 60).await?;
        Ok(v["streams"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .take(100)
            .collect())
    }
}
// Only public artwork hosts are sent to the optional resizing service. Never
// forward configured addon/CDN tokens, arbitrary URLs, or local-provider images.
fn public_episode_art(value: &Value) -> Option<String> {
    let mut url = url::Url::parse(value.as_str()?).ok()?;
    if url.host_str() == Some("api.top-posters.com") {
        let fallback = url
            .query_pairs()
            .find(|(key, _)| key == "fallback_url")?
            .1
            .into_owned();
        url = url::Url::parse(&fallback).ok()?;
    }
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || !matches!(
            url.host_str(),
            Some("artworks.thetvdb.com" | "image.tmdb.org" | "episodes.metahub.space")
        )
        || url.query().is_some()
    {
        return None;
    }
    Some(url.to_string())
}
fn episode_art_url(value: &Value) -> Option<String> {
    let original = public_episode_art(value)?;
    let mut proxy = url::Url::parse("https://wsrv.nl/").ok()?;
    proxy
        .query_pairs_mut()
        .append_pair("url", &original)
        .append_pair("w", "512")
        .append_pair("h", "288")
        .append_pair("fit", "cover")
        .append_pair("output", "jpg");
    Some(proxy.to_string())
}
fn enrich_episode_art(primary: &mut Value, alternate: &Value) {
    let same_series = primary["id"].as_str().is_some() && primary["id"] == alternate["id"];
    if !same_series {
        return;
    }
    let Some(videos) = primary["videos"].as_array_mut() else {
        return;
    };
    let Some(other) = alternate["videos"].as_array() else {
        return;
    };
    for video in videos.iter_mut().take(2000) {
        let current = video["thumbnail"].as_str().unwrap_or("");
        if !current.is_empty() && !current.contains("episodes.metahub.space") {
            continue;
        }
        if let Some(candidate) = other.iter().take(2000).find(|candidate| {
            let dates_agree = video["released"]
                .as_str()
                .zip(candidate["released"].as_str())
                .is_some_and(|(a, b)| a.get(..10).is_some() && a.get(..10) == b.get(..10));
            candidate["id"] == video["id"]
                || (dates_agree
                    && !video["season"].is_null()
                    && !video["episode"].is_null()
                    && candidate["season"] == video["season"]
                    && candidate["episode"] == video["episode"])
        }) {
            if let Some(image) = public_episode_art(&candidate["thumbnail"]) {
                if !image.contains("episodes.metahub.space") {
                    video["thumbnail"] = json!(image);
                }
            }
        }
    }
}

const MAX_CATALOG_EXTRAS: usize = 16;
const MAX_EXTRA_OPTIONS: usize = 256;
const MAX_EXTRA_NAME: usize = 64;
const MAX_EXTRA_OPTION: usize = 128;

#[derive(Clone, Debug, PartialEq)]
struct CatalogExtra {
    name: String,
    required: bool,
    options: Vec<String>,
    options_limit: Option<usize>,
    default: Option<String>,
}
impl CatalogExtra {
    fn wire(&self) -> Value {
        json!({
            "name":self.name,
            "is_required":self.required,
            "options":self.options,
            "options_limit":self.options_limit,
            "default":self.default,
        })
    }
}
fn bounded_text(value: &Value, limit: usize) -> Option<String> {
    let value = value.as_str()?.trim();
    if value.is_empty() {
        return None;
    }
    Some(value.chars().take(limit).collect())
}
fn bounded_exact_text(value: &Value, limit: usize) -> Option<String> {
    let value = value.as_str()?.trim();
    (!value.is_empty() && value.chars().count() <= limit).then(|| value.to_owned())
}
fn extra_name(value: &Value) -> Option<String> {
    let name = bounded_exact_text(value, MAX_EXTRA_NAME)?;
    name.chars()
        .all(|character| character.is_ascii_alphanumeric() || "_-".contains(character))
        .then_some(name)
}
fn bounded_options(value: &Value) -> Vec<String> {
    let mut seen = HashSet::new();
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|option| bounded_exact_text(option, MAX_EXTRA_OPTION))
        .filter(|option| seen.insert(option.clone()))
        .take(MAX_EXTRA_OPTIONS)
        .collect()
}
fn merge_extra(
    extras: &mut Vec<CatalogExtra>,
    name: String,
    required: bool,
    options: Vec<String>,
    limit: Option<usize>,
) {
    if let Some(existing) = extras.iter_mut().find(|extra| extra.name == name) {
        existing.required |= required;
        for option in options {
            if existing.options.len() == MAX_EXTRA_OPTIONS {
                break;
            }
            if !existing.options.contains(&option) {
                existing.options.push(option);
            }
        }
        if existing.options_limit.is_none() {
            existing.options_limit = limit;
        }
    } else if extras.len() < MAX_CATALOG_EXTRAS {
        extras.push(CatalogExtra {
            name,
            required,
            options,
            options_limit: limit,
            default: None,
        });
    }
}
fn catalog_extras(catalog: &Value) -> Vec<CatalogExtra> {
    let mut extras = Vec::<CatalogExtra>::new();
    let declared = match &catalog["extra"] {
        Value::Array(values) => values.iter().collect::<Vec<_>>(),
        Value::Object(_) | Value::String(_) => vec![&catalog["extra"]],
        _ => Vec::new(),
    };
    for value in declared {
        let (name, required, options, limit) = if let Some(name) = value.as_str() {
            (
                extra_name(&Value::String(name.into())),
                false,
                Vec::new(),
                None,
            )
        } else {
            (
                extra_name(&value["name"]),
                value["isRequired"].as_bool().unwrap_or(false),
                bounded_options(&value["options"]),
                value["optionsLimit"]
                    .as_u64()
                    .map(|number| number.min(1000) as usize),
            )
        };
        if let Some(name) = name {
            merge_extra(&mut extras, name.clone(), required, options, limit);
            if let Some(default) = bounded_exact_text(&value["default"], MAX_EXTRA_OPTION) {
                if let Some(extra) = extras.iter_mut().find(|e| e.name == name) {
                    if extra.options.is_empty() || extra.options.contains(&default) {
                        extra.default = Some(default);
                    }
                }
            }
        }
    }
    for value in catalog["extraSupported"].as_array().into_iter().flatten() {
        if let Some(name) = extra_name(value) {
            merge_extra(&mut extras, name, false, Vec::new(), None);
        }
    }
    for value in catalog["extraRequired"].as_array().into_iter().flatten() {
        if let Some(name) = extra_name(value) {
            merge_extra(&mut extras, name, true, Vec::new(), None);
        }
    }
    let legacy_genres = bounded_options(&catalog["genres"]);
    if !legacy_genres.is_empty() && extras.iter().any(|extra| extra.name == "genre") {
        merge_extra(&mut extras, "genre".into(), false, legacy_genres, None);
    }
    extras
}
#[cfg(test)]
fn catalog_extra(catalog: &Value, name: &str) -> bool {
    catalog_extras(catalog)
        .iter()
        .any(|extra| extra.name == name)
}
pub fn supports(m: &Value, resource: &str, kind: &str, id: &str) -> bool {
    if m["idPrefixes"].as_array().is_some_and(|prefixes| {
        !prefixes
            .iter()
            .any(|p| p.as_str().is_some_and(|p| id.starts_with(p)))
    }) {
        return false;
    }
    m["resources"]
        .as_array()
        .map(|r| {
            r.iter().any(|r| {
                if r.as_str() == Some(resource) {
                    return m["types"]
                        .as_array()
                        .map(|t| t.iter().any(|t| t == kind))
                        .unwrap_or(true);
                }
                r["name"] == resource
                    && r["types"]
                        .as_array()
                        .map(|t| t.iter().any(|t| t == kind))
                        .unwrap_or(true)
                    && r["idPrefixes"]
                        .as_array()
                        .map(|p| p.iter().any(|p| id.starts_with(p.as_str().unwrap_or("!"))))
                        .unwrap_or(true)
            })
        })
        .unwrap_or(false)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoint_encodes_untrusted_ids() {
        let u = Addons::endpoint(
            "https://host/key/manifest.json",
            &["meta", "movie", "../../secret.json"],
        )
        .unwrap();
        assert!(u.contains("..%2F..%2Fsecret.json"));
    }
    #[tokio::test]
    async fn catalog_cache_and_protocol_without_skip() {
        use axum::{routing::get, Router};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let mock = Router::new()
            .route("/manifest.json", get(|| async { axum::Json(json!({"id":"mock","name":"Mock","resources":["catalog"],"types":["movie"],"catalogs":[{"id":"top","type":"movie","name":"Top"}]})) }))
            .route("/catalog/movie/top.json", get(move || { let counter = counter.clone(); async move { counter.fetch_add(1, Ordering::SeqCst); axum::Json(json!({"metas":[{"id":"tt42","type":"movie","name":"Cached"}]})) } }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
        let addons = Addons::new(
            Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            reqwest::Client::builder().no_proxy().build().unwrap(),
        )
        .unwrap();
        let saved = addons
            .add(&format!("http://{address}/manifest.json"))
            .await
            .unwrap();
        for _ in 0..2 {
            let response = addons
                .discover("movie".into(), None, saved["id"].as_i64(), 0, None, None)
                .await
                .unwrap();
            assert_eq!(response["metas"][0]["id"], "tt42");
        }
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert!(addons
            .discover("movie".into(), None, saved["id"].as_i64(), 100, None, None)
            .await
            .is_err());
        task.abort();
    }
    fn test_addons() -> Addons {
        let addons = Addons::new(
            Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            reqwest::Client::builder().no_proxy().build().unwrap(),
        )
        .unwrap();
        addons.delete(1).unwrap();
        addons
    }
    fn insert(addons: &Addons, name: &str, url: &str, manifest: Value, priority: i64) -> i64 {
        let db = addons.db.lock().unwrap();
        db.execute(
            "INSERT INTO addons(name,manifest_url,manifest,priority) VALUES(?1,?2,?3,?4)",
            params![name, url, manifest.to_string(), priority],
        )
        .unwrap();
        db.last_insert_rowid()
    }
    #[test]
    fn seed_migration_and_restarts_never_restore_deleted_addons() {
        let path = std::env::temp_dir().join(format!(
            "viptv-addon-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let open = || {
            Addons::new(
                Arc::new(Mutex::new(Connection::open(&path).unwrap())),
                reqwest::Client::new(),
            )
            .unwrap()
        };
        let addons = open();
        assert_eq!(addons.list().unwrap().as_array().unwrap().len(), 1);
        addons.update(1, json!({"enabled":false})).unwrap();
        drop(addons);
        let addons = open();
        assert_eq!(addons.list().unwrap()[0]["enabled"], false);
        assert!(addons.list().unwrap()[0]["priority"].is_null());
        addons.delete(1).unwrap();
        drop(addons);
        assert_eq!(open().list().unwrap(), json!([]));
        std::fs::remove_file(&path).unwrap();
        // Both empty and populated pre-priority schemas are already initialized.
        for populated in [false, true] {
            let db = Connection::open_in_memory().unwrap();
            db.execute_batch("CREATE TABLE addons(id INTEGER PRIMARY KEY,name TEXT NOT NULL,manifest_url TEXT UNIQUE NOT NULL,enabled INTEGER NOT NULL DEFAULT 1,manifest TEXT NOT NULL)").unwrap();
            if populated {
                db.execute(
                    "INSERT INTO addons VALUES(4,'Legacy','https://legacy/manifest.json',0,'{}')",
                    [],
                )
                .unwrap();
            }
            let db = Arc::new(Mutex::new(db));
            for _ in 0..2 {
                let addons = Addons::new(db.clone(), reqwest::Client::new()).unwrap();
                let list = addons.list().unwrap();
                assert_eq!(list.as_array().unwrap().len(), usize::from(populated));
                if populated {
                    assert!(list[0]["priority"].is_null());
                    assert_eq!(list[0]["enabled"], false);
                }
            }
        }
    }
    #[test]
    fn patch_validation_and_order() {
        let addons = test_addons();
        let a = insert(&addons, "A", "https://a/manifest.json", json!({}), 0);
        let b = insert(&addons, "B", "https://b/manifest.json", json!({}), 0);
        assert_eq!(
            addons
                .entries()
                .unwrap()
                .iter()
                .map(|e| e.0)
                .collect::<Vec<_>>(),
            vec![a, b]
        );
        for patch in [
            json!(null),
            json!([]),
            json!({}),
            json!({"enabled":1}),
            json!({"enabled":null}),
            json!({"priority":1.5}),
            json!({"priority":"2"}),
            json!({"priority":18446744073709551615u64}),
            json!({"name":"bad"}),
            json!({"enabled":false,"priority":"bad"}),
        ] {
            assert!(addons.update(a, patch).is_err());
        }
        assert!(addons.update(999, json!({"enabled":false})).is_err());
        assert_eq!(addons.entries().unwrap().len(), 2);
        assert!(addons.update(b, json!({"priority":-1})).is_err());
        assert_eq!(addons.entries().unwrap()[0].0, a);
        addons.update(b, json!({"enabled":false})).unwrap();
        assert_eq!(addons.entries().unwrap().len(), 1);
        assert_eq!(addons.list().unwrap()[1]["id"], b);
        assert_eq!(addons.list().unwrap()[1]["enabled"], false);
    }
    #[tokio::test]
    async fn ordered_catalogs_raw_pagination_search_constraints_and_meta() {
        use axum::{extract::OriginalUri, routing::get, Router};
        let paths = Arc::new(Mutex::new(Vec::<String>::new()));
        let captured = paths.clone();
        let mock = Router::new().fallback(get(move |OriginalUri(uri): OriginalUri| {
            let captured = captured.clone();
            async move {
                let path = uri.path().to_owned();
                captured.lock().unwrap().push(path.clone());
                if path == "/slow/manifest.json" {
                    return axum::Json(json!({"id":"slow","name":"Refreshed","resources":["catalog","meta"],"types":["movie"],"catalogs":[{"id":"custom","type":"movie","extra":[{"name":"skip"},{"name":"search"}]}]}));
                }
                if path.starts_with("/slow/") { tokio::time::sleep(Duration::from_millis(40)).await; }
                if path.contains("/meta/") { return axum::Json(json!({"meta":{"id":"tt1","name":if path.starts_with("/slow/") {"preferred"} else {"fast"}}})); }
                if path.contains("skip=205") { return axum::Json(json!({"metas":[]})); }
                let metas: Vec<_> = (0..205).map(|i| json!({"id":format!("tt{}",i/2),"type":"movie","name":if path.starts_with("/slow/") {"preferred"} else {"fast"}})).collect();
                axum::Json(json!({"metas":metas}))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
        let addons = test_addons();
        let manifest = json!({"resources":["catalog","meta"],"types":["movie"],"catalogs":[{"id":"custom","type":"movie","extra":[{"name":"skip"},{"name":"search"}]},{"id":"other","type":"movie","extraSupported":["search"]}]});
        let fast = insert(
            &addons,
            "Fast",
            &format!("http://{address}/fast/manifest.json"),
            manifest.clone(),
            0,
        );
        let slow_url = format!("http://{address}/slow/manifest.json");
        let slow = insert(&addons, "Slow", &slow_url, manifest, -1);
        let page = addons
            .discover("movie".into(), None, None, 0, None, None)
            .await
            .unwrap();
        assert_eq!(page["metas"].as_array().unwrap().len(), 103);
        assert_eq!(page["metas"][0]["name"], "fast");
        assert_eq!(page["next_skip"], 205);
        assert_eq!(page["has_more"], true);
        assert_eq!(
            *paths.lock().unwrap(),
            vec!["/fast/catalog/movie/custom/skip=0.json"]
        );
        paths.lock().unwrap().clear();
        let whitespace = addons
            .discover("movie".into(), None, None, 0, Some("   ".into()), None)
            .await
            .unwrap();
        assert_eq!(whitespace["aggregated"], false);
        assert_eq!(whitespace["next_skip"], 205);
        paths.lock().unwrap().clear();
        let empty = addons
            .discover("movie".into(), None, None, 205, None, None)
            .await
            .unwrap();
        assert_eq!(empty["has_more"], false);
        assert!(empty["next_skip"].is_null());
        assert_eq!(
            addons.meta("movie", "tt1").await.unwrap()["meta"]["name"],
            "fast"
        );
        paths.lock().unwrap().clear();
        let search = addons
            .discover(
                "movie".into(),
                Some("other".into()),
                Some(fast),
                0,
                Some("a/b &?%".into()),
                None,
            )
            .await
            .unwrap();
        assert_eq!(search["has_more"], false);
        assert!(search["next_skip"].is_null());
        assert_eq!(search["aggregated"], false);
        // meta() cancels its lower-priority futures once the preferred result
        // arrives, but an already-sent metadata request can reach this mock
        // after clear(). Count catalog requests, not unrelated in-flight work.
        let requests = paths
            .lock()
            .unwrap()
            .iter()
            .filter(|path| path.contains("/catalog/"))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(requests.len(), 1, "observed catalog requests: {requests:?}");
        assert!(requests[0].starts_with("/fast/catalog/movie/other/search="));
        assert!(requests[0].contains("a%2Fb+%26%3F%25"));
        assert!(addons
            .discover("movie".into(), None, None, 1, Some("x".into()), None)
            .await
            .is_err());
        assert!(addons
            .discover(
                "movie".into(),
                Some("missing".into()),
                None,
                0,
                Some("x".into()),
                None,
            )
            .await
            .is_err());
        addons.update(slow, json!({"enabled":false})).unwrap();
        let saved = addons.add(&slow_url).await.unwrap();
        assert_eq!(saved["enabled"], false);
        assert!(saved["priority"].is_null());
        assert_eq!(saved["name"], "Refreshed");
        assert_eq!(
            addons
                .discover("movie".into(), None, None, 0, None, None)
                .await
                .unwrap()["metas"][0]["name"],
            "fast"
        );
        task.abort();
    }
    #[tokio::test]
    async fn bounded_search_and_raw_count_before_truncation() {
        let addons = test_addons();
        let base = "http://127.0.0.1:1/manifest.json";
        let catalogs: Vec<_> = (0..40).map(|i| json!({"id":format!("c{i}"),"type":"movie","extra":[{"name":"search"},{"name":"skip"}]})).collect();
        let id = insert(
            &addons,
            "Many",
            base,
            json!({"resources":["catalog"],"catalogs":catalogs}),
            0,
        );
        for i in 0..32 {
            let endpoint =
                Addons::extra_endpoint(base, "movie", &format!("c{i}"), "skip=0&search=x").unwrap();
            let metas: Vec<_> = (0..210)
                .map(|j| json!({"id":format!("{i}-{j}"),"type":"movie"}))
                .collect();
            addons
                .cache
                .lock()
                .unwrap()
                .insert(endpoint, (now() + 300, json!({"metas":metas}), 1));
        }
        let search = addons
            .discover("movie".into(), None, Some(id), 0, Some("x".into()), None)
            .await
            .unwrap();
        assert_eq!(search["metas"].as_array().unwrap().len(), 200);
        assert_eq!(search["metas"][0]["id"], "0-0");
        assert_eq!(search["metas"][199]["id"], "0-199");
        assert_eq!(search["has_more"], false);
        let endpoint = Addons::extra_endpoint(base, "movie", "c0", "skip=0").unwrap();
        let metas: Vec<_> = (0..250).map(|j| json!({"id":j,"type":"movie"})).collect();
        addons
            .cache
            .lock()
            .unwrap()
            .insert(endpoint, (now() + 300, json!({"metas":metas}), 1));
        let page = addons
            .discover("movie".into(), None, None, 0, None, None)
            .await
            .unwrap();
        assert_eq!(page["metas"].as_array().unwrap().len(), 200);
        assert_eq!(page["next_skip"], 250);
        assert!(addons
            .discover("movie".into(), None, None, 10001, None, None)
            .await
            .is_err());
    }
    #[test]
    fn catalogs_preserve_bounded_normalized_extra_capabilities() {
        let addons = test_addons();
        let mut options = (0..70)
            .map(|index| Value::String(format!("Genre {index}")))
            .collect::<Vec<_>>();
        options[0] = Value::String("x".repeat(200));
        insert(
            &addons,
            "Capabilities",
            "http://127.0.0.1:1/manifest.json",
            json!({
                "resources":["catalog"],
                "catalogs":[{
                    "id":"discover",
                    "type":"movie",
                    "name":"Discover movies",
                    "extra":[
                        {"name":"genre","isRequired":true,"options":options,"optionsLimit":5000},
                        "search"
                    ],
                    "extraSupported":["skip","genre","not valid"],
                    "extraRequired":["search"],
                    "genres":["Legacy genre"]
                }]
            }),
            0,
        );
        let catalogs = addons.catalogs().unwrap();
        let catalog = &catalogs[0];
        assert_eq!(catalog["supports_search"], true);
        assert_eq!(catalog["supports_skip"], true);
        assert_eq!(catalog["genres"].as_array().unwrap().len(), 70);
        assert_eq!(catalog["extra"].as_array().unwrap().len(), 3);
        let genre = catalog["extra"]
            .as_array()
            .unwrap()
            .iter()
            .find(|extra| extra["name"] == "genre")
            .unwrap();
        assert_eq!(genre["is_required"], true);
        assert_eq!(genre["options_limit"], 1000);
        assert_eq!(genre["options"][0], "Genre 1");
        assert!(!genre["options"]
            .as_array()
            .unwrap()
            .iter()
            .any(|option| option.as_str().unwrap().chars().count() > MAX_EXTRA_OPTION));
        let search = catalog["extra"]
            .as_array()
            .unwrap()
            .iter()
            .find(|extra| extra["name"] == "search")
            .unwrap();
        assert_eq!(search["is_required"], true);
        assert!(search["options"].as_array().unwrap().is_empty());
    }
    #[tokio::test]
    async fn genre_discover_requires_advertisement_validates_options_and_encodes_path() {
        let addons = test_addons();
        let base = "http://127.0.0.1:1/manifest.json";
        let addon = insert(
            &addons,
            "Genres",
            base,
            json!({
                "resources":["catalog"],
                "catalogs":[{
                    "id":"discover",
                    "type":"movie",
                    "extra":[{"name":"genre","isRequired":true,"options":["Family & Kids","Comedy"]},{"name":"skip"}]
                }]
            }),
            0,
        );
        let endpoint =
            Addons::extra_endpoint(base, "movie", "discover", "skip=0&genre=Family+%26+Kids")
                .unwrap();
        addons.cache.lock().unwrap().insert(
            endpoint,
            (
                now() + 300,
                json!({"metas":[{"id":"tt-kids","type":"movie","name":"Kids"}]}),
                1,
            ),
        );
        let result = addons
            .discover(
                "movie".into(),
                Some("discover".into()),
                Some(addon),
                0,
                None,
                Some("Family & Kids".into()),
            )
            .await
            .unwrap();
        assert_eq!(result["metas"][0]["id"], "tt-kids");
        let invalid = addons
            .discover(
                "movie".into(),
                Some("discover".into()),
                Some(addon),
                0,
                None,
                Some("Horror".into()),
            )
            .await
            .unwrap_err();
        assert!(invalid.contains("advertised options"));
        let missing = addons
            .discover(
                "movie".into(),
                Some("discover".into()),
                Some(addon),
                0,
                None,
                None,
            )
            .await
            .unwrap_err();
        assert!(missing.contains("No catalog source"));

        let unsupported = test_addons();
        let unsupported_id = insert(
            &unsupported,
            "No genres",
            base,
            json!({"resources":["catalog"],"catalogs":[{"id":"top","type":"movie"}]}),
            0,
        );
        let error = unsupported
            .discover(
                "movie".into(),
                Some("top".into()),
                Some(unsupported_id),
                0,
                None,
                Some("Comedy".into()),
            )
            .await
            .unwrap_err();
        assert!(error.contains("does not advertise genre"));
    }
    #[tokio::test]
    async fn metadata_falls_back_in_priority_order() {
        let addons = test_addons();
        for (i, response) in [
            json!({"meta":null}),
            json!({"meta":{"name":"second"}}),
            json!({"meta":{"name":"third"}}),
        ]
        .into_iter()
        .enumerate()
        {
            let base = format!("http://127.0.0.1:1/{i}/manifest.json");
            insert(
                &addons,
                "Meta",
                &base,
                json!({"resources":["meta"],"types":["movie"],"idPrefixes":["tt"]}),
                i as i64,
            );
            let endpoint = Addons::endpoint(&base, &["meta", "movie", "tt1.json"]).unwrap();
            addons
                .cache
                .lock()
                .unwrap()
                .insert(endpoint, (now() + 300, response, 1));
        }
        assert_eq!(
            addons.meta("movie", "tt1").await.unwrap()["meta"]["name"],
            "second"
        );
        assert!(addons.meta("movie", "other").await.is_err());
    }
    #[test]
    fn root_prefixes_and_search_extras() {
        assert!(!supports(
            &json!({"resources":[{"name":"meta","idPrefixes":["abc"]}],"idPrefixes":["tt"]}),
            "meta",
            "movie",
            "abc1"
        ));
        assert!(!supports(
            &json!({"resources":[{"name":"meta","idPrefixes":["tt1"]}],"idPrefixes":["tt"]}),
            "meta",
            "movie",
            "tt9"
        ));
        for resources in [
            json!(["meta"]),
            json!([{"name":"meta","types":["movie"],"idPrefixes":["tt1"]}]),
        ] {
            let m = json!({"resources":resources,"types":["movie"],"idPrefixes":["tt"]});
            assert!(supports(&m, "meta", "movie", "tt123"));
            assert!(!supports(&m, "meta", "movie", "other"));
        }
        assert!(!supports(
            &json!({"resources":["meta"],"idPrefixes":[]}),
            "meta",
            "movie",
            "tt1"
        ));
        assert!(catalog_extra(
            &json!({"extra":[{"name":"search"}]}),
            "search"
        ));
        assert!(catalog_extra(
            &json!({"extraSupported":["search"]}),
            "search"
        ));
        assert!(!catalog_extra(
            &json!({"extra":[{"name":"genre"}]}),
            "search"
        ));
    }
    #[test]
    fn respects_resource_prefixes() {
        assert!(!supports(
            &json!({"resources":[{"name":"stream","idPrefixes":["tt"],"types":["movie"]}]}),
            "stream",
            "movie",
            "abc"
        ));
    }
    #[tokio::test]
    async fn custom_catalog_types_options_and_explicit_search_pages_work() {
        let addons = test_addons();
        let base = "http://127.0.0.1:1/manifest.json";
        let languages: Vec<_> = (0..186).map(|i| format!("Language {i}")).collect();
        let addon = insert(
            &addons,
            "Expanded metadata",
            base,
            json!({"resources":["catalog"],"catalogs":[
                {"id":"search.anime","type":"anime.series","extra":[{"name":"search","isRequired":true},{"name":"skip"}]},
                {"id":"calendar","type":"series","extra":[{"name":"calendarVideosIds","isRequired":true}]},
                {"id":"languages","type":"movie","extra":[{"name":"genre","isRequired":true,"options":languages,"default":"Language 100"}]}
            ]}),
            0,
        );
        let catalogs = addons.catalogs().unwrap();
        assert_eq!(catalogs[2]["genres"].as_array().unwrap().len(), 186);
        assert_eq!(catalogs[2]["extra"][0]["default"], "Language 100");
        let endpoint = Addons::extra_endpoint(
            base,
            "anime.series",
            "search.anime",
            "skip=40&search=Naruto",
        )
        .unwrap();
        addons.cache.lock().unwrap().insert(
            endpoint,
            (
                now() + 300,
                json!({"metas":[{"id":"tt0409591","type":"series"}]}),
                1,
            ),
        );
        let page = addons
            .discover(
                "anime.series".into(),
                Some("search.anime".into()),
                Some(addon),
                40,
                Some("Naruto".into()),
                None,
            )
            .await
            .unwrap();
        assert_eq!(page["metas"][0]["type"], "series");
        assert_eq!(page["next_skip"], 41);
        assert_eq!(page["aggregated"], false);
        let request = |extras| DiscoverOptions {
            kind: "series".into(),
            catalog: Some("calendar".into()),
            addon: Some(addon),
            skip: 0,
            search: None,
            genre: None,
            extras,
        };
        let endpoint = Addons::extra_endpoint(
            base,
            "series",
            "calendar",
            "calendarVideosIds=tt123%3A1%3A2%26x",
        )
        .unwrap();
        addons
            .cache
            .lock()
            .unwrap()
            .insert(endpoint, (now() + 300, json!({"metas":[]}), 1));
        let result = addons
            .discover_with_options(request(HashMap::from([(
                "calendarVideosIds".into(),
                "tt123:1:2&x".into(),
            )])))
            .await
            .unwrap();
        assert_eq!(result["has_more"], false);
        assert!(addons
            .discover_with_options(request(HashMap::new()))
            .await
            .is_err());
        assert!(addons
            .discover_with_options(request(HashMap::from([("unknown".into(), "x".into())])))
            .await
            .unwrap_err()
            .contains("not advertised"));
        assert!(addons
            .discover_with_options(request(HashMap::from([("skip".into(), "99".into())])))
            .await
            .is_err());
    }
    #[test]
    fn episode_art_enrichment_preserves_identity_and_requires_matching_episode() {
        let mut primary = json!({"id":"tt5607616","videos":[{"id":"tt5607616:4:1","season":4,"episode":1,"released":"2026-04-08T00:00:00Z","thumbnail":"https://episodes.metahub.space/tt5607616/4/1/w780.jpg"}]});
        let alternate = json!({"id":"tt5607616","videos":[{"id":"kitsu:49746:1","season":4,"episode":1,"released":"2026-04-08T13:30:00Z","thumbnail":"https://api.top-posters.com/private/asset?fallback_url=https%3A%2F%2Fartworks.thetvdb.com%2Fepisode.jpg"}]});
        enrich_episode_art(&mut primary, &alternate);
        assert_eq!(primary["videos"][0]["id"], "tt5607616:4:1");
        let proxy = episode_art_url(&primary["videos"][0]["thumbnail"]).unwrap();
        assert!(proxy.starts_with("https://wsrv.nl/?"));
        assert!(proxy.contains("w=512") && !proxy.contains("private"));
        let mut wrong = primary.clone();
        wrong["videos"][0]["thumbnail"] = json!("");
        wrong["videos"][0]["released"] = json!("2026-04-09T00:00:00Z");
        enrich_episode_art(&mut wrong, &alternate);
        assert_eq!(wrong["videos"][0]["thumbnail"], "");
        for url in [
            "http://127.0.0.1/image.jpg",
            "https://provider.invalid/private.jpg",
            "https://artworks.thetvdb.com/image?token=secret",
        ] {
            assert!(episode_art_url(&json!(url)).is_none());
        }
    }
}
