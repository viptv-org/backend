//! Bounded Stremio manifest and media projections shared by native and server hosts.
mod manifest;
mod browse;
pub use browse::{ExtraValues, CatalogQuery,catalog_controls,discovery_urls,catalog_page,catalog_pageable,discovery_plan};
mod media;
mod streams;
mod source_metadata;
pub use source_metadata::{source_audio_languages, source_detail, source_quality_height};
mod subtitle_text;
mod subtitle_encoding;
mod subtitles;
pub use manifest::{
    Catalog, Manifest, catalog_url, manifest_url, meta_url, parse_manifest, search_urls,
};
pub use media::{cards, detail, detail_page, episode_cards};
pub use streams::{PlayableSource, SourceCandidate, SourcePresentation, read_streams, stream_url};
pub use subtitle_text::subtitle_vtt;
pub use subtitle_encoding::subtitle_bytes_vtt;
pub use subtitles::{
    SubtitleCandidate, SubtitleExtras, SubtitleFile, SubtitleProvider, SubtitleSnapshot,
    read_subtitles, subtitle_url,
};
