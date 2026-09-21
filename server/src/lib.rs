mod activity;
pub mod addon;
pub mod auth;
mod automation;
mod continuation;
mod guides;
mod health;
mod kids;
mod media_access;
mod library;mod lineup;
mod live_catalog;
mod live_policy;
pub mod playback;
mod preferences;
pub mod provider;
mod service_health;
mod session;
pub mod util;

mod app_state;
mod handlers_catalog;
mod handlers_media;
mod handlers_profiles;
mod http_middleware;
mod matching_context;
mod routes;
mod sources;

pub(crate) use crate::{addon::Addons, playback::PlaybackManager, provider::ProviderService};
pub(crate) use axum::{
    extract::{Path, Query, Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{delete, get, post},
    Extension, Router,
};
pub(crate) use rusqlite::{params, Connection, OptionalExtension};
pub(crate) use serde::Deserialize;
pub(crate) use serde_json::{json, Value};
pub(crate) use session::{heartbeat, media, start_playback, stop_playback, PlaybackRequest};
pub(crate) use sha2::{Digest, Sha256};
pub(crate) use std::{
    collections::{HashMap, HashSet},
    convert::Infallible,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
pub(crate) use tokio::sync::Notify;
pub(crate) use tower_http::cors::CorsLayer;
pub(crate) use uuid::Uuid;

#[cfg(test)]
mod auth_integration_tests;
#[cfg(test)]
mod contract_tests;
#[cfg(test)]
pub(crate) mod test_support;

// The original monolithic lib.rs was split into the modules above without
// changing any bodies. These re-exports keep the crate root namespace
// identical: every `use super::*` consumer (session, kids, guides, the
// contract tests, ...) keeps resolving the same names, and the public
// surface (App, ApiError, router, router_with_tv) is unchanged.
pub(crate) use app_state::*;
pub use app_state::{ApiError, App};
pub(crate) use handlers_catalog::*;
pub(crate) use handlers_media::*;
pub(crate) use handlers_profiles::*;
pub(crate) use http_middleware::*;
pub(crate) use matching_context::*;
pub use routes::{router, router_with_tv};
pub(crate) use sources::*;
