use super::*;
use std::collections::HashSet;

#[derive(Default)]
pub(super) struct Events(Mutex<HashMap<String, Entry>>);
struct Entry {
    lease: ResourceLease,
    attempt: String,
    item: String,
    sequence: u64,
    retired: HashSet<String>,
}
pub(super) struct Fence {
    key: String,
    attempt: String,
    sequence: u64,
}
fn replaced() -> ApiError {
    ApiError(
        StatusCode::CONFLICT,
        "Simkl playback event was replaced".into(),
    )
}

impl Events {
    pub(super) fn admit(
        &self,
        db: &Connection,
        lease: &ResourceLease,
        profile: i64,
        attempt: &str,
        item: &str,
        sequence: u64,
        event: &str,
    ) -> Result<Fence, ApiError> {
        if attempt.is_empty()
            || attempt.len() > 128
            || !attempt
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
            || sequence == 0
        {
            return Err("Invalid playback attempt".into());
        }
        let session = lease.session_id.as_ref().ok_or_else(auth::unauthorized)?;
        let key = format!(
            "{}:{profile}:{session}",
            lease.principal.account_id().ok_or_else(auth::forbidden)?
        );
        let mut events = self.0.lock().map_err(|_| unavailable())?;
        events.retain(|_, entry| entry.lease.validate(db).is_ok());
        if let Some(current) = events.get_mut(&key) {
            if current.attempt == attempt {
                if item != current.item || sequence <= current.sequence {
                    return Err(replaced());
                }
                current.sequence = sequence;
            } else {
                if event != "start" || sequence != 1 || current.retired.contains(attempt) {
                    return Err(replaced());
                }
                if current.retired.len() >= 128 {
                    return Err(ApiError(
                        StatusCode::TOO_MANY_REQUESTS,
                        "Simkl playback tracking capacity reached".into(),
                    ));
                }
                current
                    .retired
                    .insert(std::mem::replace(&mut current.attempt, attempt.into()));
                current.sequence = sequence;
                current.item = item.into();
            }
        } else {
            if event != "start" || sequence != 1 {
                return Err(replaced());
            }
            if events.len() >= 256 {
                return Err(ApiError(
                    StatusCode::TOO_MANY_REQUESTS,
                    "Simkl playback tracking capacity reached".into(),
                ));
            }
            events.insert(
                key.clone(),
                Entry {
                    lease: lease.clone(),
                    attempt: attempt.into(),
                    item: item.into(),
                    sequence,
                    retired: Default::default(),
                },
            );
        }
        Ok(Fence {
            key,
            attempt: attempt.into(),
            sequence,
        })
    }
    pub(super) fn validate(&self, fence: &Fence) -> Result<(), ApiError> {
        let events = self.0.lock().map_err(|_| unavailable())?;
        if !events
            .get(&fence.key)
            .is_some_and(|entry| entry.attempt == fence.attempt && entry.sequence == fence.sequence)
        {
            return Err(replaced());
        }
        Ok(())
    }
}
