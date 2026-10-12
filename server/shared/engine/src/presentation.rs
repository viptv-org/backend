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
