use super::*;

impl ProviderService {
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

    pub(super) fn channel(&self, id: &str) -> Result<(Provider, String), String> {
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
}
