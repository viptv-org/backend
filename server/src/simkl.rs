//! Experimental SIMKL integration. Public cache is shared; credentials and sync state are profile-scoped.
use crate::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use viptv_simkl::{http::Client, Category};
mod http;
mod sync;
#[cfg(test)]
mod tests;
pub(crate) use http::*;

pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(r#"
    CREATE TABLE IF NOT EXISTS simkl_connections(profile_id INTEGER PRIMARY KEY REFERENCES profiles(id) ON DELETE CASCADE,account_id INTEGER NOT NULL,user_id TEXT NOT NULL UNIQUE,user_name TEXT NOT NULL,tokens TEXT NOT NULL,expires INTEGER NOT NULL,snapshot TEXT,last_sync INTEGER NOT NULL DEFAULT 0,error TEXT,counts TEXT NOT NULL DEFAULT '{}',generation TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS simkl_oauth(state TEXT PRIMARY KEY,profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,account_id INTEGER NOT NULL,session_id TEXT NOT NULL,verifier TEXT NOT NULL,expires INTEGER NOT NULL);
    CREATE TABLE IF NOT EXISTS simkl_cache(key TEXT PRIMARY KEY,value TEXT NOT NULL,expires INTEGER NOT NULL);
    CREATE TABLE IF NOT EXISTS simkl_items(id TEXT PRIMARY KEY,value TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS simkl_aliases(alias TEXT NOT NULL,kind TEXT NOT NULL,id TEXT NOT NULL,PRIMARY KEY(alias,kind,id));
    CREATE TABLE IF NOT EXISTS simkl_library(profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,id TEXT NOT NULL,value TEXT NOT NULL,PRIMARY KEY(profile_id,id));
    CREATE TABLE IF NOT EXISTS simkl_exports(profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,key TEXT NOT NULL,PRIMARY KEY(profile_id,key));
    CREATE TABLE IF NOT EXISTS simkl_outbox(profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,id TEXT NOT NULL,action TEXT NOT NULL,value TEXT NOT NULL,PRIMARY KEY(profile_id,id,action));
    CREATE TABLE IF NOT EXISTS simkl_events(profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,event_id TEXT NOT NULL,session_id TEXT NOT NULL,action TEXT NOT NULL,created INTEGER NOT NULL,PRIMARY KEY(profile_id,event_id));
    CREATE TABLE IF NOT EXISTS simkl_user_cache(profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,key TEXT NOT NULL,marker TEXT NOT NULL,value TEXT NOT NULL,PRIMARY KEY(profile_id,key));
    CREATE TABLE IF NOT EXISTS simkl_experiment(version INTEGER PRIMARY KEY);
    "#)?;
    // A one-time experimental migration strips descriptive addon data, retaining user facts.
    if db.query_row("SELECT count(*) FROM simkl_experiment", [], |r| {
        r.get::<_, i64>(0)
    })? == 0
    {
        let tx = db.unchecked_transaction()?;
        tx.execute(
            "UPDATE favorites SET name=id,poster=NULL WHERE type!='live'",
            [],
        )?;
        tx.execute("UPDATE progress SET name=id,poster=NULL,context=json_remove(context,'$.description','$.background','$.thumbnail','$.genres','$.imdbRating','$.stremio_import_watched','$.stremio_completion_only','$.stremio_watch_date_unknown') WHERE type!='live'",[])?;
        tx.execute("DELETE FROM continuation_cache", [])?;
        tx.execute_batch("DROP TABLE IF EXISTS stremio_import_receipts")?;
        tx.execute("INSERT INTO simkl_experiment VALUES(1)", [])?;
        tx.commit()?;
    }
    Ok(())
}
#[derive(Clone)]
pub(crate) struct Service {
    pub(crate) db: Arc<Mutex<Connection>>,
    client: Option<Client>,
    pub(crate) vault: Option<Arc<secret_store::Vault>>,
    pub(crate) gate: Arc<tokio::sync::Mutex<()>>,
}
impl Service {
    pub(crate) fn new(db: Arc<Mutex<Connection>>) -> Result<Self, String> {
        let client = std::env::var("SIMKL_CLIENT_ID")
            .ok()
            .filter(|s| !s.is_empty())
            .map(|id| Client::new(id, std::env::var("SIMKL_CLIENT_SECRET").unwrap_or_default()))
            .transpose()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            db,
            client,
            vault: None,
            gate: Default::default(),
        })
    }
    fn client(&self) -> Result<&Client, String> {
        self.client
            .as_ref()
            .ok_or_else(|| "SIMKL is not configured".into())
    }
    pub(crate) fn connected(&self, profile: i64) -> bool {
        self.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM simkl_connections WHERE profile_id=?1)",
                [profile],
                |r| r.get(0),
            )
            .unwrap_or(false)
    }
    fn generation(&self, profile: i64) -> Result<String, String> {
        self.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT generation FROM simkl_connections WHERE profile_id=?1",
                [profile],
                |r| r.get(0),
            )
            .map_err(|_| "Link SIMKL to use this feature".into())
    }
    fn current(&self, profile: i64, generation: &str) -> Result<(), String> {
        if self.generation(profile)? == generation {
            Ok(())
        } else {
            Err("SIMKL connection changed".into())
        }
    }
    async fn token(&self, profile: i64) -> Result<String, String> {
        let (account,sealed,expires,generation):(i64,String,i64,String)=self.db.lock().unwrap().query_row("SELECT account_id,tokens,expires,generation FROM simkl_connections WHERE profile_id=?1",[profile],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).map_err(|_|"Link SIMKL to use this feature")?;
        let vault = self
            .vault
            .as_ref()
            .ok_or("SIMKL encrypted storage unavailable")?;
        let plain = vault
            .open(account, "simkl", &profile.to_string(), &sealed)
            .map_err(|_| "SIMKL tokens unavailable")?;
        let mut tokens: Value =
            serde_json::from_slice(plain.expose()).map_err(|_| "SIMKL tokens unavailable")?;
        if expires <= util::now() + 86400 {
            tokens = self
                .client()?
                .token(&[
                    ("grant_type", "refresh_token".into()),
                    (
                        "refresh_token",
                        tokens["refresh_token"]
                            .as_str()
                            .ok_or("Reconnect SIMKL")?
                            .into(),
                    ),
                ])
                .await
                .map_err(|_| "Reconnect SIMKL")?;
            let encrypted = vault
                .seal(
                    account,
                    "simkl",
                    &profile.to_string(),
                    tokens.to_string().as_bytes(),
                )
                .map_err(|_| "SIMKL token storage failed")?;
            self.current(profile, &generation)?;
            self.db.lock().unwrap().execute("UPDATE simkl_connections SET tokens=?1,expires=?2 WHERE profile_id=?3 AND generation=?4",params![encrypted,util::now()+tokens["expires_in"].as_i64().unwrap_or(604800),profile,generation]).map_err(|_|"SIMKL token storage failed")?;
        }
        tokens["access_token"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| "Reconnect SIMKL".into())
    }
    async fn user_get(&self, profile: i64, path: &str) -> Result<Value, String> {
        let token = self.token(profile).await?;
        match self.client()?.get(path, Some(&token)).await {
            Err(e) if e.status == 401 => {
                self.db
                    .lock()
                    .unwrap()
                    .execute(
                        "UPDATE simkl_connections SET expires=0 WHERE profile_id=?1",
                        [profile],
                    )
                    .map_err(|_| "SIMKL token refresh failed")?;
                let token = self.token(profile).await?;
                self.client()?
                    .get(path, Some(&token))
                    .await
                    .map_err(|e| e.to_string())
            }
            result => result.map_err(|e| e.to_string()),
        }
    }
    pub(crate) async fn custom(&self, p: i64, path: &str) -> Result<Value, String> {
        let activities = self.user_get(p, "/sync/activities").await?;
        let marker = json!([
            activities["custom_lists"]["lists"]["all"],
            activities["settings"]["all"]
        ])
        .to_string();
        let cached = self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT value FROM simkl_user_cache WHERE profile_id=?1 AND key=?2 AND marker=?3",
                params![p, path, marker],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(|_| "SIMKL list cache unavailable")?;
        if let Some(value) = cached {
            return serde_json::from_str(&value).map_err(|_| "SIMKL list cache invalid".into());
        }
        let value = self.user_get(p, path).await?;
        self.db.lock().unwrap().execute("INSERT INTO simkl_user_cache VALUES(?1,?2,?3,?4) ON CONFLICT(profile_id,key) DO UPDATE SET marker=excluded.marker,value=excluded.value",params![p,path,marker,value.to_string()]).map_err(|_|"SIMKL list cache unavailable")?;
        Ok(value)
    }
    async fn public(&self, path: &str, ttl: i64) -> Result<Value, String> {
        if let Some(value) = self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT value FROM simkl_cache WHERE key=?1 AND expires>?2",
                params![path, util::now()],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(|_| "SIMKL cache unavailable")?
        {
            return serde_json::from_str(&value).map_err(|_| "SIMKL cache invalid".into());
        }
        let v = self
            .client()?
            .get(path, None)
            .await
            .map_err(|e| e.to_string())?;
        self.db.lock().unwrap().execute("INSERT INTO simkl_cache VALUES(?1,?2,?3) ON CONFLICT(key) DO UPDATE SET value=excluded.value,expires=excluded.expires",params![path,v.to_string(),util::now()+ttl]).map_err(|_|"SIMKL cache unavailable")?;
        Ok(v)
    }
    fn remember(&self, items: &[Value]) -> Result<(), String> {
        let db = self.db.lock().unwrap();
        let tx = db
            .unchecked_transaction()
            .map_err(|_| "SIMKL cache unavailable")?;
        for v in items {
            let id = v["id"].as_str().ok_or("SIMKL identity unavailable")?;
            let mut merged = tx
                .query_row("SELECT value FROM simkl_items WHERE id=?1", [id], |r| {
                    r.get::<_, String>(0)
                })
                .optional()
                .map_err(|_| "SIMKL cache unavailable")?
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .unwrap_or(json!({}));
            for (key, value) in v.as_object().ok_or("SIMKL item invalid")? {
                if !value.is_null() {
                    merged[key] = value.clone();
                }
            }
            tx.execute("INSERT INTO simkl_items VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET value=excluded.value",params![id,merged.to_string()]).map_err(|_|"SIMKL cache unavailable")?;
            if let Some(imdb) = v["simkl_ids"]["imdb"].as_str() {
                let alias = if v["episode"].is_null() {
                    Some(imdb.to_owned())
                } else {
                    viptv_simkl::stream_id(v).ok()
                };
                if let Some(alias) = alias {
                    tx.execute(
                        "INSERT OR IGNORE INTO simkl_aliases VALUES(?1,?2,?3)",
                        params![alias, v["type"].as_str(), id],
                    )
                    .map_err(|_| "SIMKL cache unavailable")?;
                }
            }
        }
        tx.commit().map_err(|_| "SIMKL cache unavailable".into())
    }
    pub(crate) fn cached_item(&self, id: &str) -> Option<Value> {
        self.db
            .lock()
            .ok()?
            .query_row("SELECT value FROM simkl_items WHERE id=?1", [id], |r| {
                r.get::<_, String>(0)
            })
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    }
    pub(crate) async fn meta(
        &self,
        kind: &str,
        id: &str,
        profile: Option<i64>,
    ) -> Result<Value, String> {
        let _guard = self.gate.lock().await;
        self.meta_inner(kind, id, profile).await
    }
    async fn meta_inner(
        &self,
        _kind: &str,
        id: &str,
        profile: Option<i64>,
    ) -> Result<Value, String> {
        let alias = {
            let db = self.db.lock().unwrap();
            let matches = db
                .prepare("SELECT id FROM simkl_aliases WHERE alias=?1 AND kind=?2")
                .map_err(|_| "SIMKL cache unavailable")?
                .query_map(params![id, _kind], |r| r.get::<_, String>(0))
                .map_err(|_| "SIMKL cache unavailable")?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| "SIMKL cache unavailable")?;
            if matches.len() == 1 {
                Some(matches[0].clone())
            } else {
                None
            }
        };
        let id = alias.as_deref().unwrap_or(id);
        let (category, n) = if let Some(p) = viptv_simkl::parse_id(id) {
            p
        } else {
            let profile = profile.ok_or("SIMKL mapping unavailable")?;
            let token = self.token(profile).await?;
            let external = id.split(':').next().unwrap_or(id);
            if !external.starts_with("tt") {
                return Err("SIMKL mapping unavailable".into());
            }
            let (kind, n) = self
                .client()?
                .resolve(&[("imdb", external.into())], &token)
                .await
                .map_err(|e| e.to_string())?;
            (
                match kind.as_str() {
                    "movies" => Category::Movie,
                    "anime" => Category::Anime,
                    _ => Category::Tv,
                },
                n,
            )
        };
        let raw = self
            .public(
                &format!("/{}/{n}?extended=full_anime_seasons", category.endpoint()),
                21600,
            )
            .await?;
        let mut item =
            viptv_simkl::normalize(&raw, category.clone()).ok_or("Invalid SIMKL metadata")?;
        if category != Category::Movie {
            let episodes = self
                .public(
                    &format!(
                        "/{}/episodes/{n}?extended=full_anime_seasons",
                        category.endpoint()
                    ),
                    21600,
                )
                .await?;
            let videos: Vec<_> = episodes
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|e| viptv_simkl::episode(&item, e))
                .collect();
            self.remember(&videos)?;
            item["videos"] = json!(videos);
        }
        self.remember(&[item.clone()])?;
        Ok(json!({"meta":item}))
    }
    pub(crate) async fn mapped(
        &self,
        id: &str,
        kind: &str,
        context: &Value,
        profile: Option<i64>,
    ) -> Result<Value, String> {
        let mut item = if let Some(v) = self.cached_item(id) {
            v
        } else {
            self.meta(kind, id, profile).await?["meta"].clone()
        };
        if let Some((_, _)) = viptv_simkl::parse_id(id) {
            let parts: Vec<_> = id.split(':').collect();
            if parts.len() == 5 {
                let s: u64 = parts[3].parse().map_err(|_| "Invalid SIMKL episode")?;
                let e: u64 = parts[4].parse().map_err(|_| "Invalid SIMKL episode")?;
                if item["episode"].is_null() {
                    item = item["videos"]
                        .as_array()
                        .and_then(|v| v.iter().find(|v| v["season"] == s && v["episode"] == e))
                        .cloned()
                        .ok_or("SIMKL episode mapping unavailable")?;
                }
            }
        }
        for key in ["season", "episode", "series_id"] {
            if !context[key].is_null() {
                item[key] = context[key].clone();
            }
        }
        if viptv_simkl::stream_id(&item).is_err() {
            let parent = if let Some((c, n)) = viptv_simkl::parse_id(id) {
                format!("simkl:{}:{n}", c.endpoint())
            } else {
                id.to_owned()
            };
            let detail = self.meta(kind, &parent, profile).await?["meta"].clone();
            item = if item["episode"].is_null() {
                detail
            } else {
                detail["videos"]
                    .as_array()
                    .and_then(|v| {
                        v.iter().find(|v| {
                            v["season"] == item["season"] && v["episode"] == item["episode"]
                        })
                    })
                    .cloned()
                    .ok_or("SIMKL episode mapping unavailable")?
            };
        }
        Ok(item)
    }
    pub(crate) async fn discover(
        &self,
        r: addon::DiscoverOptions,
        profile: Option<i64>,
    ) -> Result<Value, String> {
        let _guard = self.gate.lock().await;
        let catalog = r.catalog.as_deref().unwrap_or("today");
        let category = match r.kind.as_str() {
            "movie" => Category::Movie,
            "anime" => Category::Anime,
            _ => {
                if catalog.starts_with("anime-") {
                    Category::Anime
                } else {
                    Category::Tv
                }
            }
        };
        let category = if catalog.starts_with("anime-") {
            Category::Anime
        } else {
            category
        };
        let catalog = catalog.strip_prefix("anime-").unwrap_or(catalog);
        let linked = profile.is_some_and(|p| self.connected(p));
        let term = r.search.as_deref().unwrap_or("").trim();
        let mut more = false;
        let mut upstream_page = false;
        let mut items: Vec<Value> = if !term.is_empty() && linked {
            let sort = r
                .extras
                .get("sort")
                .map(String::as_str)
                .unwrap_or("relevance");
            let allowed = [
                "relevance",
                "release-date",
                "last-air-date",
                "title",
                "rank",
            ];
            if !allowed.contains(&sort) || (sort == "last-air-date" && category == Category::Movie)
            {
                return Err("Unsupported SIMKL search sort".into());
            }
            let mut url = url::Url::parse("https://unused/search").unwrap();
            url.query_pairs_mut()
                .append_pair("q", term)
                .append_pair("extended", "full")
                .append_pair("limit", "50")
                .append_pair("page", &(r.skip / 50 + 1).to_string())
                .append_pair("sort", sort);
            let raw = self
                .user_get(
                    profile.unwrap(),
                    &format!(
                        "/search/{}?{}",
                        if category == Category::Movie {
                            "movie"
                        } else {
                            category.endpoint()
                        },
                        url.query().unwrap()
                    ),
                )
                .await?;
            let rows = raw.as_array().ok_or("Invalid SIMKL search")?;
            more = rows.len() == 50 && r.skip / 50 < 19;
            upstream_page = true;
            rows.iter()
                .filter_map(|v| viptv_simkl::normalize(v, category.clone()))
                .collect()
        } else if [
            "calendar",
            "premieres",
            "upcoming",
            "new-episodes",
            "my-calendar",
        ]
        .contains(&catalog)
        {
            let t = if category == Category::Movie {
                "movie_release"
            } else {
                category.endpoint()
            };
            let month = r.extras.get("month").filter(|s| {
                s.len() == 7
                    && s.as_bytes()[4] == b'-'
                    && s.bytes().filter(|b| *b != b'-').all(|b| b.is_ascii_digit())
            });
            let path = month
                .map(|s| format!("/calendar/v2/{}/{}/{t}.json", &s[..4], &s[5..]))
                .unwrap_or_else(|| format!("/calendar/v2/{t}.json"));
            let raw = self.public(&path, 18000).await?;
            let date = r
                .extras
                .get("date")
                .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
            raw["calendar"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|event| {
                    let key = event["simkl_id"].as_u64()?.to_string();
                    let mut source = raw["metadata"][&key].clone();
                    source["ids"]["simkl_id"] = event["simkl_id"].clone();
                    let timestamp =
                        chrono::DateTime::parse_from_rfc3339(event["date"].as_str()?).ok()?;
                    let timezone = self
                        .profile_timezone(profile)
                        .or_else(|| {
                            r.extras
                                .get("timezone")
                                .and_then(|s| s.parse::<chrono_tz::Tz>().ok())
                        })
                        .unwrap_or(chrono_tz::America::Detroit);
                    let day = timestamp.with_timezone(&timezone).date_naive();
                    let local_today =
                        chrono::DateTime::<chrono::Utc>::from_timestamp(util::now(), 0)
                            .unwrap()
                            .with_timezone(&timezone)
                            .date_naive();
                    if date.is_some_and(|d| d != day)
                        || (catalog == "new-episodes" && day != local_today)
                        || (catalog == "upcoming" && day < local_today)
                        || (catalog == "premieres"
                            && category != Category::Movie
                            && event["episode"]["episode"] != 1)
                    {
                        return None;
                    }
                    let parent = viptv_simkl::normalize(&source, category.clone())?;
                    if catalog == "my-calendar"
                        && !profile
                            .is_some_and(|p| self.saved(p, parent["id"].as_str().unwrap_or("")))
                    {
                        return None;
                    }
                    if category == Category::Movie {
                        return Some(parent);
                    }
                    let mut ep = event["episode"].clone();
                    ep["date"] = event["date"].clone();
                    ep["tvdb"] = event["tvdb"].clone();
                    let mut item = viptv_simkl::episode(&parent, &ep)?;
                    item["last_aired"] = event["date"].clone();
                    Some(item)
                })
                .collect()
        } else {
            let time = if ["today", "week", "month"].contains(&catalog) {
                catalog
            } else {
                "today"
            };
            let raw = self
                .public(
                    &format!(
                        "/discover/trending/{}/{}_500.json",
                        category.endpoint(),
                        time
                    ),
                    3600,
                )
                .await?;
            raw.as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| viptv_simkl::normalize(v, category.clone()))
                .collect()
        };
        self.remember(&items)?;
        if !term.is_empty() && !linked {
            let cached = self
                .db
                .lock()
                .unwrap()
                .prepare("SELECT value FROM simkl_items")
                .map_err(|_| "SIMKL cache unavailable")?
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(|_| "SIMKL cache unavailable")?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| "SIMKL cache unavailable")?;
            items.extend(
                cached
                    .iter()
                    .filter_map(|s| serde_json::from_str::<Value>(s).ok())
                    .filter(|v| v["simkl_category"] == serde_json::to_value(&category).unwrap()),
            );
            let mut seen = HashSet::new();
            items.retain(|v| {
                v["name"]
                    .as_str()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&term.to_lowercase())
                    && seen.insert(v["id"].to_string())
            });
        }
        for item in &mut items {
            if let Some(known) = item["id"].as_str().and_then(|id| self.cached_item(id)) {
                for key in [
                    "genres",
                    "year",
                    "ratings",
                    "rank",
                    "last_aired",
                    "released",
                ] {
                    if item[key].is_null() && !known[key].is_null() {
                        item[key] = known[key].clone();
                    }
                }
            }
            if let Some(object) = item.as_object_mut() {
                object.remove("videos");
            }
        }
        filter(&mut items, &r)?;
        let start = if upstream_page { 0 } else { r.skip };
        if !upstream_page {
            more = items.len() > start + 50;
        }
        Ok(
            json!({"metas":items.into_iter().skip(start).take(50).collect::<Vec<_>>(),"has_more":more,"next_skip":if more{Some(r.skip+50)}else{None},"coverage":if linked&&!term.is_empty(){"simkl_search"}else{"public_feeds_and_known_titles"},"full_search":linked}),
        )
    }
    fn profile_timezone(&self, profile: Option<i64>) -> Option<chrono_tz::Tz> {
        let p = profile?;
        let s = self
            .db
            .lock()
            .ok()?
            .query_row(
                "SELECT value FROM simkl_user_cache WHERE profile_id=?1 AND key='settings'",
                [p],
                |r| r.get::<_, String>(0),
            )
            .ok()?;
        let v: Value = serde_json::from_str(&s).ok()?;
        v["account"]["timezone"].as_str()?.parse().ok()
    }
    fn saved(&self, p: i64, id: &str) -> bool {
        self.db.lock().unwrap().query_row("SELECT EXISTS(SELECT 1 FROM favorites WHERE profile_id=?1 AND id=?2 UNION SELECT 1 FROM simkl_library WHERE profile_id=?1 AND id=?2)",params![p,id],|r|r.get(0)).unwrap_or(false)
    }
}
pub(crate) fn catalogs() -> Value {
    let genres = vec![
        "action",
        "adventure",
        "animation",
        "comedy",
        "crime",
        "documentary",
        "drama",
        "family",
        "fantasy",
        "horror",
        "mystery",
        "romance",
        "science-fiction",
        "thriller",
    ];
    let mut out = vec![];
    for category in [Category::Movie, Category::Tv, Category::Anime] {
        for (id, name) in [
            ("today", "SIMKL Most Watched Today"),
            ("week", "SIMKL Most Watched This Week"),
            ("month", "SIMKL Most Watched This Month"),
            ("new-episodes", "New episodes · Today"),
            ("premieres", "Premieres"),
            ("upcoming", "Upcoming"),
            ("calendar", "Calendar"),
            ("my-calendar", "My calendar"),
        ] {
            if category == Category::Movie && id == "new-episodes" {
                continue;
            }
            let id = if category == Category::Anime {
                format!("anime-{id}")
            } else {
                id.into()
            };
            out.push(json!({"addon_id":0,"addon_name":category.endpoint(),"id":id,"type":category.kind(),"name":name,"supports_search":true,"supports_skip":true,"genres":genres,
                "extra":[{"name":"search"},{"name":"genre","options":genres},{"name":"year_min"},{"name":"year_max"},{"name":"rating_min"},{"name":"rank_max"},{"name":"sort","options":["relevance","title","rank","release-date","last-air-date"]},{"name":"date"},{"name":"month"},{"name":"timezone","default":"America/Detroit"}]}));
        }
    }
    json!(out)
}
fn filter(items: &mut Vec<Value>, r: &addon::DiscoverOptions) -> Result<(), String> {
    let number = |key: &str| -> Result<Option<f64>, String> {
        r.extras
            .get(key)
            .filter(|s| !s.is_empty())
            .map(|s| {
                s.parse::<f64>()
                    .ok()
                    .filter(|n| n.is_finite())
                    .ok_or_else(|| format!("Invalid {key}"))
            })
            .transpose()
    };
    let lo = number("year_min")?;
    let hi = number("year_max")?;
    let rating = number("rating_min")?;
    let rank = number("rank_max")?;
    items.retain(|v| {
        (!lo.is_some_and(|n| v["year"].as_f64().is_none_or(|x| x < n)))
            && (!hi.is_some_and(|n| v["year"].as_f64().is_none_or(|x| x > n)))
            && (!rating.is_some_and(|n| {
                v["ratings"]["simkl"]["rating"]
                    .as_f64()
                    .is_none_or(|x| x < n)
            }))
            && (!rank.is_some_and(|n| v["rank"].as_f64().is_none_or(|x| x <= 0.0 || x > n)))
            && r.genre.as_ref().is_none_or(|g| {
                g.is_empty()
                    || g == "all"
                    || v["genres"].as_array().is_some_and(|a| {
                        a.iter()
                            .any(|v| v.as_str().is_some_and(|s| genre_key(s) == genre_key(g)))
                    })
            })
    });
    match r
        .extras
        .get("sort")
        .map(String::as_str)
        .unwrap_or("relevance")
    {
        "rank" => items.sort_by(|a, b| {
            a["rank"]
                .as_f64()
                .unwrap_or(f64::MAX)
                .total_cmp(&b["rank"].as_f64().unwrap_or(f64::MAX))
        }),
        "title" => items.sort_by_key(|v| v["name"].as_str().unwrap_or("").to_lowercase()),
        "release-date" => items.sort_by(|a, b| {
            b["released"]
                .as_str()
                .unwrap_or("")
                .cmp(a["released"].as_str().unwrap_or(""))
        }),
        "last-air-date" => items.sort_by(|a, b| {
            b["last_aired"]
                .as_str()
                .unwrap_or("")
                .cmp(a["last_aired"].as_str().unwrap_or(""))
        }),
        "relevance" => (),
        _ => return Err("Unsupported SIMKL sort".into()),
    }
    Ok(())
}
fn genre_key(s: &str) -> String {
    let key = s.trim().to_lowercase().replace([' ', '_'], "-");
    if key == "sci-fi" {
        "science-fiction".into()
    } else {
        key
    }
}
