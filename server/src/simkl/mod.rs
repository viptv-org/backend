use crate::*;
use viptv_engine::{capabilities::Capability, profile::IntegrationConnection};
mod automatic;
mod completion;
pub(crate) use automatic::schedule_progress;
mod event_fence;
mod history;
pub(crate) use history::progress;
mod lifecycle;
pub(crate) use lifecycle::playback;
mod cache;
mod client;
mod mapping;
pub(crate) use mapping::{resolve_id, resolve_request};
mod discovery;
pub(crate) mod browse;
pub(crate) mod calendar;
mod metadata;
pub(crate) use discovery::{home_cards, page};
mod oauth;
mod store;
#[cfg(test)]
mod tests;
mod tracking;
mod tokens;
pub(crate) use client::Service;
pub(crate) use metadata::{detail, search, Page, Search};
pub(crate) use oauth::{authorize, callback, disconnect, status};
pub(crate) use store::{connections, init};
pub(crate) use tracking::watched;

pub(crate) fn capabilities() -> Vec<Capability> {
    vec![
        Capability::Discovery,
        Capability::Authorization,
        Capability::Search,
        Capability::Metadata,
        Capability::Tracking,
    ]
}

fn unavailable() -> ApiError {
    ApiError(
        StatusCode::SERVICE_UNAVAILABLE,
        "Simkl is not configured on this server".into(),
    )
}

fn require_edit(app: &App, lease: &ResourceLease, profile: i64) -> Result<(), ApiError> {
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_profile(&db, profile)?;
    let auth::Principal::Account { role, .. } = &lease.principal;
    if role == "device" {
        return Err(auth::forbidden());
    }
    kids::require_parent(&db, &lease.principal)
}
