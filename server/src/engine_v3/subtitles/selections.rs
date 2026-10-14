use crate::*;
const RETENTION: Duration = Duration::from_secs(180);
#[derive(Clone)]
pub(super) struct Selection {
    pub scope: viptv_engine::profile::Scope,
    pub lease: ResourceLease,
    pub addon_revision: String,
    pub kind: String,
    pub media_id: String,
    pub url: String,
    pub language: String,
    pub label: String,
    pub created: Instant,
    pub content_vtt: Option<Arc<str>>,
    pub retained_until: Option<Instant>,
}
impl Selection {
    fn retained(&self, now: Instant) -> bool {
        self.retained_until.is_some_and(|until| until > now)
    }
    fn fresh(&self, now: Instant) -> bool {
        self.retained(now) || now.saturating_duration_since(self.created) < Duration::from_secs(600)
    }
    fn owned(&self, lease: &ResourceLease) -> bool {
        let auth::Principal::Account {
            account_id,
            profile_id,
            ..
        } = lease.principal;
        lease.session_id.is_some()
            && self.lease.session_id == lease.session_id
            && self.scope.account_id.parse::<i64>().ok() == Some(account_id)
            && self.scope.profile_id.parse::<i64>().ok() == profile_id
    }
}
#[derive(Default)]
pub(crate) struct Selections(Mutex<HashMap<String, Selection>>);
impl Selections {
    pub(super) fn insert(
        &self,
        lease: &ResourceLease,
        items: Vec<(String, Selection)>,
    ) -> Result<(), ApiError> {
        let mut entries = self
            .0
            .lock()
            .map_err(|_| ApiError::from("Subtitles unavailable"))?;
        let now = Instant::now();
        entries.retain(|_, entry| {
            entry.retained(now) || (entry.fresh(now) && entry.lease.session_id != lease.session_id)
        });
        if entries.len() + items.len() > 4096 {
            return Err(ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "Subtitle selection is busy. Try again.".into(),
            ));
        }
        entries.extend(items);
        Ok(())
    }
    pub(super) fn get(&self, id: &str, lease: &ResourceLease) -> Result<Selection, ApiError> {
        self.get_at(id, lease, Instant::now())
    }
    fn get_at(&self, id: &str, lease: &ResourceLease, now: Instant) -> Result<Selection, ApiError> {
        let mut entries = self
            .0
            .lock()
            .map_err(|_| ApiError::from("Subtitles unavailable"))?;
        entries.retain(|_, entry| entry.fresh(now));
        entries
            .get(id)
            .filter(|entry| entry.owned(lease))
            .cloned()
            .ok_or_else(auth::forbidden)
    }
    pub(crate) fn retain_content(
        &self,
        id: &str,
        lease: &ResourceLease,
        text: Arc<str>,
    ) -> Result<(), ApiError> {
        self.retain_at(id, lease, text, Instant::now())
    }
    fn retain_at(
        &self,
        id: &str,
        lease: &ResourceLease,
        text: Arc<str>,
        now: Instant,
    ) -> Result<(), ApiError> {
        if text.len() > 2 * 1024 * 1024 {
            return Err("Subtitle content is too large".into());
        }
        let mut entries = self
            .0
            .lock()
            .map_err(|_| ApiError::from("Subtitles unavailable"))?;
        entries.retain(|_, entry| entry.fresh(now));
        for entry in entries.values_mut().filter(|entry| !entry.retained(now)) {
            entry.content_vtt = None;
        }
        let selected = entries
            .get(id)
            .filter(|entry| entry.owned(lease))
            .ok_or_else(auth::forbidden)?;
        if selected
            .content_vtt
            .as_ref()
            .is_some_and(|known| known.as_ref() != text.as_ref())
        {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "Subtitle representation changed".into(),
            ));
        }
        let (count, bytes) = entries
            .values()
            .filter_map(|entry| entry.content_vtt.as_ref())
            .fold((0, 0), |(count, bytes), text| {
                (count + 1, bytes + text.len())
            });
        if selected.content_vtt.is_none() && (count >= 128 || bytes + text.len() > 32 * 1024 * 1024)
        {
            return Err(ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "Subtitle content is busy. Try again.".into(),
            ));
        }
        let selected = entries.get_mut(id).unwrap();
        selected.content_vtt = Some(text);
        selected.retained_until = Some(now + RETENTION);
        Ok(())
    }
    pub(crate) fn keep_alive(&self, ids: &[String], lease: &ResourceLease) -> Result<(), ApiError> {
        self.keep_alive_at(ids, lease, Instant::now())
    }
    fn keep_alive_at(
        &self,
        ids: &[String],
        lease: &ResourceLease,
        now: Instant,
    ) -> Result<(), ApiError> {
        let mut entries = self
            .0
            .lock()
            .map_err(|_| ApiError::from("Subtitles unavailable"))?;
        for id in ids {
            if let Some(entry) = entries.get_mut(id).filter(|entry| {
                entry.owned(lease) && entry.retained(now) && entry.content_vtt.is_some()
            }) {
                entry.retained_until = Some(now + RETENTION);
            }
        }
        Ok(())
    }
}
#[cfg(test)]
#[path = "selection_retention_tests.rs"]
mod tests;
