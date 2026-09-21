//! Media-path lease validation on the blocking pool.
//!
//! [`ResourceLease::validate`] is one fused query; running it through the
//! blocking pool keeps async workers off the process-wide database mutex on
//! the hottest path in the server — every media request and every delivered
//! chunk validates the viewer lease. No decision is ever cached: revocation
//! must take effect on the next request or delivered chunk, which the shared
//! playback contract tests pin.
use super::*;

impl ResourceLease {
    pub(crate) async fn validate_media(&self, a: &App) -> Result<(), ApiError> {
        let lease = self.clone();
        let worker = a.clone();
        blocking(move || {
            let db = worker.db.lock().unwrap();
            lease.validate(&db)
        })
        .await
    }
}
