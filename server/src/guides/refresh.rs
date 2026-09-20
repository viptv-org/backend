use super::*;

fn source(db: &Connection, id: i64) -> Result<Source, ApiError> {
    let (url, provider): (Option<String>, Option<i64>) = db
        .query_row(
            "SELECT url,provider_id FROM guide_sources WHERE id=?1 AND enabled=1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(db_error)?;
    let url = if let Some(provider) = provider {
        let (base, user, password): (String, String, String) = db
            .query_row(
                "SELECT url,username,password FROM providers WHERE id=?1 AND enabled=1",
                [provider],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(db_error)?;
        let mut url = url::Url::parse(&format!("{}/xmltv.php", base.trim_end_matches('/')))
            .map_err(|_| "Invalid provider guide URL")?;
        url.query_pairs_mut()
            .append_pair("username", &user)
            .append_pair("password", &password);
        url.to_string()
    } else {
        url.ok_or("Guide URL missing")?
    };
    let proxy = provider
        .map(|id| crate::provider::egress::proxy(db, id))
        .transpose()?
        .flatten();
    let revision = format!("{:x}", Sha256::digest(format!("{url}{proxy:?}").as_bytes()));
    let channel_epg = db
        .query_row(
            "SELECT channel_epg FROM guide_sources WHERE id=?1",
            [id],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    Ok(Source {
        id,
        url,
        revision,
        channel_epg,
        proxy,
    })
}
// Some providers publish hundreds of megabytes of unrelated XMLTV. On that
// bounded-input failure, fetch only verified selected channels and remember the
// strategy. The guide remains source-scoped and independent of the video input.
async fn selected_channel_epg(
    a: &App,
    lease: &RunLease,
    saved: &Source,
) -> Option<Result<Parsed, String>> {
    let rows = {
        let db = a.db.lock().unwrap();
        if lease.validate(&db).is_err() {
            return Some(Err("guide_run_cancelled".into()));
        }
        let provider: Option<i64> = db
            .query_row(
                "SELECT provider_id FROM guide_sources WHERE id=?1",
                [saved.id],
                |r| r.get(0),
            )
            .ok()
            .flatten();
        let provider = provider?;
        let read = || -> rusqlite::Result<Vec<(String, String, String)>> {
            let mut query=db.prepare("SELECT DISTINCT l.stream_id,'xtream-stream:'||l.stream_id,l.name FROM family_candidates c JOIN family_channels f ON f.id=c.channel_id JOIN provider_live l ON l.id=c.live_id WHERE l.provider_id=?1 AND json_extract(f.data,'$.enabled')=1 AND EXISTS(SELECT 1 FROM json_each(f.data,'$.candidates') saved WHERE json_extract(saved.value,'$.id')=l.id AND json_extract(saved.value,'$.name')=l.name) ORDER BY c.rank,l.id LIMIT 1000")?;
            let rows = query
                .query_map([provider], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect();
            rows
        };
        match read() {
            Ok(rows) => rows,
            Err(_) => return Some(Err("guide_channel_lookup_failed".into())),
        }
    };
    let result=async {
        let client=crate::provider::egress::builder(reqwest::Client::builder().connect_timeout(Duration::from_secs(5)).timeout(Duration::from_secs(10)).redirect(reqwest::redirect::Policy::limited(3)),saved.proxy.as_deref())?.build().map_err(|_|"guide_request_failed")?;
        let mut parsed=Parsed{channels:HashMap::new(),programs:Vec::new()};
        let mut attempted=std::collections::HashSet::new();
        for (stream,guide,name) in rows {
            if attempted.contains(&guide) { continue; }
            if attempted.len()>=100 { break; }
            attempted.insert(guide.clone());
            {let db=a.db.lock().unwrap();lease.validate(&db).map_err(|_|"guide_run_cancelled")?;if source(&db,saved.id).map_err(|_|"guide_source_changed")?.revision!=saved.revision{return Err("guide_source_changed".to_owned());}}
            let mut url=url::Url::parse(&saved.url).map_err(|_|"guide_request_failed")?;
            let path=url.path().trim_end_matches("xmltv.php").to_owned()+"player_api.php";url.set_path(&path);url.query_pairs_mut().append_pair("action","get_short_epg").append_pair("stream_id",&stream).append_pair("limit","1000");
            let mut response=client.get(url).send().await.map_err(|_|"guide_request_failed")?;
            if !response.status().is_success(){continue;}
            let mut bytes=Vec::new();while let Some(chunk)=response.chunk().await.map_err(|_|"guide_request_failed")?{if bytes.len()+chunk.len()>2*1024*1024{return Err("guide_channel_size_limit".into());}bytes.extend_from_slice(&chunk);}
            let data:Value=serde_json::from_slice(&bytes).map_err(|_|"guide_channel_invalid")?;
            let Some(rows)=data["epg_listings"].as_array() else {continue;};
            if rows.len()>1000{return Err("guide_channel_programme_limit".into());}
            let mut count=0;
            for row in rows {
                let number=|key:&str|row[key].as_i64().or_else(||row[key].as_str()?.parse::<i64>().ok());
                let (Some(start),Some(end))=(number("start_timestamp"),number("stop_timestamp")) else {continue;};
                if end<=start||end-start>86400||end<=util::now()-86400||start>=util::now()+8*86400 {continue;}
                let decode=|key:&str|{use base64::Engine;let raw=row[key].as_str().unwrap_or("");let text=base64::engine::general_purpose::STANDARD.decode(raw).ok().and_then(|bytes|String::from_utf8(bytes).ok()).unwrap_or_else(||raw.to_owned());text.chars().take(8192).collect::<String>()};
                let title=decode("title");if title.trim().is_empty(){continue;}
                parsed.programs.push((guide.clone(),start,end,json!({"title":title,"description":decode("description"),"language":row["lang"].as_str().unwrap_or("en")})));count+=1;
            }
            if count>0 {parsed.channels.insert(guide,name);}
        }
        if parsed.programs.is_empty(){return Err("guide_empty_or_incomplete".into());}
        Ok(parsed)
    }.await;
    Some(result)
}
async fn refresh(a: &App, lease: &Arc<RunLease>, id: i64) -> Result<(), ApiError> {
    let saved = {
        let db = a.db.lock().unwrap();
        lease.validate(&db)?;
        source(&db, id)?
    };
    let mut result = async {
        if saved.channel_epg {
            return Err("guide_size_limit".to_owned());
        }
        let client = crate::provider::egress::builder(
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(45))
                .redirect(reqwest::redirect::Policy::none()),
            saved.proxy.as_deref(),
        )?
        .build()
        .map_err(|_| "guide_request_failed")?;
        let mut response = client
            .get(&saved.url)
            .send()
            .await
            .map_err(|_| "guide_request_failed")?;
        if !response.status().is_success() {
            return Err("guide_http_failed".to_owned());
        }
        if response
            .content_length()
            .is_some_and(|n| n > 32 * 1024 * 1024)
        {
            return Err("guide_size_limit".into());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| "guide_request_failed")? {
            if bytes.len() + chunk.len() > 32 * 1024 * 1024 {
                return Err("guide_size_limit".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        tokio::task::spawn_blocking(move || parse(&bytes))
            .await
            .map_err(|_| "guide_parse_failed")?
    }
    .await;
    let mut channel_epg = saved.channel_epg;
    if result
        .as_ref()
        .is_err_and(|reason| reason == "guide_size_limit")
    {
        match tokio::time::timeout(
            Duration::from_secs(120),
            selected_channel_epg(a, lease, &saved),
        )
        .await
        {
            Ok(Some(value)) => {
                if value.is_ok() {
                    channel_epg = true;
                }
                result = value;
            }
            Ok(None) => {}
            Err(_) => result = Err("guide_channel_epg_timeout".into()),
        }
    }

    let db = a.db.clone();
    let lease = lease.clone();
    blocking(move||{let mut db=db.lock().unwrap();lease.validate(&db)?;if source(&db,id)?.revision!=saved.revision{return Err("Guide refresh cancelled or source changed".into());}let rules=policy(&db)?;
        match result{
            Ok(parsed)=>{let tx=db.transaction().map_err(db_error)?;
                for (guide,name) in parsed.channels{tx.execute("INSERT INTO guide_channels(source_id,guide_id,name) VALUES(?1,?2,?3) ON CONFLICT(source_id,guide_id) DO UPDATE SET name=excluded.name",params![saved.id,guide,name]).map_err(db_error)?;}
                reconcile_authoritative(&tx,id,&parsed.programs)?;
                let mut ranges=HashMap::<String,(i64,i64)>::new();for(id,start,end,_)in &parsed.programs{ranges.entry(id.clone()).and_modify(|r|{r.0=r.0.min(*start);r.1=r.1.max(*end);}).or_insert((*start,*end));}
                for(guide,(start,end))in ranges{tx.execute("DELETE FROM source_programmes WHERE source_id=?1 AND guide_id=?2 AND start<?4 AND end>?3",params![id,guide,start,end]).map_err(db_error)?;}
                for(guide,start,end,data)in parsed.programs{tx.execute("INSERT OR REPLACE INTO source_programmes(source_id,guide_id,start,end,data) VALUES(?1,?2,?3,?4,?5)",params![id,guide,start,end,data.to_string()]).map_err(db_error)?;}
                tx.execute("DELETE FROM source_programmes WHERE end<?1",[util::now()-86400]).map_err(db_error)?;tx.execute("UPDATE guide_sources SET updated_at=?2,attempt_at=?2,reason=NULL,next_refresh=?3,last_generation=?4,channel_epg=?5 WHERE id=?1",params![id,util::now(),util::now()+rules.refresh_minutes as i64*60,lease.generation,channel_epg]).map_err(db_error)?;
                lease.validate(&tx)?;tx.commit().map_err(db_error)?;
            },Err(reason)=>{db.execute("UPDATE guide_sources SET attempt_at=?2,reason=?3,next_refresh=?4,last_generation=?5 WHERE id=?1",params![id,util::now(),reason,util::now()+900,lease.generation]).map_err(db_error)?;event(&db,Some(id),None,&reason)?;}
        }Ok(axum::Json(json!({"ok":true})))
    }).await.map(|_|())
}
// Refresh corrections replace only intervals already supplied by this exact
// source mapping. Programmes from another source retain their precedence.
fn reconcile_authoritative(
    db: &Connection,
    source: i64,
    programs: &[(String, i64, i64, Value)],
) -> Result<(), ApiError> {
    let mut grouped = HashMap::<&str, Vec<&(String, i64, i64, Value)>>::new();
    for row in programs {
        grouped.entry(&row.0).or_default().push(row);
    }
    for (guide, mut incoming) in grouped {
        incoming.sort_by_key(|r| (r.1, r.2));
        let start = incoming.iter().map(|r| r.1).min().unwrap();
        let end = incoming.iter().map(|r| r.2).max().unwrap();
        let families = {
            let mut q=db.prepare("SELECT DISTINCT channel_id FROM family_programmes WHERE source_id=?1 AND guide_id=?2 AND start<?4 AND end>?3").map_err(db_error)?;
            let ids = q
                .query_map(params![source, guide, start, end], |r| {
                    r.get::<_, String>(0)
                })
                .map_err(db_error)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(db_error)?;
            ids
        };
        for family in families {
            let valid:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM guide_mappings m JOIN guide_channels g ON g.source_id=m.source_id AND g.guide_id=m.guide_id AND g.name=m.observed_name WHERE m.channel_id=?1 AND m.source_id=?2 AND m.guide_id=?3)",params![family,source,guide],|r|r.get(0)).map_err(db_error)?;
            if !valid {
                continue;
            }
            let mut occupied = {
                let mut q=db.prepare("SELECT start,end FROM family_programmes WHERE channel_id=?1 AND NOT(source_id=?2 AND guide_id=?3 AND start<?5 AND end>?4)").map_err(db_error)?;
                let rows = q
                    .query_map(params![family, source, guide, start, end], |r| {
                        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
                    })
                    .map_err(db_error)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(db_error)?;
                rows
            };
            // Conflicting correction requires owner review rather than damaging a valid
            // adjacent programme or fabricating a clipped start/end time.
            if incoming
                .iter()
                .any(|r| occupied.iter().any(|(a, b)| r.1 < *b && r.2 > *a))
            {
                event(db, Some(source), Some(&family), "guide_correction_conflict")?;
                continue;
            }
            let mut accepted = Vec::new();
            for row in &incoming {
                if occupied.iter().any(|(a, b)| row.1 < *b && row.2 > *a) {
                    continue;
                }
                occupied.push((row.1, row.2));
                accepted.push(*row);
            }
            db.execute("DELETE FROM family_programmes WHERE channel_id=?1 AND source_id=?2 AND guide_id=?3 AND start<?5 AND end>?4",params![family,source,guide,start,end]).map_err(db_error)?;
            for row in accepted {
                db.execute("INSERT INTO family_programmes(channel_id,source_id,guide_id,start,end,data) VALUES(?1,?2,?3,?4,?5,?6)",params![family,source,guide,row.1,row.2,row.3.to_string()]).map_err(db_error)?;
            }
        }
    }
    Ok(())
}

pub(crate) fn start(a: &App) {
    if a.guide_control.started.swap(true, Ordering::AcqRel) {
        return;
    }
    let mut a = a.clone();
    let life = Arc::downgrade(&a.automation_life);
    a.automation_life = Arc::new(());
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if life.upgrade().is_none() || a.playback.is_shutting_down() {
                break;
            }
            let claim = {
                let db = a.db.lock().unwrap();
                claim(&a, &db)
            };
            let Ok(Some((run, sources))) = claim else {
                continue;
            };
            a.guide_control.busy.store(true, Ordering::Release);
            for source in sources {
                if run.cancelled.load(Ordering::Acquire) {
                    break;
                }
                let stop = async {
                    while !run.cancelled.load(Ordering::Acquire) && !a.playback.is_shutting_down() {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    run.cancelled.store(true, Ordering::Release);
                };
                tokio::select! {_=refresh(&a,&run,source)=>{},_=stop=>break}
            }
            let app = a.clone();
            let access = run.clone();
            let result=tokio::task::spawn_blocking(move||{
   let mut db=app.db.lock().unwrap();
   let mut reason=None;
   if access.validate(&db).is_ok(){
    if automatic_mappings(&db).is_err(){reason=Some("guide_mapping_failed");}
    if policy(&db)?.automatic_repair {
     let ids={let mut q=db.prepare("SELECT id FROM family_channels WHERE json_extract(data,'$.enabled')=1 LIMIT 1000").map_err(db_error)?;let rows=q.query_map([],|r|r.get::<_,String>(0)).map_err(db_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;rows};
     for id in ids{if access.validate(&db).is_err(){break;}
if repair_guarded(&mut db,&id,true,Some(&access)).is_err(){reason=Some("guide_repair_failed");}}
    }
   }
   let cancelled=access.validate(&db).is_err();
   let failures:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM guide_sources WHERE last_generation=?1 AND reason IS NOT NULL)",[access.generation],|r|r.get(0)).map_err(db_error)?;
   if failures&&reason.is_none(){reason=Some("some_sources_failed");}
   if cancelled{reason=Some("cancelled");}
   db.execute("UPDATE guide_runs SET state=?2,last_finish=?3,reason=?4 WHERE id=1 AND generation=?1",params![access.generation,if cancelled{"cancelled"}else{"completed"},util::now(),reason]).map_err(db_error)?;
   let interval=policy(&db)?.audit_minutes;
   db.execute("UPDATE guide_settings SET next_audit=?1 WHERE id=1",[util::now()+interval as i64*60]).map_err(db_error)?;
   Ok::<_,ApiError>(())
  }).await;
            if !matches!(result, Ok(Ok(()))) {
                let _=a.db.lock().unwrap().execute("UPDATE guide_runs SET state='failed',last_finish=?2,reason='guide_run_failed' WHERE id=1 AND generation=?1",params![run.generation,util::now()]);
            }
            a.guide_control.busy.store(false, Ordering::Release);
            a.guide_control.current.lock().unwrap().take();
        }
    });
}
type QueuedRun = (i64, i64, i64, Option<String>);
type ClaimedRun = (Arc<RunLease>, Vec<i64>);
fn claim(a: &App, db: &Connection) -> Result<Option<ClaimedRun>, ApiError> {
    let queued:Option<QueuedRun>=db.query_row("SELECT owner_id,last_start,generation,reason FROM guide_runs WHERE id=1 AND state='queued'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(db_error)?;
    let (owner, began, generation, manual) = if let Some((owner, began, generation, reason)) =
        queued
    {
        (
            owner,
            began,
            if reason.as_deref() == Some("resuming_after_restart") {
                generation
            } else {
                generation + 1
            },
            true,
        )
    } else {
        let rules = policy(db)?;
        if !rules.enabled || crate::activity::paused(db) {
            return Ok(None);
        }
        let owner: Option<i64> = db
            .query_row("SELECT owner_id FROM guide_settings WHERE id=1", [], |r| {
                r.get(0)
            })
            .map_err(db_error)?;
        let Some(owner) = owner else {
            return Ok(None);
        };
        let due:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM guide_sources WHERE enabled=1 AND next_refresh<=?1) OR (SELECT next_audit<=?1 FROM guide_settings WHERE id=1)",[util::now()],|r|r.get(0)).map_err(db_error)?;
        if !due {
            return Ok(None);
        }
        let generation: i64 = db
            .query_row("SELECT generation+1 FROM guide_runs WHERE id=1", [], |r| {
                r.get(0)
            })
            .map_err(db_error)?;
        (owner, util::now(), generation, false)
    };
    let access = crate::automation::OwnerAccess::scheduled(owner);
    if access.validate(db).is_err() {
        db.execute(
            "UPDATE guide_runs SET state='failed',reason='authorization_lost' WHERE id=1",
            [],
        )
        .map_err(db_error)?;
        db.execute(
            "UPDATE guide_settings SET next_audit=?1 WHERE id=1",
            [util::now() + 3600],
        )
        .map_err(db_error)?;
        return Ok(None);
    }
    let run = Arc::new(RunLease {
        owner: access,
        generation,
        cancelled: AtomicBool::new(false),
    });
    db.execute("UPDATE guide_runs SET state='running',owner_id=?1,last_start=?2,generation=?3,reason=NULL WHERE id=1",params![owner,began,generation]).map_err(db_error)?;
    // This happens under the same database lock as pause/cancel and publication.
    *a.guide_control.current.lock().unwrap() = Some(run.clone());
    let mut q=db.prepare("SELECT id FROM guide_sources WHERE enabled=1 AND ((?1 AND last_generation<>?2) OR (NOT ?1 AND next_refresh<=?3)) ORDER BY id LIMIT 20").map_err(db_error)?;
    let sources = q
        .query_map(params![manual, generation, util::now()], |r| r.get(0))
        .map_err(db_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error)?;
    Ok(Some((run, sources)))
}
pub(crate) fn pause(a: &App, db: &Connection) -> Result<(), ApiError> {
    if let Some(run) = a.guide_control.current.lock().unwrap().as_ref() {
        run.cancelled.store(true, Ordering::Release);
    }
    db.execute("UPDATE guide_runs SET state=CASE WHEN state='queued' THEN 'cancelled' ELSE 'cancel_requested' END,reason='cancelled' WHERE state IN ('queued','running')",[]).map_err(db_error)?;
    Ok(())
}
