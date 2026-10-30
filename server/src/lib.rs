mod account_api;
mod android_links;
pub mod addon;
pub mod auth;
mod continuation;
mod engine_v3;
#[cfg(test)]
mod discovery_error_tests;
mod gateway;
mod kids;
mod library;
pub mod migration_v2;
mod preferences;
pub mod provider;
mod secret_store;
mod service_errors;
mod source_http;
mod images;
mod simkl;
mod stremio_import;
pub mod util;

mod app_state;
mod handlers_catalog;
mod handlers_media;
mod handlers_profiles;
mod http_middleware;
mod matching_context;
mod routes;
mod sources;

pub(crate) use crate::{addon::Addons, provider::ProviderService};
pub(crate) use axum::{
    extract::{Path, Query, Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Extension, Router,
};
pub(crate) use rusqlite::{params, Connection, OptionalExtension};
pub(crate) use serde::Deserialize;
pub(crate) use serde_json::{json, Value};
pub(crate) use sha2::{Digest, Sha256};
pub(crate) use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
pub(crate) use tokio::sync::Notify;
pub(crate) use uuid::Uuid;

#[cfg(test)]
mod auth_integration_tests;
#[cfg(test)]
mod contract_tests;
#[cfg(test)]
mod preserved_data_tests;
#[cfg(test)]
pub(crate) mod test_support;

pub(crate) use app_state::*;
pub use app_state::{ApiError, App};
pub(crate) use handlers_catalog::*;
pub(crate) use handlers_media::*;
pub(crate) use handlers_profiles::*;
pub(crate) use http_middleware::*;
pub(crate) use matching_context::*;

pub fn start_anime_show_migration(app: App) {
    engine_v3::anime_show_migration::start(app);
}

pub use routes::{router, router_with_viewers, validate_tv_dist};

#[cfg(test)]
mod remote_qa_fixture;

#[cfg(test)]
mod real_gateway_qa;
