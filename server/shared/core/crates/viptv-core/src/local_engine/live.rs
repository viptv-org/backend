use super::*;
use viptv_engine::{
    capabilities::{Availability, Capability},
    live::*,
    profile::IntegrationConnection,
};
#[cfg_attr(not(feature = "live-runtime"), allow(dead_code))]
#[derive(Clone)]
pub(super) struct Cache {
    revision: u64,
    expires: std::time::Instant,
    channels: Vec<PrivateChannel>,
    groups: Vec<LiveGroup>,
    guides: std::collections::HashMap<String, (std::time::Instant, LiveGuide)>,
    xmltv: Option<(std::time::Instant, Arc<str>)>,
}

mod channels;
mod favorites;
#[cfg(feature = "live-runtime")]
mod connections;
#[cfg(feature = "live-runtime")]
mod directory;
mod guide;
#[cfg(feature = "live-runtime")]
mod http;
#[cfg(feature = "live-runtime")]
mod lookup;
#[cfg(all(test, feature = "live-runtime"))]
mod public_qualification;
mod sources;
#[cfg(all(test, feature = "live-runtime"))]
mod tests;

#[cfg(all(test, feature = "live-runtime"))]
pub(super) type Fixture =
    Arc<dyn Fn(&Scope, &str) -> Result<(String, String), CoreError> + Send + Sync>;
#[cfg(feature = "live-runtime")]
fn deadline() -> std::time::Instant {
    std::time::Instant::now() + std::time::Duration::from_secs(10)
}

#[cfg(feature = "live-runtime")]
mod xmltv_cache;

#[cfg(all(test, feature="live-runtime"))]
mod favorites_tests;
