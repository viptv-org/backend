//! Bounded Stremio manifest and media projections shared by native and server hosts.
mod manifest;
mod media;
mod streams;
pub use manifest::{
    Catalog, Manifest, catalog_url, manifest_url, meta_url, parse_manifest, search_urls,
};
pub use media::{cards, detail};
pub use streams::{PlayableSource, SourceCandidate, read_streams, stream_url};
