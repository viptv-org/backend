//! Pure addon/provider client logic shared between the VIPTV backend server and
//! the shared Crux core so both sides validate, normalize, plan, and aggregate
//! the same catalog/media requests.
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

pub use normalize::validate_url;
