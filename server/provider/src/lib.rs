//! Pure addon/provider client logic for the VIPTV backend server, which vendors
//! this crate with `scripts/sync-provider.sh`. The Crux core no longer depends
//! on it: the application-facing provider bridge was retired (BE-002).
//!
//! No networking or storage lives here: consumers fetch bytes and feed the
//! parsed JSON back in. Modules are feature-gated so each consumer pulls only
//! what it needs. `extras` and `candidate` are default; `discovery` is opt-in
//! and pulls in catalog planning, aggregation, and addon URL construction.

pub mod normalize;

#[cfg(feature = "extras")]
pub mod extras;

#[cfg(feature = "candidate")]
pub mod candidate;

#[cfg(feature = "discovery")]
pub mod discover;
#[cfg(feature = "discovery")]
pub mod discovery_values;

pub use normalize::validate_url;
