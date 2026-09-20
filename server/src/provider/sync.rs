use super::*;

impl ProviderService {
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
    pub(super) fn store_index(
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
}
