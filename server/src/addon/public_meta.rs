use super::*;
use rusqlite::OptionalExtension;
use std::future::Future;

const MAX_ENTRY: usize = 8 * 1024 * 1024;
const MAX_BYTES: i64 = 256 * 1024 * 1024;
const MAX_ENTRIES: i64 = 10_000;
type Flight = tokio::sync::OnceCell<Result<Value, String>>;

pub(super) fn identity(raw: &str) -> Option<(String, String)> {
    let url = url::Url::parse(raw).ok()?;
    if url.scheme() != "https" || url.host_str()? != "v3-cinemeta.strem.io"
        || url.port().is_some() || !url.username().is_empty() || url.password().is_some()
        || url.query().is_some() || url.fragment().is_some()
    {
        return None;
    }
    let parts: Vec<_> = url.path().split('/').collect();
    if parts.len() != 4 || parts[1] != "meta" || !matches!(parts[2], "movie" | "series") {
        return None;
    }
    let id = parts[3].strip_suffix(".json")?;
    let digits = id.strip_prefix("tt")?;
    if digits.is_empty() || digits.len() > 16 || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((parts[2].to_owned(), id.to_owned()))
}

pub(super) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS public_metadata_cache_v1(url TEXT PRIMARY KEY,body TEXT NOT NULL,bytes INTEGER NOT NULL,refresh_at INTEGER NOT NULL,expires_at INTEGER NOT NULL,stored_at INTEGER NOT NULL);CREATE INDEX IF NOT EXISTS public_metadata_cache_expiry ON public_metadata_cache_v1(expires_at);")
}

pub(super) struct Cache {
    db: Arc<Mutex<Connection>>,
    flights: Mutex<HashMap<String, Weak<Flight>>>,
    retries: Mutex<HashMap<String, i64>>,
}

impl Cache {
    pub(super) fn new(db: Arc<Mutex<Connection>>) -> Self {
        Self { db, flights: Default::default(), retries: Default::default() }
    }

    fn read(&self, url: &str) -> Option<(Value, i64)> {
        let (body, refresh): (String, i64) = self.db.lock().ok()?.query_row(
            "SELECT body,refresh_at FROM public_metadata_cache_v1 WHERE url=?1 AND expires_at>?2",
            params![url, now()], |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional().ok()??;
        let value = serde_json::from_str(&body).ok()?;
        valid(url, &value).then_some((value, refresh))
    }

    fn store(&self, url: &str, value: &Value) -> Result<(), String> {
        if !valid(url, value) { return Err("Invalid public metadata".into()); }
        let body = serde_json::to_string(value).map_err(|_| "Invalid public metadata")?;
        if body.len() > MAX_ENTRY { return Err("Public metadata too large".into()); }
        let (fresh, stale) = lifetime(value);
        let mut db = self.db.lock().map_err(|_| "Database unavailable")?;
        let tx = db.transaction().map_err(|_| "Metadata cache unavailable")?;
        tx.execute("DELETE FROM public_metadata_cache_v1 WHERE expires_at<=?1", [now()]).map_err(|_| "Metadata cache unavailable")?;
        tx.execute("INSERT OR REPLACE INTO public_metadata_cache_v1(url,body,bytes,refresh_at,expires_at,stored_at) VALUES(?1,?2,?3,?4,?5,?6)", params![url,body,body.len() as i64,now()+fresh,now()+stale,now()]).map_err(|_| "Metadata cache unavailable")?;
        let (mut count, mut bytes): (i64, i64) = tx.query_row("SELECT COUNT(*),COALESCE(SUM(bytes),0) FROM public_metadata_cache_v1", [], |r| Ok((r.get(0)?,r.get(1)?))).map_err(|_| "Metadata cache unavailable")?;
        while count > MAX_ENTRIES || bytes > MAX_BYTES {
            let (key, size): (String, i64) = tx.query_row("SELECT url,bytes FROM public_metadata_cache_v1 ORDER BY stored_at,url LIMIT 1", [], |r| Ok((r.get(0)?,r.get(1)?))).map_err(|_| "Metadata cache unavailable")?;
            tx.execute("DELETE FROM public_metadata_cache_v1 WHERE url=?1", [key]).map_err(|_| "Metadata cache unavailable")?;
            count -= 1; bytes -= size;
        }
        tx.commit().map_err(|_| "Metadata cache unavailable".into())
    }

    pub(super) async fn load<F, Fut>(self: &Arc<Self>, url: String, fetch: F) -> Result<Value, String>
    where F: FnOnce() -> Fut + Send + 'static, Fut: Future<Output = Result<Value, String>> + Send + 'static {
        if identity(&url).is_none() { return Err("Not public metadata".into()); }
        let hit = self.read(&url);
        if let Some((value, refresh)) = &hit {
            if *refresh > now() { return Ok(value.clone()); }
            if self.retries.lock().unwrap().get(&url).is_some_and(|retry| *retry > now()) {
                return Ok(value.clone());
            }
        }
        let flight = {
            let mut flights = self.flights.lock().unwrap();
            flights.retain(|_, flight| flight.strong_count() > 0);
            flights.entry(url.clone()).or_default().upgrade().unwrap_or_else(|| {
                let flight = Arc::new(Flight::new());
                flights.insert(url.clone(), Arc::downgrade(&flight));
                flight
            })
        };
        let cache = self.clone();
        let update = async move {
            let result = fetch().await;
            match result {
                Ok(value) if valid(&url, &value) => {
                    // Cache persistence must never make an otherwise valid response fail.
                    let _ = cache.store(&url, &value);
                    cache.retries.lock().unwrap().remove(&url);
                    Ok(value)
                }
                result => {
                    let mut retries = cache.retries.lock().unwrap();
                    retries.retain(|_, retry| *retry > now());
                    if retries.len() >= 256 { retries.clear(); }
                    retries.insert(url, now() + 60);
                    Err(result.err().unwrap_or_else(|| "Invalid public metadata".into()))
                }
            }
        };
        if let Some((value, _)) = hit {
            tokio::spawn(async move { flight.get_or_init(|| update).await; });
            Ok(value)
        } else {
            flight.get_or_init(|| update).await.clone()
        }
    }
}

fn valid(url: &str, value: &Value) -> bool {
    let Some((kind, id)) = identity(url) else { return false; };
    let meta = &value["meta"];
    meta["id"].as_str() == Some(id.as_str()) && meta["type"].as_str() == Some(kind.as_str())
        && meta["name"].as_str().is_some_and(|name| !name.trim().is_empty())
}

fn lifetime(value: &Value) -> (i64, i64) {
    let series = value["meta"]["type"] == "series";
    let incomplete = !value["meta"]["description"].as_str().is_some_and(|s| !s.trim().is_empty());
    (if series || incomplete { 15 * 60 } else { 24 * 3600 }, if series { 7 * 86400 } else { 30 * 86400 })
}

#[cfg(test)]
mod tests;
