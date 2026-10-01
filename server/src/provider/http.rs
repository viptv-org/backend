use super::*;

impl ProviderService {
    async fn api(
        &self,
        provider: &Provider,
        action: &str,
        extra: &[(&str, &str)],
    ) -> Result<Value, String> {
        self.api_bounded(provider, action, extra, MAX_RESPONSE)
            .await
    }
    pub(super) async fn api_bounded(
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
        let snapshot = provider.clone();
        self.blocking(move |s| {
            s.provider_for_kind(id, kind)?;
            let db = s.lock()?;
            s.require_owner(&db, id)?;
            snapshot.ensure_current(&db)
        })
        .await?;
        let protected = provider.sealed.is_some() || self.account.is_some();
        let mut url = if protected {
            transport_v2::base(&provider.url, self.fixture_transport())?
                .join("player_api.php")
                .map_err(|_| "invalid_provider_endpoint")?
        } else {
            endpoint(&provider.url, "player_api.php")?
        };
        url.query_pairs_mut()
            .append_pair("username", &provider.username)
            .append_pair("password", &provider.password)
            .append_pair("action", action)
            .extend_pairs(extra.iter().copied());
        if protected {
            if egress::enabled(&*self.lock()?, provider.id)? {
                return Err("source_route_migration_required".into());
            }
            return self.protected_json(url, response_limit).await;
        }
        // Never format reqwest errors: they can contain the credential-bearing URL.
        let client = self.client.clone();
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

    pub(super) async fn cached_api(
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
            s.require_owner(&db, provider_id)?;
            snapshot.ensure_current(&db)?;
            let cached: Option<String> = db.query_row(
                "SELECT payload FROM provider_cache WHERE provider_id=?1 AND cache_key=?2 AND expires_at>?3",
                params![provider_id,cache_key,now], |r|r.get(0)
            ).optional().map_err(db_error)?;
            Ok(cached.and_then(|body| {
                if let Some((account,_))=&snapshot.sealed {
                    let plain=s.vault.as_ref()?.open(*account,"xtream-cache",&credentials_v2::cache_record(provider_id,&cache_key),&body).ok()?;
                    serde_json::from_slice::<Value>(plain.expose()).ok()
                } else {serde_json::from_str::<Value>(&body).ok()}
            }))
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
            let plain=zeroize::Zeroizing::new(value.to_string());
            let payload=if let Some((account,_))=&snapshot.sealed {
                s.vault.as_ref().and_then(|vault|vault.seal(*account,"xtream-cache",&credentials_v2::cache_record(provider_id,&key),plain.as_bytes()).ok())
            } else {Some(plain.to_string())};
            let db=s.lock()?;
            s.require_owner(&db, provider_id)?;
            snapshot.ensure_current(&db)?;
            if let Some(payload)=payload {
                let _ = db.execute("DELETE FROM provider_cache WHERE expires_at<=?1", [now]);
                let _=db.execute("INSERT INTO provider_cache(provider_id,cache_key,expires_at,payload)
                    SELECT ?1,?2,?3,?4 WHERE EXISTS(SELECT 1 FROM providers WHERE id=?1 AND enabled=1 AND CASE ?5 WHEN 'live' THEN enable_live WHEN 'movie' THEN enable_movies WHEN 'series' THEN enable_series ELSE 0 END=1)
                    ON CONFLICT(provider_id,cache_key) DO UPDATE SET expires_at=excluded.expires_at,payload=excluded.payload",
                    params![provider_id,key,crate::util::now()+ttl,payload,kind]);
            }
            Ok(value)
        }).await
    }
}
