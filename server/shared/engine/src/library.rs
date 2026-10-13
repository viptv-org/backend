//! Canonical personal-library queries and real, independently optional watch statistics.
use crate::{
    Error,
    capabilities::Availability,
    identity::{EpisodeCoordinates, MediaIdentity},
    presentation::Card,
    profile::Scope,
    query::{Choice, ControlKind, QueryControl},
};
use facet::Facet;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
pub struct LibraryStatistics {
    pub identity: MediaIdentity,
    pub watched: bool,
    pub last_watched_millis: Option<u64>,
    pub times_watched: Option<u64>,
    pub user_rating: Option<f64>,
    pub watched_episode_count: Option<u64>,
    pub total_episode_count: Option<u64>,
    pub aired_episode_count: Option<u64>,
    #[serde(default)]
    pub has_unwatched_aired_episodes: Option<bool>,
    pub status: Option<String>,
    pub next_episode: Option<EpisodeCoordinates>,
    pub next_air_millis: Option<u64>,
}
impl LibraryStatistics {
    pub fn empty(identity: MediaIdentity) -> Self {
        Self {
            identity,
            watched: false,
            last_watched_millis: None,
            times_watched: None,
            user_rating: None,
            watched_episode_count: None,
            total_episode_count: None,
            aired_episode_count: None,
            has_unwatched_aired_episodes: None,
            status: None,
            next_episode: None,
            next_air_millis: None,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
pub struct LibrarySource {
    pub id: String,
    pub label: String,
    pub availability: Availability,
    pub reason: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
pub struct LibrarySnapshot {
    pub scope: Scope,
    pub revision: u64,
    pub query: LibraryQuery,
    pub cards: Vec<Card>,
    pub next_cursor: Option<String>,
    pub complete: bool,
    pub controls: Vec<QueryControl>,
    pub sources: Vec<LibrarySource>,
    pub statistics: Vec<LibraryStatistics>,
    pub availability: Availability,
    pub reason: Option<String>,
    #[serde(default)]
    pub refresh_after_millis: Option<u64>,
}
#[derive(Clone)]
pub struct Entry {
    pub card: Card,
    pub statistics: LibraryStatistics,
}

mod controls;
mod project;
mod query;
pub use controls::controls;
pub use project::project;
pub use query::{LibraryQuery, added_lower_bound};
