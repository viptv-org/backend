use crate::*;

#[derive(Clone)]
pub(super) struct Selection {
    pub scope: viptv_engine::profile::Scope,
    pub lease: ResourceLease,
    pub addon_revision: String,
    pub kind: String,
    pub media_id: String,
    pub url: String,
    pub created: Instant,
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
        entries.retain(|_, entry| {
            entry.created.elapsed() < Duration::from_secs(600)
                && entry.lease.session_id != lease.session_id
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
        let entries = self
            .0
            .lock()
            .map_err(|_| ApiError::from("Subtitles unavailable"))?;
        let selection = entries
            .get(id)
            .filter(|item| {
                item.created.elapsed() < Duration::from_secs(600)
                    && item.lease.session_id.is_some()
                    && item.lease.session_id == lease.session_id
            })
            .ok_or_else(auth::forbidden)?;
        Ok(selection.clone())
    }
}
