use super::*;

fn summary(db: &Connection, source_id: Option<i64>, search: &str) -> Result<Value, ApiError> {
    let mut q=db.prepare("SELECT id,name,provider_id,enabled,updated_at,attempt_at,reason,next_refresh,channel_epg FROM guide_sources ORDER BY id").map_err(db_error)?;
    let sources=q.query_map([],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"name":r.get::<_,String>(1)?,"provider_id":r.get::<_,Option<i64>>(2)?,"enabled":r.get::<_,bool>(3)?,"updated_at":r.get::<_,Option<i64>>(4)?,"attempt_at":r.get::<_,Option<i64>>(5)?,"reason":r.get::<_,Option<String>>(6)?,"next_refresh":r.get::<_,i64>(7)?,"mode":if r.get::<_,bool>(8)? {"selected_channels"} else {"xmltv"}}))).map_err(db_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
    let mut q = db
        .prepare(
            "SELECT id,data FROM family_channels ORDER BY json_extract(data,'$.number') LIMIT 1000",
        )
        .map_err(db_error)?;
    let raw = q
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(db_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error)?;
    let mut channels = Vec::new();
    for (id, data) in raw {
        let channel: Value = serde_json::from_str(&data).map_err(|_| "Family channel invalid")?;
        let mut q=db.prepare("SELECT m.source_id,m.guide_id,m.observed_name,m.pinned,m.priority,g.name FROM guide_mappings m LEFT JOIN guide_channels g ON g.source_id=m.source_id AND g.guide_id=m.guide_id WHERE m.channel_id=?1 ORDER BY m.pinned DESC,m.priority,m.source_id").map_err(db_error)?;
        let mappings=q.query_map([&id],|r|Ok(json!({"source_id":r.get::<_,i64>(0)?,"guide_id":r.get::<_,String>(1)?,"observed_name":r.get::<_,String>(2)?,"pinned":r.get::<_,bool>(3)?,"priority":r.get::<_,i64>(4)?,"current_name":r.get::<_,Option<String>>(5)?}))).map_err(db_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
        channels.push(json!({"id":id,"name":channel["name"],"feed":channel["feed"],"market":channel["market"],"mappings":mappings,"coverage":coverage(&programmes(db,&id)?,util::now())}));
    }
    let mut q=db.prepare("SELECT source_id,guide_id,name FROM guide_channels WHERE (?1 IS NULL OR source_id=?1) AND instr(lower(name),lower(?2))>0 ORDER BY source_id,name,guide_id LIMIT 100").map_err(db_error)?;
    let guide_channels=q.query_map(params![source_id,search],|r|Ok(json!({"source_id":r.get::<_,i64>(0)?,"guide_id":r.get::<_,String>(1)?,"name":r.get::<_,String>(2)?}))).map_err(db_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
    let run:Value=db.query_row("SELECT state,last_start,last_finish,reason FROM guide_runs WHERE id=1",[],|r|Ok(json!({"state":r.get::<_,String>(0)?,"last_start":r.get::<_,Option<i64>>(1)?,"last_finish":r.get::<_,Option<i64>>(2)?,"reason":r.get::<_,Option<String>>(3)?}))).map_err(db_error)?;
    let next_audit: i64 = db
        .query_row(
            "SELECT next_audit FROM guide_settings WHERE id=1",
            [],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    Ok(
        json!({"settings":policy(db)?,"sources":sources,"channels":channels,"guide_channels":guide_channels,"last_run":run,"next_audit":next_audit}),
    )
}
#[derive(Deserialize)]
pub(crate) struct GuideQuery {
    source_id: Option<i64>,
    search: Option<String>,
}
pub(crate) async fn list(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Query(query): Query<GuideQuery>,
) -> ApiResult {
    let search = query.search.unwrap_or_default();
    if search.len() > 128 {
        return Err("Guide search too long".into());
    }
    blocking(move || {
        let db = a.db.lock().unwrap();
        provider::accounts::owner(&lease, &db)?;
        summary(&db, query.source_id, &search).map(axum::Json)
    })
    .await
}
pub(crate) async fn configure(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    axum::Json(v): axum::Json<Value>,
) -> ApiResult {
    let rules: Policy = serde_json::from_value(v).map_err(|_| "Invalid guide settings")?;
    if rules.timezone.parse::<chrono_tz::Tz>().is_err()
        || !(15..=10080).contains(&rules.refresh_minutes)
        || !(15..=1440).contains(&rules.audit_minutes)
    {
        return Err(
            "Use a valid IANA timezone, refresh 15–10080 minutes and audit 15–1440 minutes".into(),
        );
    }
    blocking(move || {
        let db = a.db.lock().unwrap();
        provider::accounts::owner(&lease, &db)?;
        let before = crate::activity::snapshot(&db, "guide_settings", "")?;
        let auth::Principal::Account { account_id, .. } = lease.principal;
        db.execute(
            "UPDATE guide_settings SET data=?1,owner_id=?2,next_audit=0 WHERE id=1",
            params![serde_json::to_string(&rules).unwrap(), account_id],
        )
        .map_err(db_error)?;
        if !rules.enabled {
            pause(&a, &db)?;
        }

        crate::activity::record(&db, "guide_settings", "", before)?;
        summary(&db, None, "").map(axum::Json)
    })
    .await
}
pub(crate) async fn add_source(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    axum::Json(v): axum::Json<Value>,
) -> ApiResult {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Add {
        name: String,
        url: Option<String>,
        provider_id: Option<i64>,
    }
    let input: Add = serde_json::from_value(v).map_err(|_| "Invalid guide source")?;
    if input.name.trim().is_empty()
        || input.name.len() > 128
        || input.url.is_some() == input.provider_id.is_some()
    {
        return Err("Name the source and supply either an XMLTV URL or provider account".into());
    }
    if let Some(url) = &input.url {
        if url.len() > 4096 || util::validate_url(url).is_err() {
            return Err("Invalid XMLTV URL".into());
        }
    }
    blocking(move || {
        let db = a.db.lock().unwrap();
        provider::accounts::owner(&lease, &db)?;
        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM guide_sources", [], |r| r.get(0))
            .map_err(db_error)?;
        if count >= 20 {
            return Err("At most 20 guide sources are supported".into());
        }
        if let Some(id) = input.provider_id {
            let exists: bool = db
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM providers WHERE id=?1)",
                    [id],
                    |r| r.get(0),
                )
                .map_err(db_error)?;
            if !exists {
                return Err("Provider account not found".into());
            }
        }
        db.execute(
            "INSERT INTO guide_sources(name,url,provider_id) VALUES(?1,?2,?3)",
            params![input.name.trim(), input.url, input.provider_id],
        )
        .map_err(db_error)?;
        summary(&db, None, "").map(axum::Json)
    })
    .await
}
pub(crate) async fn enable_source(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<i64>,
    axum::Json(v): axum::Json<Value>,
) -> ApiResult {
    let enabled = v["enabled"]
        .as_bool()
        .ok_or("Set enabled to true or false")?;
    blocking(move || {
        let db = a.db.lock().unwrap();
        provider::accounts::owner(&lease, &db)?;
        let before = crate::activity::snapshot(&db, "guide_source", &id.to_string())?;
        if db
            .execute(
                "UPDATE guide_sources SET enabled=?2,next_refresh=0 WHERE id=?1",
                params![id, enabled],
            )
            .map_err(db_error)?
            == 0
        {
            return Err("Guide source not found".into());
        }
        event(
            &db,
            Some(id),
            None,
            if enabled {
                "source_enabled"
            } else {
                "source_disabled"
            },
        )?;
        crate::activity::record(&db, "guide_source", &id.to_string(), before)?;
        summary(&db, None, "").map(axum::Json)
    })
    .await
}
pub(crate) async fn map_channel(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
    axum::Json(v): axum::Json<Value>,
) -> ApiResult {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Mapping {
        source_id: i64,
        guide_id: String,
        observed_name: String,
        #[serde(default)]
        verified: bool,
        #[serde(default)]
        remove: bool,
        #[serde(default)]
        priority: u32,
    }
    let mapping: Mapping = serde_json::from_value(v).map_err(|_| "Invalid guide mapping")?;
    if mapping.guide_id.len() > 512 || mapping.observed_name.len() > 512 || mapping.priority > 1000
    {
        return Err("Guide mapping too large".into());
    }
    blocking(move||{let mut db=a.db.lock().unwrap();provider::accounts::owner(&lease,&db)?;let family=channel(&db,&id)?;let tx=db.transaction().map_err(db_error)?;
        if mapping.remove{tx.execute("DELETE FROM guide_mappings WHERE channel_id=?1 AND source_id=?2 AND guide_id=?3",params![id,mapping.source_id,mapping.guide_id]).map_err(db_error)?;tx.execute("INSERT OR IGNORE INTO guide_rejections(channel_id,source_id,guide_id) VALUES(?1,?2,?3)",params![id,mapping.source_id,mapping.guide_id]).map_err(db_error)?;}
        else{let name:String=tx.query_row("SELECT name FROM guide_channels WHERE source_id=?1 AND guide_id=?2",params![mapping.source_id,mapping.guide_id],|r|r.get(0)).map_err(db_error)?;if !mapping.verified||name!=mapping.observed_name||!lineup::matching::compatible_guide(&family,&name,true){return Err("Verify the current guide name matches this exact US English station and feed".into());}tx.execute("INSERT INTO guide_mappings(channel_id,source_id,guide_id,observed_name,pinned,priority) VALUES(?1,?2,?3,?4,1,?5) ON CONFLICT(channel_id,source_id,guide_id) DO UPDATE SET observed_name=excluded.observed_name,pinned=1,priority=excluded.priority",params![id,mapping.source_id,mapping.guide_id,name,mapping.priority]).map_err(db_error)?;tx.execute("DELETE FROM guide_rejections WHERE channel_id=?1 AND source_id=?2 AND guide_id=?3",params![id,mapping.source_id,mapping.guide_id]).map_err(db_error)?;}
        // An explicit mapping change permits rebuilding from the new precedence.
        tx.execute("DELETE FROM family_programmes WHERE channel_id=?1",[&id]).map_err(db_error)?;event(&tx,Some(mapping.source_id),Some(&id),if mapping.remove{"mapping_rejected"}else{"mapping_pinned"})?;tx.commit().map_err(db_error)?;repair(&mut db,&id,true)?;summary(&db,None,"").map(axum::Json)
    }).await
}
pub(crate) async fn repair_channel(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
    axum::Json(v): axum::Json<Value>,
) -> ApiResult {
    let apply = v["apply"].as_bool().unwrap_or(false);
    let revision = v["revision"].as_str().map(str::to_owned);
    blocking(move || {
        let mut db = a.db.lock().unwrap();
        provider::accounts::owner(&lease, &db)?;
        let preview = repair(&mut db, &id, false)?;
        if !apply {
            return Ok(axum::Json(preview));
        }
        if revision.as_deref() != preview["revision"].as_str() {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "Guide changed; preview the repair again".into(),
            ));
        }
        repair(&mut db, &id, true).map(axum::Json)
    })
    .await
}
pub(crate) async fn run_now(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
) -> Result<(StatusCode, axum::Json<Value>), ApiError> {
    blocking(move||{let db=a.db.lock().unwrap();provider::accounts::owner(&lease,&db)?;let active:bool=db.query_row("SELECT state IN ('queued','running','cancel_requested') FROM guide_runs WHERE id=1",[],|r|r.get(0)).map_err(db_error)?;if active{return Err(ApiError(StatusCode::CONFLICT,"Guide refresh already active".into()));}let auth::Principal::Account{account_id,..}=lease.principal;db.execute("UPDATE guide_runs SET state='queued',owner_id=?1,last_start=?2,last_finish=NULL,reason=NULL WHERE id=1",params![account_id,util::now()]).map_err(db_error)?;Ok((StatusCode::ACCEPTED,axum::Json(json!({"accepted":true}))))}).await
}
pub(crate) async fn cancel(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
) -> ApiResult {
    blocking(move || {
        let db = a.db.lock().unwrap();
        provider::accounts::owner(&lease, &db)?;
        pause(&a, &db)?;
        Ok(axum::Json(json!({"ok":true})))
    })
    .await
}
