//! Shared domain contracts and execution for native and server hosts.
pub mod capabilities;
#[cfg(feature = "runtime")]
pub mod home;
pub mod identity;
pub mod presentation;
pub mod profile;
pub mod playback;
pub mod query;
pub mod registry;
pub mod stremio;
pub mod simkl;
#[cfg(feature = "runtime")]
pub mod store;

pub const CONTRACT_VERSION: u32 = 3;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("This integration is not available in this mode")]
    Unavailable,
    #[error("This profile cannot use that connection")]
    Forbidden,
    #[error("This configuration has changed; reload and try again")]
    Conflict,
    #[error("The supplied query is not supported")]
    InvalidQuery,
    #[error("This title or episode could not be identified")]
    Unresolved,
    #[error("Local data could not be read or saved")]
    Storage,
}

pub mod source_integration;
#[cfg(feature = "vsources-runtime")]
pub mod vsources_runtime;

pub mod preferences;

#[cfg(feature="source-runtime")]
pub mod egress;

pub mod live;

pub mod search;

pub mod library;

pub mod continuation;

pub mod home_browse;

pub mod origin;
