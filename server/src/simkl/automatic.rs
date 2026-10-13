use super::*;
use std::collections::HashSet;

pub(super) struct Queue {
    pub(super) events: super::event_fence::Events,
    pub(super) workers: Arc<tokio::sync::Semaphore>,
    pending: Mutex<HashSet<String>>,
    profiles: Mutex<HashMap<i64, std::sync::Weak<tokio::sync::Mutex<()>>>>,
}
impl Default for Queue {
    fn default() -> Self {
        Self {
            events: Default::default(),
            workers: Arc::new(tokio::sync::Semaphore::new(4)),
            pending: Default::default(),
            profiles: Default::default(),
        }
    }
}
impl Queue {
    #[cfg(test)]
    pub(super) fn pending_count(&self) -> usize {
        self.pending.lock().unwrap().len()
    }
    pub(super) fn reserve(&self, key: &str) -> bool {
        let Ok(mut pending) = self.pending.lock() else {
            return false;
        };
        if pending.len() >= 128 {
            return false;
        }
        pending.insert(key.into())
    }
    pub(super) fn release(&self, key: &str) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(key);
        }
    }
    pub(super) fn profile_gate(
        &self,
        profile: i64,
    ) -> Result<Arc<tokio::sync::Mutex<()>>, ApiError> {
        let mut profiles = self
            .profiles
            .lock()
            .map_err(|_| ApiError::from("Simkl tracking unavailable"))?;
        profiles.retain(|_, gate| gate.strong_count() > 0);
        if let Some(gate) = profiles.get(&profile).and_then(|g| g.upgrade()) {
            return Ok(gate);
        }
        let gate = Arc::new(tokio::sync::Mutex::new(()));
        profiles.insert(profile, Arc::downgrade(&gate));
        Ok(gate)
    }
}

pub(super) struct Reservation {
    service: Arc<Service>,
    key: String,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        self.service.tracking.release(&self.key);
    }
}
pub(super) fn reserve(service: Arc<Service>, key: String) -> Option<Reservation> {
    service
        .tracking
        .reserve(&key)
        .then(|| Reservation { service, key })
}

/// Called only after an authenticated history write succeeds; saving history never awaits egress.
pub(crate) fn schedule_progress(app: App, profile: i64, value: Value, explicit_watched: bool) {
    let completed = value["position"]
        .as_f64()
        .zip(value["duration"].as_f64())
        .is_some_and(|(p, d)| p.is_finite() && d.is_finite() && d > 0.0 && p / d >= 0.95);
    if !completed && !explicit_watched {
        return;
    }
    let Ok(q) = tracking::Watched::canonical(&value) else {
        return;
    };
    let Ok(credential) = store::credential(&app, profile, Capability::Tracking) else {
        return;
    };
    let Some(revision) = credential.revision else {
        return;
    };
    let id = value["id"].as_str().unwrap_or_default().to_owned();
    let receipt_id = if explicit_watched {
        let Ok(db) = app.db.lock() else { return };
        let Ok(at) = db.query_row(
            "SELECT updated_at FROM progress WHERE profile_id=?1 AND id=?2 AND type=?3",
            params![profile, id, value["type"].as_str()],
            |row| row.get::<_, i64>(0),
        ) else {
            return;
        };
        format!("{id}#explicit:{at}")
    } else {
        id.clone()
    };
    let key = format!("history:{profile}:{receipt_id}:{revision}");
    let Some(reservation) = reserve(app.simkl.clone(), key) else {
        return;
    };
    let queued = (|| -> Result<bool, ApiError> {
        let db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        app.require_profile(&db, profile)?;
        kids::require_item(
            &db,
            &app.identity(),
            value["type"].as_str().unwrap_or_default(),
            &id,
        )?;
        let changed=db.execute("INSERT INTO simkl_tracking_receipts VALUES(?1,?2,?3,'pending',?4) ON CONFLICT(profile_id,media_id,link_revision) DO UPDATE SET state='pending',attempt_at=excluded.attempt_at WHERE simkl_tracking_receipts.state!='done' AND simkl_tracking_receipts.attempt_at<=?4-60",params![profile,receipt_id,revision,util::now()]).map_err(db_error)?;
        Ok(changed == 1)
    })();
    if !matches!(queued, Ok(true)) {
        return;
    }
    let watched_at = chrono::DateTime::<chrono::Utc>::from_timestamp(util::now(), 0)
        .map(|date| date.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    tokio::spawn(async move {
        let _reservation = reservation;
        let result = tokio::time::timeout(Duration::from_secs(30), async {
            let _permit = app
                .simkl
                .tracking
                .workers
                .acquire()
                .await
                .map_err(|_| unavailable())?;
            let gate = app.simkl.tracking.profile_gate(profile)?;
            let _serial = gate.lock().await;
            store::validate(&app, profile, &credential)?;
            if !explicit_watched && completion::confirmed(&app, profile, &id, revision)? {
                return Ok(());
            }
            tracking::record(&app, profile, &q, &credential, watched_at.as_deref()).await?;
            completion::confirm(&app, profile, &id, revision)?;
            if receipt_id != id {
                completion::confirm(&app, profile, &receipt_id, revision)?;
            }
            Ok::<(), ApiError>(())
        })
        .await;
        let done = matches!(result, Ok(Ok(())));
        if let Ok(db) = app.db.lock() {
            let _=db.execute("UPDATE simkl_tracking_receipts SET state=?4 WHERE profile_id=?1 AND media_id=?2 AND link_revision=?3 AND state!='done'",params![profile,receipt_id,revision,if done{"done"}else{"failed"}]);
        }
    });
}

pub(super) fn status(db: &Connection, profile: i64, revision: u64) -> Result<Value, ApiError> {
    let (pending,failed):(u64,u64)=db.query_row("SELECT COALESCE(SUM(state='pending'),0),COALESCE(SUM(state='failed'),0) FROM simkl_tracking_receipts WHERE profile_id=?1 AND link_revision=?2",params![profile,revision],|r|Ok((r.get(0)?,r.get(1)?))).map_err(db_error)?;
    Ok(json!({"pending":pending,"failed":failed}))
}
