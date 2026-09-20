//! Managed, capability-addressed HLS sessions. No upstream URL is returned to clients.
mod compat;
mod direct;
mod direct_gate;
mod hardware;
mod hls;
mod lease;
mod lifecycle;
mod manager;
mod probe;
mod start;
#[cfg(test)]
mod tests;
mod types;

pub use manager::PlaybackManager;
pub(crate) use manager::SampleLimits;
pub use types::{
    Capabilities, Config, MediaTrack, PlaybackAuthorization, PlaybackResponse, SelectedAudio,
    SelectedSubtitle, TrackDisposition, TrackSelection,
};

use compat::{
    conservative_frame_rate, normalize_audio_language, Probe, ProbeStream, H264_COPY_LEVEL,
};
use direct_gate::{direct_file_extension, direct_format, source_authorization};
#[cfg(test)]
use hls::hdr_size;
use hls::{
    cache_safe, dimensions, hdr_filter, media_type, playback_ready, scale_filter,
    stable_hls_target_duration, HLS_DELETE_GRACE_SECONDS, HLS_INITIAL_SEGMENT_SECONDS,
    HLS_SEGMENT_SECONDS, HLS_WINDOW_SECONDS,
};
use lease::{cleanup_orphans, constant_time_eq, header_block, input_args, InputPermits, Session};
#[cfg(test)]
use probe::{probe_failure, ProbeFailure, LIVE_PROBE_CACHE_TTL, PROBE_CACHE_TTL};
use probe::{probe_output, ProbeCacheEntry, ProbeChild};
use types::{CleanupTasks, ProbeDisposition};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    sync::{Mutex, OnceCell, OwnedSemaphorePermit, RwLock, Semaphore},
    time::{sleep, timeout},
};
use uuid::Uuid;
