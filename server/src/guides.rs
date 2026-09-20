//! Persistent source-scoped XMLTV data, exact-feed mappings and conservative gap filling.
use super::*;
use chrono::{DateTime, Utc};
use quick_xml::{events::Event, Reader};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};

mod parse;
mod query;
mod refresh;
mod repair;

use parse::parse;
#[cfg(test)]
use parse::timestamp;
pub(crate) use query::{
    add_source, cancel, configure, enable_source, list, map_channel, repair_channel, run_now,
};
pub(crate) use refresh::{pause, start};
pub(crate) use repair::read;
use repair::{automatic_mappings, channel, coverage, programmes, repair, repair_guarded};

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
