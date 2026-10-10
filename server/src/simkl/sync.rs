use super::*;

impl Service {
    pub(crate) async fn sync(&self, profile: i64, manual: bool) -> Result<Value, String> {
        let _guard = self.gate.lock().await;
        let result = self.sync_inner(profile, manual).await;
        if let Err(error) = &result {
            let _ = self.db.lock().unwrap().execute(
                "UPDATE simkl_connections SET error=?1 WHERE profile_id=?2",
                params![error, profile],
            );
        }
        result
    }
    async fn sync_inner(&self, profile: i64, manual: bool) -> Result<Value, String> {
        let generation = self.generation(profile)?;
        let (saved, last): (Option<String>, i64) = self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT snapshot,last_sync FROM simkl_connections WHERE profile_id=?1",
                [profile],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| "SIMKL connection unavailable")?;
        if !manual && util::now() - last < 900 {
            return Ok(json!({"throttled":true}));
        }
        let snapshot = self.user_get(profile, "/sync/activities").await?;
        let old: Value = saved
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(Value::Null);
        let initial = old.is_null();
        if initial || snapshot["settings"]["all"] != old["settings"]["all"] {
            let settings = self.user_get(profile, "/users/settings").await?;
            self.db.lock().unwrap().execute("INSERT INTO simkl_user_cache VALUES(?1,'settings',?2,?3) ON CONFLICT(profile_id,key) DO UPDATE SET marker=excluded.marker,value=excluded.value",params![profile,snapshot["settings"]["all"].to_string(),settings.to_string()]).map_err(|_|"SIMKL settings save failed")?;
        }
        let mut pulled = vec![];
        let mut playback = None;
        if initial {
            for t in ["shows", "movies", "anime"] {
                let flags = if t == "movies" {
                    ""
                } else {
                    if t == "anime" {
                        "?extended=full_anime_seasons&episode_watched_at=yes&include_all_episodes=original&next_watch_info=yes"
                    } else {
                        "?extended=full&episode_watched_at=yes&include_all_episodes=original&next_watch_info=yes"
                    }
                };
                let path = format!("/sync/all-items/{t}{flags}");
                match self.user_get(profile, &path).await {
                    Ok(v) => pulled.push(v),
                    Err(e) if e.contains("max_items") => {
                        for status in viptv_simkl::STATUSES {
                            if t == "movies" && ["watching", "hold"].contains(status) {
                                continue;
                            }
                            pulled.push(
                                self.user_get(
                                    profile,
                                    &format!("/sync/all-items/{t}/{status}{flags}"),
                                )
                                .await?,
                            );
                        }
                    }
                    Err(e) => return Err(e),
                }
            }
            playback = Some(self.user_get(profile, "/sync/playback").await?);
        } else if snapshot["all"] != old["all"] {
            let since = old["all"].as_str().ok_or("SIMKL snapshot invalid")?;
            let mut u = url::Url::parse("https://unused").unwrap();
            u.query_pairs_mut()
                .append_pair("date_from", since)
                .append_pair("extended", "full_anime_seasons")
                .append_pair("episode_watched_at", "yes")
                .append_pair("include_all_episodes", "original")
                .append_pair("next_watch_info", "yes");
            let mut item_change = false;
            let mut ratings = false;
            let mut removed = false;
            let mut paused = false;
            for t in ["shows", "movies", "anime"] {
                for status in viptv_simkl::STATUSES {
                    if snapshot[t][status] != old[t][status] {
                        item_change = true;
                    }
                }
                ratings |= snapshot[t]["rated_at"] != old[t]["rated_at"];
                removed |= snapshot[t]["removed_from_list"] != old[t]["removed_from_list"];
                paused |= snapshot[t]["playback"] != old[t]["playback"];
            }
            if item_change {
                pulled.push(
                    self.user_get(profile, &format!("/sync/all-items?{}", u.query().unwrap()))
                        .await?,
                );
            }
            if ratings {
                let mut q = url::Url::parse("https://unused").unwrap();
                q.query_pairs_mut().append_pair("date_from", since);
                pulled.push(
                    self.user_get(profile, &format!("/sync/ratings?{}", q.query().unwrap()))
                        .await?,
                );
            }
            if paused {
                playback = Some(self.user_get(profile, "/sync/playback").await?);
            }
            if removed {
                let ids = self
                    .user_get(profile, "/sync/all-items?extended=simkl_ids_only")
                    .await?;
                self.current(profile, &generation)?;
                let current: HashSet<String> = entries(&ids)
                    .into_iter()
                    .filter_map(|(c, v)| {
                        viptv_simkl::normalize(
                            v.get("movie")
                                .or_else(|| v.get("show"))
                                .or_else(|| v.get("anime"))
                                .unwrap_or(&v),
                            c,
                        )
                        .and_then(|v| v["id"].as_str().map(str::to_owned))
                    })
                    .collect();
                let db = self.db.lock().unwrap();
                let tx = db
                    .unchecked_transaction()
                    .map_err(|_| "SIMKL storage unavailable")?;
                let local = tx
                    .prepare("SELECT id FROM simkl_library WHERE profile_id=?1")
                    .map_err(|_| "SIMKL storage unavailable")?
                    .query_map([profile], |r| r.get::<_, String>(0))
                    .map_err(|_| "SIMKL storage unavailable")?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| "SIMKL storage unavailable")?;
                for id in local.into_iter().filter(|id| !current.contains(id)) {
                    let pending:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM simkl_outbox WHERE profile_id=?1 AND id=?2)",params![profile,id],|r|r.get(0)).map_err(|_|"SIMKL pending change lookup failed")?;
                    if pending {
                        continue;
                    }
                    tx.execute(
                        "DELETE FROM simkl_library WHERE profile_id=?1 AND id=?2",
                        params![profile, id],
                    )
                    .map_err(|_| "SIMKL storage unavailable")?;
                    tx.execute("DELETE FROM progress WHERE profile_id=?1 AND (id=?2 OR title_id=?2) AND json_extract(context,'$.simkl_imported')=1",params![profile,id]).map_err(|_|"SIMKL storage unavailable")?;
                    tx.execute(
                        "DELETE FROM favorites WHERE profile_id=?1 AND id=?2",
                        params![profile, id],
                    )
                    .map_err(|_| "SIMKL storage unavailable")?;
                    // Tombstones prevent later backfill from re-adding a remote removal.
                    tx.execute(
                        "INSERT OR IGNORE INTO simkl_exports VALUES(?1,?2)",
                        params![profile, format!("removed:{id}")],
                    )
                    .map_err(|_| "SIMKL storage unavailable")?;
                }
                tx.commit().map_err(|_| "SIMKL storage unavailable")?;
            }
        }
        self.current(profile, &generation)?;
        let mut imported = 0;
        for batch in &pulled {
            for (category, row) in entries(batch) {
                self.import(profile, category, &row)?;
                imported += 1;
            }
        }
        if let Some(playback) = playback {
            for row in playback.as_array().into_iter().flatten() {
                let (c, title) = if row["movie"].is_object() {
                    (Category::Movie, &row["movie"])
                } else if row["anime"].is_object() {
                    (Category::Anime, &row["anime"])
                } else {
                    (Category::Tv, &row["show"])
                };
                if let Some(mut item) = viptv_simkl::normalize(title, c.clone()) {
                    if row["episode"].is_object() {
                        let mut ep = row["episode"].clone();
                        ep["episode"] = ep["number"].clone();
                        if let Some(e) = viptv_simkl::episode(&item, &ep) {
                            item = e;
                        }
                    }
                    let duration = title["runtime"].as_f64().unwrap_or(0.0) * 60.0;
                    let percentage = row["progress"].as_f64().unwrap_or(0.0);
                    if duration > 0.0
                        && percentage.is_finite()
                        && (0.0..=100.0).contains(&percentage)
                    {
                        self.import_progress(
                            profile,
                            &item,
                            duration * percentage / 100.0,
                            duration,
                            date(&row["paused_at"]),
                            false,
                        )?;
                    }
                }
            }
        }
        let exported = self.backfill(profile).await?;
        self.flush(profile).await?;
        self.current(profile, &generation)?;
        let counts = json!({"imported":imported,"exported":exported});
        self.db.lock().unwrap().execute("UPDATE simkl_connections SET snapshot=?1,last_sync=?2,error=NULL,counts=?3 WHERE profile_id=?4 AND generation=?5",params![snapshot.to_string(),util::now(),counts.to_string(),profile,generation]).map_err(|_|"SIMKL snapshot save failed")?;
        Ok(counts)
    }
    fn import(&self, profile: i64, c: Category, row: &Value) -> Result<(), String> {
        let title = row
            .get("movie")
            .or_else(|| row.get("show"))
            .or_else(|| row.get("anime"))
            .unwrap_or(row);
        let Some(item) = viptv_simkl::normalize(title, c.clone()) else {
            return Ok(());
        };
        self.remember(&[item.clone()])?;
        self.adopt_alias(profile, &item)?;
        let id = item["id"].as_str().unwrap();
        let kind = c.kind();
        // Preserve recorded state in a mirror; API imports never enqueue writes.
        {
            let db = self.db.lock().unwrap();
            db.execute("INSERT INTO simkl_library VALUES(?1,?2,?3) ON CONFLICT(profile_id,id) DO UPDATE SET value=excluded.value",params![profile,id,row.to_string()]).map_err(|_|"SIMKL library save failed")?;
            if ["plantowatch", "watching", "hold"].contains(&row["status"].as_str().unwrap_or("")) {
                db.execute("INSERT INTO favorites(profile_id,id,type,name,poster) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(profile_id,type,id) DO UPDATE SET name=excluded.name,poster=excluded.poster",params![profile,id,kind,item["name"].as_str().unwrap_or(id),item["poster"].as_str()]).map_err(|_|"SIMKL list save failed")?;
            }
        }
        let duration = item["duration"].as_f64().unwrap_or(0.0);
        if c == Category::Movie
            && (row["status"] == "completed" || !row["last_watched_at"].is_null())
        {
            self.import_progress(
                profile,
                &item,
                duration,
                duration,
                date(&row["last_watched_at"]),
                true,
            )?;
        }
        for season in row["seasons"].as_array().into_iter().flatten() {
            for episode in season["episodes"].as_array().into_iter().flatten() {
                let mut e = episode.clone();
                e["season"] = season["number"].clone();
                e["episode"] = episode["number"].clone();
                if let Some(ep) = viptv_simkl::episode(&item, &e) {
                    self.remember(&[ep.clone()])?;
                    self.import_progress(
                        profile,
                        &ep,
                        duration,
                        duration,
                        date(&episode["watched_at"]),
                        true,
                    )?;
                }
            }
        }
        // Rehydrate existing IMDb identities without replacing their user facts.
        if let Some(imdb) = item["simkl_ids"]["imdb"].as_str() {
            let db = self.db.lock().unwrap();
            db.execute(
                "UPDATE favorites SET name=?1,poster=?2 WHERE profile_id=?3 AND id=?4",
                params![
                    item["name"].as_str(),
                    item["poster"].as_str(),
                    profile,
                    imdb
                ],
            )
            .map_err(|_| "SIMKL rehydration failed")?;
            db.execute("UPDATE progress SET name=?1,poster=?2 WHERE profile_id=?3 AND (id=?4 OR title_id=?4)",params![item["name"].as_str(),item["poster"].as_str(),profile,imdb]).map_err(|_|"SIMKL rehydration failed")?;
        }
        Ok(())
    }
    fn import_progress(
        &self,
        p: i64,
        item: &Value,
        position: f64,
        duration: f64,
        at: i64,
        watched: bool,
    ) -> Result<(), String> {
        self.adopt_alias(p, item)?;
        let mut context = matching_context(item).map_err(|_| "SIMKL episode context invalid")?;
        if watched {
            context["watched_override"] = json!(true);
            context["simkl_completion_only"] = json!(duration <= 0.0);
        }
        context["simkl_watch_date_unknown"] = json!(at == 0);
        let id = item["id"].as_str().ok_or("SIMKL identity invalid")?;
        let db = self.db.lock().unwrap();
        let pending: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM simkl_outbox WHERE profile_id=?1 AND id=?2)",
                params![p, id],
                |r| r.get(0),
            )
            .map_err(|_| "SIMKL pending change lookup failed")?;
        if pending {
            return Ok(());
        }
        let owned:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM progress WHERE profile_id=?1 AND id=?2 AND json_extract(context,'$.simkl_imported') IS NOT 1)",params![p,id],|r|r.get(0)).map_err(|_|"SIMKL provenance lookup failed")?;
        context["simkl_imported"] = json!(!owned);
        db.execute("INSERT INTO progress(profile_id,id,type,name,poster,position,duration,updated_at,context,title_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT(profile_id,type,id) DO UPDATE SET name=excluded.name,poster=excluded.poster,position=CASE WHEN excluded.updated_at>progress.updated_at THEN excluded.position ELSE progress.position END,duration=CASE WHEN excluded.duration>0 THEN excluded.duration ELSE progress.duration END,updated_at=MAX(excluded.updated_at,progress.updated_at),context=json_patch(progress.context,excluded.context)",params![p,id,item["type"].as_str(),item["name"].as_str().unwrap_or(id),item["poster"].as_str(),position,duration,at,context.to_string(),continuation::title_id(item)]).map_err(|_|"SIMKL progress save failed")?;
        Ok(())
    }
    fn adopt_alias(&self, p: i64, item: &Value) -> Result<(), String> {
        let alias = if item["episode"].is_null() {
            item["simkl_ids"]["imdb"].as_str().map(str::to_owned)
        } else {
            viptv_simkl::stream_id(item).ok()
        };
        let Some(alias) = alias else { return Ok(()) };
        let id = item["id"].as_str().ok_or("SIMKL identity missing")?;
        let kind = item["type"].as_str().ok_or("SIMKL kind missing")?;
        let db = self.db.lock().unwrap();
        let matches: i64 = db
            .query_row(
                "SELECT count(*) FROM simkl_aliases WHERE alias=?1 AND kind=?2",
                params![alias, kind],
                |r| r.get(0),
            )
            .map_err(|_| "SIMKL alias lookup failed")?;
        if matches != 1 {
            return Ok(());
        }
        let tx = db
            .unchecked_transaction()
            .map_err(|_| "SIMKL alias migration failed")?;
        tx.execute("INSERT OR IGNORE INTO favorites(profile_id,id,type,name,poster) SELECT profile_id,?1,type,?2,?3 FROM favorites WHERE profile_id=?4 AND id=?5 AND type=?6",params![id,item["name"].as_str().unwrap_or(id),item["poster"].as_str(),p,alias,kind]).map_err(|_|"SIMKL alias migration failed")?;
        tx.execute(
            "DELETE FROM favorites WHERE profile_id=?1 AND id=?2 AND type=?3",
            params![p, alias, kind],
        )
        .map_err(|_| "SIMKL alias migration failed")?;
        let previous:Option<(f64,f64,i64,String)>=tx.query_row("SELECT position,duration,updated_at,context FROM progress WHERE profile_id=?1 AND id=?2 AND type=?3",params![p,alias,kind],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(|_|"SIMKL alias migration failed")?;
        if let Some((position, duration, updated, context)) = previous {
            let mut context: Value = serde_json::from_str(&context).unwrap_or(json!({}));
            for key in [
                "simkl_category",
                "simkl_ids",
                "simkl_episode_ids",
                "tvdb",
                "series_id",
                "season",
                "episode",
            ] {
                if !item[key].is_null() {
                    context[key] = item[key].clone();
                }
            }
            tx.execute("INSERT INTO progress(profile_id,id,type,name,poster,position,duration,updated_at,context,title_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT(profile_id,type,id) DO UPDATE SET position=CASE WHEN excluded.updated_at>progress.updated_at THEN excluded.position ELSE progress.position END,updated_at=MAX(excluded.updated_at,progress.updated_at),context=json_patch(excluded.context,progress.context)",params![p,id,kind,item["name"].as_str().unwrap_or(id),item["poster"].as_str(),position,duration,updated,context.to_string(),continuation::title_id(item)]).map_err(|_|"SIMKL alias migration failed")?;
            tx.execute(
                "DELETE FROM progress WHERE profile_id=?1 AND id=?2 AND type=?3",
                params![p, alias, kind],
            )
            .map_err(|_| "SIMKL alias migration failed")?;
        }
        tx.commit()
            .map_err(|_| "SIMKL alias migration failed".into())
    }
    async fn backfill(&self, p: i64) -> Result<usize, String> {
        let rows = {
            let db = self.db.lock().unwrap();
            let rows=db.prepare("SELECT id,type,name,poster,position,duration,updated_at,context FROM progress WHERE profile_id=?1 AND type!='live'").map_err(|_|"SIMKL history read failed")?.query_map([p],|r|Ok(json!({"id":r.get::<_,String>(0)?,"type":r.get::<_,String>(1)?,"name":r.get::<_,String>(2)?,"poster":r.get::<_,Option<String>>(3)?,"position":r.get::<_,f64>(4)?,"duration":r.get::<_,f64>(5)?,"updated_at":r.get::<_,i64>(6)?,"context":r.get::<_,String>(7)?}))).map_err(|_|"SIMKL history read failed")?.collect::<Result<Vec<_>,_>>().map_err(|_|"SIMKL history read failed")?;
            rows
        };
        let mut history = vec![];
        let mut exports = vec![];
        for mut row in rows {
            let context: Value =
                serde_json::from_str(row["context"].as_str().unwrap_or("{}")).unwrap_or(json!({}));
            if !library::watched(&row, &context) {
                continue;
            }
            row.as_object_mut()
                .unwrap()
                .extend(context.as_object().cloned().unwrap_or_default());
            let key = format!("history:{}:{}", row["id"], row["updated_at"]);
            let title = row["series_id"]
                .as_str()
                .unwrap_or(row["id"].as_str().unwrap_or(""));
            if self.exported(p, &format!("removed:{title}")) {
                continue;
            }
            if self.exported(p, &key)
                || self.exported(p, &format!("watched:{}", row["id"].as_str().unwrap_or("")))
                || self.remote_watched(p, &row)
            {
                continue;
            }
            let Ok(payload) = viptv_simkl::write_item(&row) else {
                continue;
            };
            let mut entry = payload_to_history(payload, &row);
            if let Some(t) =
                chrono::DateTime::from_timestamp(row["updated_at"].as_i64().unwrap_or(0).max(0), 0)
            {
                if let Some(seasons) = entry["seasons"].as_array_mut() {
                    for s in seasons {
                        if let Some(e) = s["episodes"].as_array_mut() {
                            for e in e {
                                e["watched_at"] = json!(t.to_rfc3339());
                            }
                        }
                    }
                } else {
                    entry["watched_at"] = json!(t.to_rfc3339());
                }
            }
            history.push((row["type"] == "movie", entry));
            exports.push(key);
        }
        let token = self.token(p).await?;
        let mut count = 0;
        for (batch, keys) in history.chunks(50).zip(exports.chunks(50)) {
            let body = json!({"movies":batch.iter().filter(|v|v.0).map(|v|v.1.clone()).collect::<Vec<_>>(),"shows":batch.iter().filter(|v|!v.0).map(|v|v.1.clone()).collect::<Vec<_>>()});
            let result = self
                .client()?
                .post("/sync/history?skip_auto_watching=yes", &token, body)
                .await
                .map_err(|e| e.to_string())?;
            if result["not_found"].as_object().is_some_and(|o| {
                o.values()
                    .any(|v| v.as_array().is_some_and(|a| !a.is_empty()))
            }) {
                return Err("Some SIMKL history IDs could not be matched".into());
            }
            for key in keys {
                self.mark_export(p, key)?;
            }
            count += batch.len();
        }
        let favorites = {
            let db = self.db.lock().unwrap();
            let rows=db.prepare("SELECT id,type,name FROM favorites WHERE profile_id=?1 AND type!='live'").map_err(|_|"SIMKL list read failed")?.query_map([p],|r|Ok(json!({"id":r.get::<_,String>(0)?,"type":r.get::<_,String>(1)?,"name":r.get::<_,String>(2)?}))).map_err(|_|"SIMKL list read failed")?.collect::<Result<Vec<_>,_>>().map_err(|_|"SIMKL list read failed")?;
            rows
        };
        let local = {
            let db = self.db.lock().unwrap();
            let rows=db.prepare("SELECT id,value FROM simkl_library WHERE profile_id=?1 AND json_extract(value,'$.local')=1").map_err(|_|"Local watchlist read failed")?.query_map([p],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).map_err(|_|"Local watchlist read failed")?.collect::<Result<Vec<_>,_>>().map_err(|_|"Local watchlist read failed")?;
            rows
        };
        let mut local: HashMap<String, Value> = local
            .into_iter()
            .filter_map(|(id, s)| serde_json::from_str(&s).ok().map(|v| (id, v)))
            .collect();
        let mut pending = vec![];
        let mut keys = vec![];
        for row in favorites {
            let id = row["id"].as_str().unwrap();
            let key = format!("list:{id}");
            if self.exported(p, &key)
                || self.exported(p, &format!("removed:{id}"))
                || self.remote_exists(p, &row)
            {
                continue;
            }
            let watched:bool=self.db.lock().unwrap().query_row("SELECT EXISTS(SELECT 1 FROM progress WHERE profile_id=?1 AND (id=?2 OR title_id=?2) AND ((duration>0 AND position/duration>=0.8) OR json_extract(context,'$.watched_override')=1))",params![p,id],|r|r.get(0)).map_err(|_|"SIMKL history read failed")?;
            if watched {
                continue;
            }
            let Ok(payload) = viptv_simkl::write_item(&row) else {
                continue;
            };
            let mut entry = payload
                .get("movie")
                .or_else(|| payload.get("show"))
                .or_else(|| payload.get("anime"))
                .unwrap()
                .clone();
            entry["status"] = local
                .remove(id)
                .map(|row| row["status"].clone())
                .unwrap_or(json!("plantowatch"));
            pending.push((row["type"] == "movie", entry));
            keys.push(key);
        }
        for (id, row) in local {
            let key = format!("list:{id}");
            if self.exported(p, &key) || self.exported(p, &format!("removed:{id}")) {
                continue;
            }
            let movie = row["movie"].is_object();
            let mut entry = if movie {
                row["movie"].clone()
            } else if row["anime"].is_object() {
                row["anime"].clone()
            } else {
                row["show"].clone()
            };
            entry["status"] = row["status"].clone();
            if row["status"] == "completed" {
                if let Some(time) = chrono::DateTime::<chrono::Utc>::from_timestamp(
                    row["updated_at"].as_i64().unwrap_or(0),
                    0,
                ) {
                    entry["watched_at"] = json!(time.to_rfc3339());
                }
            }
            pending.push((movie, entry));
            keys.push(key);
        }
        for (batch, keys) in pending.chunks(50).zip(keys.chunks(50)) {
            let result=self.client()?.post("/sync/history?skip_auto_watching=yes",&token,json!({"movies":batch.iter().filter(|v|v.0).map(|v|v.1.clone()).collect::<Vec<_>>(),"shows":batch.iter().filter(|v|!v.0).map(|v|v.1.clone()).collect::<Vec<_>>()})).await.map_err(|e|e.to_string())?;
            if not_found(&result) {
                return Err("Some SIMKL list IDs could not be matched".into());
            }
            for key in keys {
                self.mark_export(p, key)?;
                if let Some(id) = key.strip_prefix("list:") {
                    self.db.lock().unwrap().execute("UPDATE simkl_library SET value=json_set(value,'$.local',json('false')) WHERE profile_id=?1 AND id=?2",params![p,id]).map_err(|_|"Watchlist export save failed")?;
                }
            }
            count += batch.len();
        }
        Ok(count)
    }
    fn exported(&self, p: i64, key: &str) -> bool {
        self.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM simkl_exports WHERE profile_id=?1 AND key=?2)",
                params![p, key],
                |r| r.get(0),
            )
            .unwrap_or(false)
    }
    fn mark_export(&self, p: i64, key: &str) -> Result<(), String> {
        self.db
            .lock()
            .unwrap()
            .execute(
                "INSERT OR IGNORE INTO simkl_exports VALUES(?1,?2)",
                params![p, key],
            )
            .map_err(|_| "SIMKL export save failed")?;
        Ok(())
    }
    fn remote_exists(&self, p: i64, item: &Value) -> bool {
        self.remote_row(p, item).is_some()
    }
    fn remote_watched(&self, p: i64, item: &Value) -> bool {
        self.remote_row(p, item).is_some_and(|r| {
            if item["episode"].is_null() {
                r["status"] == "completed"
            } else {
                r["seasons"].as_array().into_iter().flatten().any(|s| {
                    s["number"] == item["season"]
                        && s["episodes"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .any(|e| e["number"] == item["episode"])
                })
            }
        })
    }
    fn remote_row(&self, p: i64, item: &Value) -> Option<Value> {
        let title = item["series_id"]
            .as_str()
            .unwrap_or(item["id"].as_str().unwrap_or(""));
        let db = self.db.lock().ok()?;
        let mut q = db
            .prepare("SELECT value FROM simkl_library WHERE profile_id=?1")
            .ok()?;
        let found = q
            .query_map([p], |r| r.get::<_, String>(0))
            .ok()?
            .flatten()
            .filter_map(|s| serde_json::from_str::<Value>(&s).ok())
            .filter(|v| v["local"] != true)
            .find(|v| {
                let media = v
                    .get("movie")
                    .or_else(|| v.get("show"))
                    .or_else(|| v.get("anime"))
                    .unwrap_or(v);
                viptv_simkl::parse_id(title)
                    .is_some_and(|(_, n)| viptv_simkl::identifier(&media["ids"]) == Some(n))
                    || media["ids"]["imdb"].as_str() == Some(title)
            });
        found
    }
    pub(crate) fn enqueue(
        &self,
        p: i64,
        id: &str,
        action: &str,
        value: &Value,
    ) -> Result<(), String> {
        self.db.lock().unwrap().execute("INSERT INTO simkl_outbox VALUES(?1,?2,?3,?4) ON CONFLICT(profile_id,id,action) DO UPDATE SET value=excluded.value",params![p,id,action,value.to_string()]).map_err(|_|"SIMKL pending change save failed")?;
        Ok(())
    }
    pub(crate) async fn changed(&self, p: i64, item: Value, action: &str) -> Result<(), String> {
        if !self.connected(p) {
            return Ok(());
        }
        let _guard = self.gate.lock().await;
        if (action == "save" && self.remote_exists(p, &item))
            || (action == "remove" && self.remote_watched(p, &item))
        {
            return Ok(());
        }
        let id = item["id"].as_str().ok_or("SIMKL item missing")?;
        let payload = viptv_simkl::write_item(&item).map_err(str::to_owned)?;
        let mut entry = payload_to_history(payload, &item);
        let route = match action {
            "save" => {
                entry["status"] = json!("plantowatch");
                "history?skip_auto_watching=yes"
            }
            "remove" | "unwatched" => "history/remove",
            "watched" => {
                entry["watched_at"] = json!(chrono::DateTime::<chrono::Utc>::from_timestamp(
                    util::now(),
                    0
                )
                .unwrap()
                .to_rfc3339());
                "history?skip_auto_watching=yes"
            }
            _ => return Ok(()),
        };
        let body = if item["type"] == "movie" {
            json!({"movies":[entry]})
        } else {
            json!({"shows":[entry]})
        };
        self.enqueue(p, id, route, &body)?;
        self.flush(p).await
    }
    pub(crate) async fn flush(&self, p: i64) -> Result<(), String> {
        let rows = {
            let db = self.db.lock().unwrap();
            let rows = db
                .prepare("SELECT id,action,value FROM simkl_outbox WHERE profile_id=?1")
                .map_err(|_| "SIMKL pending changes unavailable")?
                .query_map([p], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })
                .map_err(|_| "SIMKL pending changes unavailable")?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| "SIMKL pending changes unavailable")?;
            rows
        };
        if rows.is_empty() {
            return Ok(());
        }
        let generation = self.generation(p)?;
        let token = self.token(p).await?;
        for (id, action, value) in rows {
            let body = serde_json::from_str(&value).map_err(|_| "SIMKL pending change invalid")?;
            let response = self
                .client()?
                .post(&format!("/sync/{action}"), &token, body)
                .await
                .map_err(|e| e.to_string())?;
            if not_found(&response) {
                return Err("SIMKL could not match a pending item".into());
            }
            self.current(p, &generation)?;
            if action == "add-to-list" {
                self.db.lock().unwrap().execute("UPDATE simkl_library SET value=json_set(value,'$.local',json('false')) WHERE profile_id=?1 AND id=?2",params![p,id]).map_err(|_|"Watchlist acknowledgement failed")?;
                let status = response["added"]["movies"]
                    .as_array()
                    .and_then(|a| a.first())
                    .or_else(|| {
                        response["added"]["shows"]
                            .as_array()
                            .and_then(|a| a.first())
                    })
                    .and_then(|v| v["to"].as_str());
                if let Some(status) = status {
                    self.db.lock().unwrap().execute("UPDATE simkl_library SET value=json_set(value,'$.status',?1) WHERE profile_id=?2 AND id=?3",params![status,p,id]).map_err(|_|"SIMKL status save failed")?;
                }
            }
            self.db.lock().unwrap().execute("DELETE FROM simkl_outbox WHERE profile_id=?1 AND id=?2 AND action=?3 AND value=?4",params![p,id,action,value]).map_err(|_|"SIMKL pending change save failed")?;
        }
        Ok(())
    }
}
fn entries(v: &Value) -> Vec<(Category, Value)> {
    let mut out = vec![];
    for (key, c) in [
        ("movies", Category::Movie),
        ("shows", Category::Tv),
        ("anime", Category::Anime),
    ] {
        for item in v[key].as_array().into_iter().flatten() {
            out.push((c.clone(), item.clone()));
        }
    }
    out
}
fn date(v: &Value) -> i64 {
    v.as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.timestamp())
        .filter(|n| *n > 0)
        .unwrap_or(0)
}
fn payload_to_history(payload: Value, row: &Value) -> Value {
    let mut item = payload
        .get("movie")
        .or_else(|| payload.get("show"))
        .or_else(|| payload.get("anime"))
        .unwrap()
        .clone();
    if !row["episode"].is_null() {
        item["seasons"] = json!([{"number":row["season"].as_u64().unwrap_or(1),"episodes":[{"number":row["episode"]}]}]);
    }
    item
}

fn not_found(v: &Value) -> bool {
    v["not_found"].as_object().is_some_and(|o| {
        o.values()
            .any(|v| v.as_array().is_some_and(|a| !a.is_empty()))
    })
}
