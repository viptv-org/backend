//! Xtream indexes are internal stream candidates, never discovery catalogs.
//! Call `init` during database startup before constructing `ProviderService`.
use base64::{engine::general_purpose::STANDARD, Engine};
use futures::{stream, StreamExt};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::time::Duration;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};
use tokio::sync::Semaphore;
use unicode_normalization::{char::is_combining_mark, UnicodeNormalization};
use url::Url;

pub(crate) mod accounts;
pub(crate) mod egress;
pub(crate) mod pools;
pub(crate) mod selection;

mod candidates;
mod http;
mod live;
mod normalize;
mod service;
mod streams;
mod sync;

#[cfg(test)]
mod tests;

pub use self::service::ProviderService;
use candidates::*;
use normalize::*;
use service::*;

const MAX_RESPONSE: usize = 64 * 1024 * 1024;
const MAX_ITEMS: usize = 300_000;
// Detail discovery is demand-driven, never a full-library metadata sync.
const MAX_LAZY_DETAILS: usize = 8;

pub fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS providers (
            id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL,
            url TEXT NOT NULL, username TEXT NOT NULL, password TEXT NOT NULL,
            enabled INTEGER NOT NULL DEFAULT 1
        );
        CREATE TABLE IF NOT EXISTS provider_live (
            id TEXT PRIMARY KEY, provider_id INTEGER NOT NULL,
            stream_id TEXT NOT NULL, name TEXT NOT NULL, logo TEXT,
            category TEXT, category_id TEXT, epg_channel_id TEXT,
            FOREIGN KEY(provider_id) REFERENCES providers(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS provider_live_provider ON provider_live(provider_id);
        CREATE TABLE IF NOT EXISTS provider_vod (
            id TEXT PRIMARY KEY, provider_id INTEGER NOT NULL, stream_id TEXT NOT NULL,
            kind TEXT NOT NULL, name TEXT NOT NULL, normalized TEXT NOT NULL,
            year INTEGER, imdb_id TEXT, tmdb_id TEXT, extension TEXT NOT NULL,
            poster TEXT,
            FOREIGN KEY(provider_id) REFERENCES providers(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS provider_vod_lookup ON provider_vod(kind, normalized, year);
        CREATE INDEX IF NOT EXISTS provider_vod_provider ON provider_vod(provider_id);
        CREATE INDEX IF NOT EXISTS provider_vod_imdb ON provider_vod(imdb_id);
        CREATE INDEX IF NOT EXISTS provider_vod_tmdb ON provider_vod(tmdb_id);
        CREATE TABLE IF NOT EXISTS provider_matches (
            vod_id TEXT PRIMARY KEY, metadata_id TEXT NOT NULL, kind TEXT NOT NULL,
            FOREIGN KEY(vod_id) REFERENCES provider_vod(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS provider_matches_metadata ON provider_matches(metadata_id);
        CREATE TABLE IF NOT EXISTS provider_cache (
            provider_id INTEGER NOT NULL, cache_key TEXT NOT NULL, expires_at INTEGER NOT NULL,
            payload TEXT NOT NULL, PRIMARY KEY(provider_id,cache_key),
            FOREIGN KEY(provider_id) REFERENCES providers(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS provider_cache_expiry ON provider_cache(expires_at);",
    )?;
    let has_limit = db
        .prepare("PRAGMA table_info(providers)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|name| name == "max_connections");
    if !has_limit {
        db.execute_batch(
            "ALTER TABLE providers ADD COLUMN max_connections INTEGER NOT NULL DEFAULT 1;",
        )?;
    }
    for column in ["enable_live", "enable_movies", "enable_series"] {
        let exists = db
            .prepare("PRAGMA table_info(providers)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .iter()
            .any(|name| name == column);
        if !exists {
            db.execute_batch(&format!(
                "ALTER TABLE providers ADD COLUMN {column} INTEGER NOT NULL DEFAULT 1;"
            ))?;
        }
    }
    egress::init(db)?;
    crate::live_policy::init(db)?;
    pools::init(db)?;
    selection::init(db)?;
    crate::activity::init(db)?;
    crate::health::init(db)?;
    crate::guides::init(db)?;
    crate::lineup::init(db)
}
