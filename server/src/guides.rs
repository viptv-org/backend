//! Persistent source-scoped XMLTV data, exact-feed mappings and conservative gap filling.
use super::*;
use chrono::{DateTime, Utc};
use quick_xml::{events::Event, Reader};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Policy {
    enabled: bool,
    timezone: String,
    refresh_minutes: u64,
    audit_minutes: u64,
    automatic_repair: bool,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            enabled: false,
            timezone: "America/New_York".into(),
            refresh_minutes: 360,
            audit_minutes: 60,
            automatic_repair: true,
        }
    }
}
#[derive(Default)]
pub(crate) struct Control {
    started: AtomicBool,
    busy: AtomicBool,
    current: Mutex<Option<Arc<RunLease>>>,
}
struct RunLease {
    owner: crate::automation::OwnerAccess,
    generation: i64,
    cancelled: AtomicBool,
}
impl RunLease {
    fn validate(&self, db: &Connection) -> Result<(), ApiError> {
        self.owner.validate(db)?;
        let valid:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM guide_runs WHERE id=1 AND generation=?1 AND state='running')",[self.generation],|r|r.get(0)).map_err(db_error)?;
        if self.cancelled.load(Ordering::Acquire) || !valid {
            return Err("Guide run cancelled or superseded".into());
        }
        Ok(())
    }
}
#[derive(Clone)]
struct Source {
    id: i64,
    url: String,
    revision: String,
    channel_epg: bool,
    proxy: Option<String>,
}
struct Parsed {
    channels: HashMap<String, String>,
    programs: Vec<(String, i64, i64, Value)>,
}
pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS guide_settings(id INTEGER PRIMARY KEY CHECK(id=1),data TEXT NOT NULL,owner_id INTEGER,next_audit INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS guide_sources(id INTEGER PRIMARY KEY AUTOINCREMENT,name TEXT NOT NULL,url TEXT,provider_id INTEGER,enabled INTEGER NOT NULL DEFAULT 1,updated_at INTEGER,attempt_at INTEGER,reason TEXT,next_refresh INTEGER NOT NULL DEFAULT 0,last_generation INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS guide_channels(source_id INTEGER NOT NULL,guide_id TEXT NOT NULL,name TEXT NOT NULL,PRIMARY KEY(source_id,guide_id));
CREATE TABLE IF NOT EXISTS source_programmes(source_id INTEGER NOT NULL,guide_id TEXT NOT NULL,start INTEGER NOT NULL,end INTEGER NOT NULL,data TEXT NOT NULL,PRIMARY KEY(source_id,guide_id,start,end));
CREATE INDEX IF NOT EXISTS source_programmes_lookup ON source_programmes(source_id,guide_id,end);
CREATE TABLE IF NOT EXISTS guide_mappings(channel_id TEXT NOT NULL,source_id INTEGER NOT NULL,guide_id TEXT NOT NULL,observed_name TEXT NOT NULL,pinned INTEGER NOT NULL DEFAULT 0,verified INTEGER NOT NULL DEFAULT 0,priority INTEGER NOT NULL DEFAULT 100,PRIMARY KEY(channel_id,source_id,guide_id));
CREATE TABLE IF NOT EXISTS family_programmes(channel_id TEXT NOT NULL,source_id INTEGER NOT NULL,guide_id TEXT NOT NULL,start INTEGER NOT NULL,end INTEGER NOT NULL,data TEXT NOT NULL,PRIMARY KEY(channel_id,start,end));
CREATE TABLE IF NOT EXISTS guide_runs(id INTEGER PRIMARY KEY CHECK(id=1),state TEXT NOT NULL DEFAULT 'idle',generation INTEGER NOT NULL DEFAULT 0,owner_id INTEGER,last_start INTEGER,last_finish INTEGER,reason TEXT);
INSERT OR IGNORE INTO guide_runs(id) VALUES(1);
UPDATE guide_runs SET state='queued',reason='resuming_after_restart' WHERE state='running';
UPDATE guide_runs SET state='cancelled',reason='cancelled' WHERE state='cancel_requested';
CREATE TABLE IF NOT EXISTS guide_rejections(channel_id TEXT NOT NULL,source_id INTEGER NOT NULL,guide_id TEXT NOT NULL,PRIMARY KEY(channel_id,source_id,guide_id));
CREATE TABLE IF NOT EXISTS guide_events(id INTEGER PRIMARY KEY AUTOINCREMENT,at INTEGER NOT NULL,source_id INTEGER,channel_id TEXT,reason TEXT NOT NULL);")?;
    let columns = db
        .prepare("PRAGMA table_info(guide_sources)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !columns.iter().any(|name| name == "channel_epg") {
        db.execute(
            "ALTER TABLE guide_sources ADD COLUMN channel_epg INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    db.execute(
        "INSERT OR IGNORE INTO guide_settings(id,data) VALUES(1,?1)",
        [serde_json::to_string(&Policy::default()).unwrap()],
    )?;
    Ok(())
}
fn policy(db: &Connection) -> Result<Policy, ApiError> {
    let s: String = db
        .query_row("SELECT data FROM guide_settings WHERE id=1", [], |r| {
            r.get(0)
        })
        .map_err(db_error)?;
    serde_json::from_str(&s).map_err(|_| "Guide settings invalid".into())
}
fn timestamp(s: &str) -> Result<i64, String> {
    DateTime::parse_from_str(s.trim(), "%Y%m%d%H%M%S %z")
        .map(|d| d.timestamp())
        .map_err(|_| "guide_timestamp_invalid".into())
}
fn xml_text(bytes: &[u8]) -> Result<String, String> {
    let raw = std::str::from_utf8(bytes).map_err(|_| "guide_encoding_invalid")?;
    quick_xml::escape::unescape(raw)
        .map(|s| s.into_owned())
        .map_err(|_| "guide_entity_invalid".into())
}
fn parse(bytes: &[u8]) -> Result<Parsed, String> {
    if bytes.len() > 32 * 1024 * 1024 {
        return Err("guide_size_limit".into());
    }
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut stack = Vec::<String>::new();
    let mut channels = HashMap::new();
    let mut programs = Vec::new();
    let mut channel = None::<String>;
    let mut name = String::new();
    let mut program = None::<(String, i64, i64, Value)>;
    let mut root = false;
    let mut capture_name = false;
    loop {
        let event = reader.read_event().map_err(|_| "guide_xml_invalid")?;
        let empty = matches!(&event, Event::Empty(_));
        match event {
            Event::DocType(e) => {
                if e.as_ref().contains('[') {
                    return Err("guide_doctype_unsupported".into());
                }
            }
            Event::Start(e) | Event::Empty(e) => {
                let tag = e.name().as_ref().to_owned();
                let attrs = e
                    .attributes()
                    .map(|v| {
                        let v = v.map_err(|_| "guide_xml_invalid")?;
                        Ok((v.key.as_ref().to_owned(), xml_text(v.value.as_bytes())?))
                    })
                    .collect::<Result<HashMap<_, _>, String>>()?;
                if stack.is_empty() {
                    if tag != "tv" || root {
                        return Err("guide_xml_invalid".into());
                    }
                    root = true;
                }
                if stack.len() > 32 {
                    return Err("guide_depth_limit".into());
                }
                if tag == "channel" {
                    channel = Some(
                        attrs
                            .get("id")
                            .filter(|s| !s.is_empty() && s.len() <= 512)
                            .ok_or("guide_channel_invalid")?
                            .clone(),
                    );
                    name.clear();
                }
                if tag == "display-name" {
                    capture_name = name.is_empty();
                }
                if tag == "programme" {
                    let id = attrs.get("channel").ok_or("guide_channel_invalid")?.clone();
                    let start = timestamp(attrs.get("start").ok_or("guide_timestamp_invalid")?)?;
                    let end = timestamp(attrs.get("stop").ok_or("guide_timestamp_invalid")?)?;
                    if end <= start || end - start > 86400 {
                        return Err("guide_interval_invalid".into());
                    }
                    program = Some((id, start, end, json!({})));
                }
                if tag == "icon" {
                    if let Some((_, _, _, data)) = program.as_mut() {
                        if let Some(url) = attrs
                            .get("src")
                            .filter(|s| s.len() <= 2048 && util::validate_url(s).is_ok())
                        {
                            data["icon"] = json!(url);
                        }
                    }
                }
                if !empty {
                    stack.push(tag);
                }
            }
            Event::Text(e) => {
                let text = xml_text(e.as_ref().as_bytes())?;
                append_text(&stack, &mut name, &mut program, &text, capture_name)?;
            }
            Event::CData(e) => {
                let text = e.as_ref();
                append_text(&stack, &mut name, &mut program, text, capture_name)?;
            }
            Event::GeneralRef(e) => {
                let text = xml_text(format!("&{};", e.as_ref()).as_bytes())?;
                append_text(&stack, &mut name, &mut program, &text, capture_name)?;
            }
            Event::End(e) => {
                let tag = e.name().as_ref().to_owned();
                if stack.pop().as_deref() != Some(tag.as_str()) {
                    return Err("guide_xml_invalid".into());
                }
                if tag == "display-name" {
                    capture_name = false;
                }
                if tag == "channel" {
                    let id = channel.take().ok_or("guide_channel_invalid")?;
                    if name.trim().is_empty()
                        || channels.insert(id, name.trim().to_owned()).is_some()
                    {
                        return Err("guide_channel_invalid".into());
                    }
                    if channels.len() > 50000 {
                        return Err("guide_channel_limit".into());
                    }
                }
                if tag == "programme" {
                    let row = program.take().ok_or("guide_programme_invalid")?;
                    if row.3["title"].as_str().is_none_or(|s| s.trim().is_empty()) {
                        return Err("guide_title_invalid".into());
                    }
                    if row.2 > util::now() - 86400 && row.1 < util::now() + 8 * 86400 {
                        programs.push(row);
                    }
                    if programs.len() > 250000 {
                        return Err("guide_programme_limit".into());
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !root || !stack.is_empty() || channels.is_empty() || programs.is_empty() {
        return Err("guide_empty_or_incomplete".into());
    }
    if programs
        .iter()
        .any(|(id, _, _, _)| !channels.contains_key(id))
    {
        return Err("guide_channel_missing".into());
    }
    Ok(Parsed { channels, programs })
}
fn append_text(
    stack: &[String],
    name: &mut String,
    program: &mut Option<(String, i64, i64, Value)>,
    text: &str,
    capture_name: bool,
) -> Result<(), String> {
    if let Some((_, _, _, data)) = program {
        let tag = stack.last().map(String::as_str).unwrap_or("");
        if [
            "title",
            "sub-title",
            "desc",
            "category",
            "date",
            "episode-num",
            "value",
            "country",
            "language",
        ]
        .contains(&tag)
        {
            let key = match tag {
                "desc" => "description",
                "sub-title" => "subtitle",
                "episode-num" => "episode",
                "value" => "rating",
                v => v,
            };
            let mut value = data[key].as_str().unwrap_or("").to_owned();
            value.push_str(text);
            if value.len() > 8192 {
                return Err("guide_text_limit".into());
            }
            data[key] = json!(value);
        }
    } else if capture_name && stack.last().is_some_and(|s| s == "display-name") {
        if name.len() + text.len() > 512 {
            return Err("guide_text_limit".into());
        }
        name.push_str(text);
    }
    Ok(())
}
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

fn event(
    db: &Connection,
    source: Option<i64>,
    channel: Option<&str>,
    reason: &str,
) -> Result<(), ApiError> {
    db.execute(
        "INSERT INTO guide_events(at,source_id,channel_id,reason) VALUES(?1,?2,?3,?4)",
        params![util::now(), source, channel, reason],
    )
    .map_err(db_error)?;
    db.execute("DELETE FROM guide_events WHERE id NOT IN (SELECT id FROM guide_events ORDER BY id DESC LIMIT 200)",[]).map_err(db_error)?;
    Ok(())
}
fn channel(db: &Connection, id: &str) -> Result<Value, ApiError> {
    let data: String = db
        .query_row("SELECT data FROM family_channels WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .map_err(db_error)?;
    serde_json::from_str(&data).map_err(|_| "Family channel invalid".into())
}
fn programmes(db: &Connection, id: &str) -> Result<Vec<Value>, ApiError> {
    let mut q=db.prepare("SELECT f.start,f.end,f.data,f.source_id,f.guide_id,s.updated_at FROM family_programmes f JOIN guide_sources s ON s.id=f.source_id AND s.enabled=1 JOIN guide_mappings m ON m.channel_id=f.channel_id AND m.source_id=f.source_id AND m.guide_id=f.guide_id JOIN guide_channels g ON g.source_id=m.source_id AND g.guide_id=m.guide_id AND g.name=m.observed_name WHERE f.channel_id=?1 AND f.end>?2 ORDER BY f.start,f.end LIMIT 2000").map_err(db_error)?;
    let result = q
        .query_map(params![id, util::now()], |r| {
            let mut data: Value =
                serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or(json!({}));
            data["start"] = json!(r.get::<_, i64>(0)?);
            data["end"] = json!(r.get::<_, i64>(1)?);
            data["source_id"] = json!(r.get::<_, i64>(3)?);
            data["guide_id"] = json!(r.get::<_, String>(4)?);
            data["source_updated_at"] = json!(r.get::<_, Option<i64>>(5)?);
            Ok(data)
        })
        .map_err(db_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error);
    result
}
fn coverage(programs: &[Value], now: i64) -> Value {
    let mut cursor = now;
    let mut covered24 = 0;
    let mut covered72 = 0;
    let mut gaps = Vec::new();
    let mut overlaps = 0;
    for p in programs {
        let start = p["start"].as_i64().unwrap_or(0).max(now);
        let end = p["end"].as_i64().unwrap_or(0);
        if end <= now {
            continue;
        }
        if start > cursor && cursor < now + 72 * 3600 && gaps.len() < 50 {
            gaps.push(json!({"start":cursor,"end":start.min(now+72*3600)}));
        }
        if start < cursor {
            overlaps += 1;
        }
        let from = start.max(cursor);
        covered24 += (end.min(now + 86400) - from).max(0);
        covered72 += (end.min(now + 72 * 3600) - from).max(0);
        cursor = cursor.max(end);
    }
    if cursor < now + 72 * 3600 && gaps.len() < 50 {
        gaps.push(json!({"start":cursor,"end":now+72*3600}));
    }
    json!({"current_programme":programs.iter().find(|p|p["start"].as_i64().unwrap_or(i64::MAX)<=now&&p["end"].as_i64().unwrap_or(0)>now),"coverage_24_hours":covered24 as f64/3600.0,"coverage_72_hours":covered72 as f64/3600.0,"gaps":gaps,"overlaps":overlaps,"needs_attention":covered24<86400,"horizon":cursor})
}
fn automatic_mappings(db: &Connection) -> Result<(), ApiError> {
    let mut q = db
        .prepare(
            "SELECT id,data FROM family_channels WHERE json_extract(data,'$.enabled')=1 LIMIT 1000",
        )
        .map_err(db_error)?;
    let channels = q
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(db_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error)?;
    let mut suggestions = Vec::new();
    for (id, data) in &channels {
        let value: Value = serde_json::from_str(data).map_err(|_| "Family channel invalid")?;
        let needle = format!(
            "%{}%",
            value["network"]
                .as_str()
                .unwrap_or("")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let mut q=db.prepare("SELECT c.source_id,c.guide_id,c.name FROM guide_channels c JOIN guide_sources s ON s.id=c.source_id AND s.enabled=1 WHERE c.name LIKE ?1 ESCAPE '\\' ORDER BY c.source_id,c.guide_id LIMIT 100").map_err(db_error)?;
        for row in q
            .query_map([needle], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(db_error)?
        {
            let (source, guide, name) = row.map_err(db_error)?;
            if lineup::matching::compatible_guide(&value, &name, false) {
                suggestions.push((id.clone(), source, guide, name, false));
            }
        }
    }
    // A provider-scoped EPG ID is trusted only through a currently verified
    // stream mapping; identical IDs on unrelated providers never imply identity.
    for (id, data) in &channels {
        let value: Value = serde_json::from_str(data).map_err(|_| "Family channel invalid")?;
        let mut q = db.prepare("SELECT DISTINCT g.source_id,g.guide_id,g.name FROM family_candidates c JOIN provider_live l ON l.id=c.live_id JOIN family_channels f ON f.id=c.channel_id JOIN guide_sources s ON s.provider_id=l.provider_id AND s.enabled=1 JOIN guide_channels g ON g.source_id=s.id AND (g.guide_id=l.epg_channel_id OR g.guide_id='xtream-stream:'||l.stream_id) WHERE c.channel_id=?1 AND EXISTS(SELECT 1 FROM json_each(f.data,'$.candidates') saved WHERE json_extract(saved.value,'$.id')=l.id AND json_extract(saved.value,'$.name')=l.name) LIMIT 100").map_err(db_error)?;
        let rows = q
            .query_map([id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(db_error)?;
        for row in rows {
            let (source, guide, name) = row.map_err(db_error)?;
            if lineup::matching::compatible_guide(&value, &name, true) {
                suggestions.retain(|(family, sid, gid, _, _)| {
                    !(family == id && *sid == source && gid == &guide)
                });
                suggestions.push((id.clone(), source, guide, name, true));
            }
        }
    }
    let mut counts = HashMap::new();
    for (_, source, guide, _, _) in &suggestions {
        *counts.entry((*source, guide.clone())).or_insert(0) += 1;
    }
    let tx = db.unchecked_transaction().map_err(db_error)?;
    tx.execute("DELETE FROM guide_mappings WHERE pinned=0", [])
        .map_err(db_error)?;
    for (id, source, guide, name, verified) in suggestions {
        if counts[&(source, guide.clone())] != 1 {
            continue;
        }
        tx.execute("INSERT OR IGNORE INTO guide_mappings(channel_id,source_id,guide_id,observed_name,pinned,verified,priority) SELECT ?1,?2,?3,?4,0,?5,100 WHERE NOT EXISTS(SELECT 1 FROM guide_rejections WHERE channel_id=?1 AND source_id=?2 AND guide_id=?3)",params![id,source,guide,name,verified]).map_err(db_error)?;
    }
    tx.commit().map_err(db_error)?;
    Ok(())
}
fn repair(db: &mut Connection, id: &str, apply: bool) -> Result<Value, ApiError> {
    repair_guarded(db, id, apply, None)
}
fn repair_guarded(
    db: &mut Connection,
    id: &str,
    apply: bool,
    guard: Option<&RunLease>,
) -> Result<Value, ApiError> {
    if let Some(guard) = guard {
        guard.validate(db)?;
    }
    let family = channel(db, id)?;
    let existing = programmes(db, id)?;
    let mut intervals = existing
        .iter()
        .map(|p| (p["start"].as_i64().unwrap(), p["end"].as_i64().unwrap()))
        .collect::<Vec<_>>();
    let mut additions = Vec::new();
    {
        let mut q=db.prepare("SELECT p.source_id,p.guide_id,p.start,p.end,p.data,g.name,(m.pinned OR m.verified) FROM guide_mappings m JOIN guide_sources s ON s.id=m.source_id AND s.enabled=1 JOIN guide_channels g ON g.source_id=m.source_id AND g.guide_id=m.guide_id AND g.name=m.observed_name JOIN source_programmes p ON p.source_id=m.source_id AND p.guide_id=m.guide_id WHERE m.channel_id=?1 AND p.end>?2 ORDER BY m.pinned DESC,m.priority,s.updated_at DESC,p.source_id,p.start,p.end LIMIT 10000").map_err(db_error)?;
        for row in q
            .query_map(params![id, util::now()], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, bool>(6)?,
                ))
            })
            .map_err(db_error)?
        {
            let (source, guide, start, end, data, name, pinned) = row.map_err(db_error)?;
            if !lineup::matching::compatible_guide(&family, &name, pinned)
                || intervals.iter().any(|(a, b)| start < *b && end > *a)
            {
                continue;
            }
            intervals.push((start, end));
            let data: Value = serde_json::from_str(&data).map_err(|_| "Stored guide invalid")?;
            additions.push(json!({"source_id":source,"guide_id":guide,"start":start,"end":end,"programme":data}));
        }
    }
    let revision = format!(
        "{:x}",
        Sha256::digest(json!([id, existing, additions]).to_string())
    );
    if apply {
        let tx = db.transaction().map_err(db_error)?;
        tx.execute(
            "DELETE FROM family_programmes WHERE end<?1",
            [util::now() - 86400],
        )
        .map_err(db_error)?;
        for row in &additions {
            tx.execute("INSERT OR IGNORE INTO family_programmes(channel_id,source_id,guide_id,start,end,data) VALUES(?1,?2,?3,?4,?5,?6)",params![id,row["source_id"].as_i64(),row["guide_id"].as_str(),row["start"].as_i64(),row["end"].as_i64(),row["programme"].to_string()]).map_err(db_error)?;
        }
        event(&tx, None, Some(id), "compatible_gaps_filled")?;
        if let Some(guard) = guard {
            guard.validate(&tx)?;
        }
        tx.commit().map_err(db_error)?;
    }
    Ok(
        json!({"channel_id":id,"revision":revision,"additions":additions,"preserved":existing.len(),"applied":apply}),
    )
}
pub(crate) fn read(db: &Connection, id: &str) -> Result<Value, ApiError> {
    let rules = policy(db)?;
    let timezone: chrono_tz::Tz = rules
        .timezone
        .parse()
        .map_err(|_| "Guide timezone invalid")?;
    let mut programs = programmes(db, id)?;
    for p in &mut programs {
        if let Some(date) = DateTime::<Utc>::from_timestamp(p["start"].as_i64().unwrap_or(0), 0) {
            let local = date.with_timezone(&timezone);
            p["display_time"] = json!(local.format("%-I:%M %p %Z").to_string());
            p["display_date"] = json!(local.format("%Y-%m-%d").to_string());
        }
        p["id"] = json!(format!("{id}:{}", p["start"]));
    }
    let now = util::now();
    let timeline: Vec<Value> = (0..54).filter_map(|slot| {
        let start = now / 1800 * 1800 + slot * 1800;
        let local = DateTime::<Utc>::from_timestamp(start, 0)?.with_timezone(&timezone);
        Some(json!({"start":start,"display_time":local.format("%-I:%M %p %Z").to_string(),"display_date":local.format("%a %b %-d").to_string()}))
    }).collect();
    let status = coverage(&programs, now);
    programs.truncate(100);
    Ok(
        json!({"programs":programs,"timezone":rules.timezone,"timeline":timeline,"coverage":status,"missing":status["current_programme"].is_null()}),
    )
}
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn xmltv_decodes_metadata_and_rejects_invalid_or_unsafe_documents() {
        let now = DateTime::<Utc>::from_timestamp(util::now(), 0).unwrap();
        let xml = format!("<?xml version=\"1.0\"?><tv><channel id=\"cn\"><display-name>Cartoon Network East</display-name><display-name>CN</display-name></channel><programme channel=\"cn\" start=\"{}\" stop=\"{}\"><title>Tom &amp; Jerry</title><desc><![CDATA[Family <fun>]]></desc><icon src=\"https://example.org/icon.png\"/></programme></tv>",now.format("%Y%m%d%H%M%S %z"),(now+chrono::Duration::hours(1)).format("%Y%m%d%H%M%S %z"));
        let value = parse(xml.as_bytes()).unwrap();
        assert_eq!(value.channels["cn"], "Cartoon Network East");
        assert_eq!(value.programs.len(), 1);
        assert_eq!(value.programs[0].3["title"], "Tom & Jerry");
        for bad in ["<tv/>","<tv><channel></tv>","<!DOCTYPE tv [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><tv>&x;</tv>","<tv><programme channel='cn' start='invalid' stop='invalid'><title>x</title></programme></tv>"] {assert!(parse(bad.as_bytes()).is_err(),"{bad}");}
    }
    #[test]
    fn timestamps_and_household_display_preserve_dst_and_coverage_counts_union() {
        let before = timestamp("20261101013000 -0400").unwrap();
        let after = timestamp("20261101013000 -0500").unwrap();
        assert_eq!(after - before, 3600);
        let zone: chrono_tz::Tz = "America/New_York".parse().unwrap();
        assert!(DateTime::<Utc>::from_timestamp(before, 0)
            .unwrap()
            .with_timezone(&zone)
            .format("%Z")
            .to_string()
            .contains("EDT"));
        assert!(DateTime::<Utc>::from_timestamp(after, 0)
            .unwrap()
            .with_timezone(&zone)
            .format("%Z")
            .to_string()
            .contains("EST"));
        let c = coverage(
            &[
                json!({"start":0,"end":3600}),
                json!({"start":1800,"end":5400}),
                json!({"start":86400,"end":90000}),
            ],
            0,
        );
        assert_eq!(c["coverage_24_hours"], 1.5);
        assert_eq!(c["overlaps"], 1);
        assert_eq!(c["needs_attention"], true);
    }
}
