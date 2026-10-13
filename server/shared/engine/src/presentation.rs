use crate::{identity::MediaIdentity, profile::Scope};
use facet::Facet;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
pub struct Card {
    pub identity: MediaIdentity,
    pub provider_id: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub description: Option<String>,
    pub poster: Option<String>,
    pub landscape: Option<String>,
    pub logo: Option<String>,
    pub progress: Option<f64>,
    pub badges: Vec<String>,
    #[serde(default)]
    pub parent_identity: Option<MediaIdentity>,
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub added_at_millis:Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Facet)]
#[repr(C)]
pub enum RowState {
    Pending,
    Ready,
    Empty,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
pub struct HomeRow {
    pub id: String,
    pub title: String,
    pub provider_id: String,
    pub state: RowState,
    pub cards: Vec<Card>,
    pub next_cursor: Option<String>,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
pub struct HomeSnapshot {
    pub scope: Scope,
    pub revision: u64,
    pub hero: Option<Card>,
    pub rows: Vec<HomeRow>,
    pub complete: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
pub struct DetailSnapshot {
    pub item: Card,
    pub episodes: Vec<Card>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
pub struct CatalogPage {
    pub cards: Vec<Card>,
    pub next_cursor: Option<String>,
    pub complete: bool,
}

impl Card {
    /// Bounded typed favorites retain canonical identity without parsing provider IDs.
    pub fn validate_library(&self) -> Result<(), crate::Error> {
        let valid_identity = |identity: &MediaIdentity| {
            !identity.id.is_empty()
                && identity.id.len() <= 512
                && !identity.id.chars().any(char::is_control)
                && matches!(identity.kind.as_str(), "movie" | "series")
                && identity.ids.len() <= 32
                && identity.ids.iter().all(|id| {
                    !id.namespace.is_empty()
                        && id.namespace.len() <= 64
                        && !id.value.is_empty()
                        && id.value.len() <= 512
                        && !id
                            .namespace
                            .chars()
                            .chain(id.value.chars())
                            .any(char::is_control)
                })
                && identity
                    .edition
                    .as_ref()
                    .is_none_or(|s| s.len() <= 512 && !s.chars().any(char::is_control))
        };
        if !valid_identity(&self.identity)
            || self.title.is_empty()
            || self.title.len() > 512
            || self.title.chars().any(char::is_control)
            || self.provider_id.is_empty()
            || self.provider_id.len() > 128
            || self.provider_id.chars().any(char::is_control)
            || self.badges.len() > 32
            || self
                .badges
                .iter()
                .any(|s| s.len() > 128 || s.chars().any(char::is_control))
            || self.description.as_ref().is_some_and(|s| {
                s.len() > 16_384 || s.chars().any(|c| c.is_control() && c != '\n' && c != '\t')
            })
            || [&self.poster, &self.landscape, &self.logo, &self.subtitle]
                .into_iter()
                .flatten()
                .any(|s| {
                    s.len() > 4096 || s.chars().any(|c| c.is_control() && c != '\n' && c != '\t')
                })
            || self.parent_identity.as_ref().is_some_and(|p| {
                !valid_identity(p)
                    || self.identity.episode.is_none()
                    || p.kind != "series"
                    || p.episode.is_some()
            })
            || serde_json::to_vec(self)
                .map_err(|_| crate::Error::InvalidQuery)?
                .len()
                > 64 * 1024
        {
            return Err(crate::Error::InvalidQuery);
        }
        Ok(())
    }
}
