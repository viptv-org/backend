//! The playback engine now lives in the `viptv-playback-engine` crate: an
//! identity-free HLS engine with no database or account dependencies. This
//! re-export keeps the crate-internal namespace (`playback::...`) identical
//! for every consumer; the capability message constant stays single-sourced
//! in the engine.
pub(crate) use viptv_playback_engine::MSG_PLAYBACK_CAPACITY;
pub use viptv_playback_engine::{
    Capabilities, Config, MediaTrack, PlaybackAuthorization, PlaybackManager, PlaybackResponse,
    SampleLimits, SelectedAudio, SelectedSubtitle, TrackDisposition, TrackSelection,
};
