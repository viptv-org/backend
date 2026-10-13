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
