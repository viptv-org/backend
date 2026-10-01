use super::*;

impl ProviderService {
    pub(super) fn channel(&self, id: &str) -> Result<(Provider, String), String> {
        if let Some(account) = self.account {
            let allowed:bool = self.lock()?.query_row("SELECT EXISTS(SELECT 1 FROM provider_live l JOIN providers p ON p.id=l.provider_id JOIN provider_ownership o ON o.provider_id=p.id WHERE l.id=?1 AND o.account_id=?2 AND p.enabled=1 AND p.enable_live=1)",params![id,account],|r|r.get(0)).map_err(db_error)?;
            if !allowed {
                return Err("Live channel not found".into());
            }
            return self.raw_channel(id);
        }
        self.raw_channel(id)
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
            s.require_owner(&db,provider_id)?;
                // Each authorized connection has its own stable admission gate.
                let limit:usize=db.query_row("SELECT max_connections FROM providers WHERE id=?1",[provider_id],|r|r.get(0)).map_err(db_error)?;
                if limit>1_000_000 {return Err("Invalid provider connection limit".into());}
                let mut gates=s.playback_gates.lock().map_err(|_|"Provider limiter unavailable")?;
                let gate=gates.entry(-provider_id).or_insert_with(||PlaybackGate {semaphore:Arc::new(Semaphore::new(1_000_000))});
                if limit>0 && 1_000_000-gate.semaphore.available_permits()>=limit {return Err("Provider connection limit reached".into());}
                gate.semaphore.clone().try_acquire_owned().map_err(|_|"Provider connection limit reached".into())

        })
        .await
    }

    pub fn channel_source(&self, id: &str) -> Result<(String, i64), String> {
        let (provider, stream) = self.channel(id)?;
        Ok((
            media_url(
                &provider.url,
                &provider.username,
                &provider.password,
                "live",
                &stream,
                "ts",
            )?,
            provider.id,
        ))
    }

    pub async fn guide(&self, channel_id: String) -> Result<Value, String> {
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
