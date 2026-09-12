//! Select and reserve under the same database/limiter locks. A later viewer sees
//! the reservation immediately, before either request starts media preparation.
use super::*;
use rusqlite::OptionalExtension;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use tokio::sync::OwnedSemaphorePermit;

pub(super) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS family_input_observations(live_id TEXT PRIMARY KEY,source_key TEXT NOT NULL,device_settings_key TEXT NOT NULL,at INTEGER NOT NULL,healthy INTEGER NOT NULL,startup_ms INTEGER NOT NULL);")
}
#[derive(Clone)]
pub(crate) struct ObservationKey {
    pub(crate) candidate: String,
    pub(crate) source: String,
    device_settings: String,
}
pub(crate) struct Reservation {
    pub candidate: String,
    pub url: String,
    pub permit: OwnedSemaphorePermit,
    pub explanation: Value,
    pub observation: ObservationKey,
}
pub(crate) struct Selection {
    pub reservation: Option<Reservation>,
    pub skipped: Vec<Value>,
}
struct Ranked {
    candidate: String,
    provider: i64,
    url: String,
    pool: Value,
    healthy: bool,
    startup_ms: u64,
    rank: usize,
    observation: ObservationKey,
}
pub(crate) fn current_source(db: &Connection, candidate: &str) -> Result<String, String> {
    let (provider, stream, name) = db.query_row("SELECT p.id,p.name,p.url,p.username,p.password,l.stream_id,l.name FROM provider_live l JOIN providers p ON p.id=l.provider_id WHERE l.id=?1", [candidate], |r| Ok((Provider{id:r.get(0)?,name:r.get(1)?,url:r.get(2)?,username:r.get(3)?,password:r.get(4)?},r.get::<_,String>(5)?,r.get::<_,String>(6)?))).map_err(db_error)?;
    let url = media_url(&provider, "live", &stream, "ts")?;
    Ok(format!("{:x}", Sha256::digest(format!("{url}\n{name}"))))
}
impl ProviderService {
    pub(crate) fn reserve_family(
        &self,
        channel: &str,
        excluded: &HashSet<String>,
        device_settings: &str,
    ) -> Result<Selection, String> {
        let db = self.lock()?;
        let mut gates = self
            .playback_gates
            .lock()
            .map_err(|_| "Account limiter unavailable")?;
        let mut skipped = Vec::new();
        let mut ranked = Vec::new();
        for (rank, candidate) in crate::lineup::candidates(&db, channel)?
            .into_iter()
            .enumerate()
        {
            if excluded.contains(&candidate) {
                continue;
            }
            if !crate::lineup::eligible(&db, channel, &candidate)? {
                skipped.push(json!({"candidate_id":candidate,"reason":"unavailable_or_changed"}));
                continue;
            }
            let (provider, stream, name) = db.query_row("SELECT p.id,p.name,p.url,p.username,p.password,l.stream_id,l.name FROM provider_live l JOIN providers p ON p.id=l.provider_id WHERE l.id=?1", [&candidate], |r| Ok((Provider{id:r.get(0)?,name:r.get(1)?,url:r.get(2)?,username:r.get(3)?,password:r.get(4)?},r.get::<_,String>(5)?,r.get::<_,String>(6)?))).map_err(db_error)?;
            let url = media_url(&provider, "live", &stream, "ts")?;
            let observation = ObservationKey {
                candidate: candidate.clone(),
                source: format!("{:x}", Sha256::digest(format!("{url}\n{name}"))),
                device_settings: device_settings.to_owned(),
            };
            let observed = db.query_row("SELECT at,healthy,startup_ms FROM family_input_observations WHERE live_id=?1 AND source_key=?2 AND device_settings_key=?3",params![candidate,observation.source,device_settings],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,bool>(1)?,r.get::<_,u64>(2)?))).optional().map_err(db_error)?;
            let recent = observed
                .filter(|(at, _, _)| *at <= crate::util::now() && crate::util::now() - at <= 300);
            if recent.is_some_and(|(at, healthy, _)| !healthy && crate::util::now() - at < 60) {
                skipped.push(json!({"candidate_id":candidate,"reason":"recent_input_failure","retry_after_seconds":60-(crate::util::now()-recent.unwrap().0)}));
                continue;
            }
            let pool = pools::ensure(&db, provider.id)?;
            let status = pools::snapshot(&db, &gates, pool)?;
            if status["estimated_free"] == 0 {
                skipped.push(json!({"candidate_id":candidate,"reason":"connections_busy","pool_id":pool,"confidence":status["confidence"]}));
                continue;
            }
            ranked.push(Ranked {
                candidate,
                provider: provider.id,
                url,
                pool: status,
                healthy: recent.is_some_and(|(_, healthy, _)| healthy),
                startup_ms: recent
                    .filter(|(_, healthy, _)| *healthy)
                    .map_or(u64::MAX, |(_, _, ms)| ms),
                rank,
                observation,
            });
        }
        ranked.sort_by(|a, b| {
            b.pool["estimated_free"]
                .as_u64()
                .cmp(&a.pool["estimated_free"].as_u64())
                .then_with(|| b.healthy.cmp(&a.healthy))
                .then_with(|| a.startup_ms.cmp(&b.startup_ms))
                .then_with(|| a.rank.cmp(&b.rank))
                .then_with(|| a.candidate.cmp(&b.candidate))
        });
        let alternatives = ranked.iter().skip(1).map(|r|json!({"candidate_id":r.candidate,"estimated_free":r.pool["estimated_free"],"reason":"lower_selection_rank"})).collect::<Vec<_>>();
        let reservation = if let Some(selected) = ranked.into_iter().next() {
            let permit = pools::acquire(&db, &mut gates, selected.provider)?;
            let explanation = json!({"candidate_id":selected.candidate,"reason":"selected","pool_id":selected.pool["id"],"estimated_free_before":selected.pool["estimated_free"],"confidence":selected.pool["confidence"],"recent_success":selected.healthy,"startup_ms":(selected.startup_ms!=u64::MAX).then_some(selected.startup_ms),"quality_preference_order":selected.rank+1,"alternatives":alternatives});
            Some(Reservation {
                candidate: selected.candidate,
                url: selected.url,
                permit,
                explanation,
                observation: selected.observation,
            })
        } else {
            None
        };
        Ok(Selection {
            reservation,
            skipped,
        })
    }
    pub(crate) fn record_family_input(
        &self,
        key: &ObservationKey,
        healthy: bool,
        startup_ms: u64,
    ) -> Result<(), String> {
        let db = self.db.try_lock().map_err(|_| "Observation store busy")?;
        db.execute("INSERT INTO family_input_observations(live_id,source_key,device_settings_key,at,healthy,startup_ms) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(live_id) DO UPDATE SET source_key=excluded.source_key,device_settings_key=excluded.device_settings_key,at=excluded.at,healthy=excluded.healthy,startup_ms=excluded.startup_ms",params![key.candidate,key.source,key.device_settings,crate::util::now(),healthy,startup_ms]).map_err(db_error)?;
        Ok(())
    }
}
