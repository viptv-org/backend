//! Managed, capability-addressed HLS sessions. No upstream URL is returned to clients.
mod direct;
mod hardware;
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    /// Dedicated media directory: never share with another running server/manager.
    pub root: PathBuf,
    pub max_sessions: usize,
    pub ttl: Duration,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Capabilities {
    pub max_width: u32,
    pub max_height: u32,
    pub h264: bool,
    pub hevc: bool,
    pub aac: bool,
    /// Opt-in only: old clients retain managed HLS.
    pub direct_play: bool,
    /// Omitted by legacy clients; runtime probes may disable either transport.
    pub direct_mp4: Option<bool>,
    pub direct_hls: Option<bool>,
    /// A WebCodecs client reads original containers itself. When set, the
    /// original file may be served as a byte-range resource for any container
    /// this client can demux, with the codecs it declares below.
    pub direct_files: Option<bool>,
    pub direct_video_codecs: Option<Vec<String>>,
    pub direct_audio_codecs: Option<Vec<String>>,
    /// A native client (the Tauri engine) fetches sources itself. When set, the
    /// original URL is delivered with the server's upstream authorization
    /// instead of a proxy: no transcoding and no server-side media path.
    pub direct_urls: Option<bool>,
    pub hevc_sdr: bool,
}
impl Default for Capabilities {
    fn default() -> Self {
        Self {
            max_width: 1280,
            max_height: 720,
            h264: true,
            hevc: false,
            aac: true,
            direct_play: false,
            direct_mp4: None,
            direct_hls: None,
            direct_files: None,
            direct_video_codecs: None,
            direct_audio_codecs: None,
            direct_urls: None,
            hevc_sdr: false,
        }
    }
}
/// Absolute INPUT stream indices from ffprobe, never native/output audio indices.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackSelection {
    pub audio_track_index: Option<u32>,
    pub audio_language: Option<String>,
    pub subtitle_track_index: Option<u32>,
    pub preferred_audio_language: Option<String>,
    pub preferred_subtitle_language: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct TrackDisposition {
    pub default: bool,
    pub commentary: bool,
    pub hearing_impaired: bool,
    pub visual_impaired: bool,
    pub forced: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct ProbeDisposition {
    default: u8,
    comment: u8,
    hearing_impaired: u8,
    visual_impaired: u8,
    forced: u8,
}
impl ProbeDisposition {
    fn public(&self) -> TrackDisposition {
        TrackDisposition {
            default: self.default == 1,
            commentary: self.comment == 1,
            hearing_impaired: self.hearing_impaired == 1,
            visual_impaired: self.visual_impaired == 1,
            forced: self.forced == 1,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MediaTrack {
    pub input_index: u32,
    pub codec: Option<String>,
    pub language: Option<String>,
    pub language_status: String,
    pub title: String,
    /// None means these disposition flags were not provided by the probe.
    pub disposition: Option<TrackDisposition>,
    pub selected: bool,
    pub supported: bool,
    pub selectable: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SelectedAudio {
    pub input_index: u32,
    /// Compatibility alias for output_audio_ordinal; never a native Roku track ID.
    pub output_index: u32,
    /// Ordinal within output AUDIO tracks; native IDs come from the player itself.
    pub output_audio_ordinal: u32,
    pub output_stream_index: u32,
    pub language: Option<String>,
    pub language_status: String,
    pub title: String,
    /// None means these disposition flags were not provided by the probe.
    pub disposition: Option<TrackDisposition>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SelectedSubtitle {
    pub input_index: u32,
    /// Subtitle ordinal, never a native player track ID.
    pub output_index: u32,
    pub output_stream_index: u32,
    pub language: Option<String>,
    pub language_status: String,
    pub title: String,
    /// None means these disposition flags were not provided by the probe.
    pub disposition: Option<TrackDisposition>,
}

/// Upstream authorization for a delivered original URL. The client applies it
/// to its own engine's requests; browser deliveries never carry one.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlaybackAuthorization {
    pub cookie: Option<String>,
    pub user_agent: Option<String>,
    /// Every other upstream header the client's engine may need to fetch the
    /// source itself (Referer and friends); cookie and user agent are excluded
    /// because they ride in the dedicated fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlaybackResponse {
    pub id: String,
    pub url: String,
    pub format: String,
    pub mode: String,
    /// Whether the source video packets are copied or encoded.
    pub video_mode: String,
    /// Whether selected source audio is copied, encoded, or absent.
    pub audio_mode: String,
    pub position: f64,
    /// True only for explicitly indexed/discovered live sources.
    pub live: bool,
    /// Full source duration in seconds; zero means unknown or live.
    pub duration: f64,
    pub audio_tracks: Vec<MediaTrack>,
    pub subtitle_tracks: Vec<MediaTrack>,
    pub selected_audio: Option<SelectedAudio>,
    pub subtitles_supported: bool,
    pub selected_subtitle: Option<SelectedSubtitle>,
    /// Present only when the original URL is delivered for a client that
    /// fetches it itself: the headers that client must present upstream.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization: Option<PlaybackAuthorization>,
}

type CleanupTasks = Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>>;

const PROBE_STDOUT_LIMIT: usize = 1024 * 1024;
const PROBE_STDERR_LIMIT: usize = 64 * 1024;
// A one-second initial segment reduces player startup; steady-state stays at
// two seconds. Copied video uses split-by-time so variable source GOPs cannot
// change the advertised target duration during a session.
const HLS_INITIAL_SEGMENT_SECONDS: u32 = 1;
const HLS_SEGMENT_SECONDS: u32 = 2;
const HLS_WINDOW_SECONDS: u32 = 120;
const HLS_DELETE_GRACE_SECONDS: u32 = 60;
const PROBE_CACHE_TTL: Duration = Duration::from_secs(120);
/// Live sources keep their stream identity while content rolls, so a short
/// reuse window absorbs channel hopping without serving long-stale metadata.
const LIVE_PROBE_CACHE_TTL: Duration = Duration::from_secs(30);
const PROBE_CACHE_CAP: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProbeFailure {
    Spawn,
    OutputRead,
    Oversized,
    /// Metadata that exceeded the stdout budget, which a smaller entry set can fix.
    OversizedOutput,
    InvalidJson,
    Protocol,
    Http(u16),
    Network,
    Timeout,
    Exit,
}
impl ProbeFailure {
    fn retryable(self) -> bool {
        matches!(
            self,
            Self::Http(404 | 408 | 429 | 500..=599) | Self::Network | Self::Timeout
        )
    }
}

// Own the child across every await, including cancellation during kill/wait. Diagnostics
// never escape memory; only the closed failure enum (and recognized HTTP code) is logged.
struct ProbeChild {
    child: Option<Child>,
    permits: Option<Arc<InputPermits>>,
    cleanup_tasks: CleanupTasks,
}
impl ProbeChild {
    async fn reap(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
        self.child.take();
    }
}
impl Drop for ProbeChild {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let _ = child.start_kill();
        let permits = self.permits.take();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let task = runtime.spawn(async move {
                let _ = child.wait().await;
                drop(permits);
            });
            let mut tasks = self.cleanup_tasks.lock().unwrap_or_else(|e| e.into_inner());
            tasks.retain(|task| !task.is_finished());
            tasks.push(task);
        }
    }
}

async fn probe_output(
    reader: impl tokio::io::AsyncRead + Unpin,
    limit: usize,
) -> Result<Vec<u8>, ProbeFailure> {
    let mut bytes = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| ProbeFailure::OutputRead)?;
    if bytes.len() > limit {
        return Err(ProbeFailure::Oversized);
    }
    Ok(bytes)
}

fn probe_failure(stderr: &[u8]) -> ProbeFailure {
    let text = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    if [
        "not on whitelist",
        "protocol not found",
        "protocol not allowed",
        "operation not permitted",
    ]
    .iter()
    .any(|s| text.contains(s))
    {
        return ProbeFailure::Protocol;
    }
    let mut http = None;
    for prefix in [
        "http error ",
        "server returned ",
        "http/1.1 ",
        "http/1.0 ",
        "http/2 ",
    ] {
        for (_, tail) in text
            .match_indices(prefix)
            .map(|(i, p)| (i, &text[i + p.len()..]))
        {
            if let Some(code) = tail
                .get(..3)
                .filter(|s| {
                    s.bytes().all(|b| b.is_ascii_digit())
                        && tail
                            .as_bytes()
                            .get(3)
                            .is_none_or(|b| b.is_ascii_whitespace())
                })
                .and_then(|s| s.parse::<u16>().ok())
                .filter(|c| (400..=599).contains(c))
            {
                // A permanent status anywhere must not be masked by a later transient one.
                if !ProbeFailure::Http(code).retryable() {
                    return ProbeFailure::Http(code);
                }
                http = Some(code);
            }
        }
    }
    if let Some(code) = http {
        return ProbeFailure::Http(code);
    }
    if [
        "connection timed out",
        "connection reset",
        "operation timed out",
        "i/o timeout",
    ]
    .iter()
    .any(|s| text.contains(s))
    {
        return ProbeFailure::Network;
    }
    ProbeFailure::Exit
}

// Every process reading an input shares these reservations. Cancellation may
// detach cleanup, but admission stays closed until the final child is reaped.
struct InputPermits {
    _playback: OwnedSemaphorePermit,
    _provider: Option<OwnedSemaphorePermit>,
}

struct Session {
    // Keep inspected VOD facts alive while this input is in use.
    _source_probe: Option<Arc<Probe>>,
    direct: Option<Arc<direct::Direct>>,
    capability: String,
    dir: PathBuf,
    child: Option<Child>,
    touched: Instant,
    stable_target_duration: bool,
    supervised_live: bool,
    permits: Arc<InputPermits>,
    cleanup_tasks: CleanupTasks,
}
impl Session {
    async fn cleanup(mut self) {
        if let Some(direct) = &self.direct {
            direct
                .closed
                .store(true, std::sync::atomic::Ordering::Release);
        }
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
        self.child.take();
        if tokio::fs::remove_dir_all(&self.dir).await.is_ok() {
            self.dir = PathBuf::new();
        }
    }
}
// Also clean partially-started sessions when an HTTP request is cancelled.
impl Drop for Session {
    fn drop(&mut self) {
        if let Some(direct) = &self.direct {
            direct
                .closed
                .store(true, std::sync::atomic::Ordering::Release);
        }
        let mut child = self.child.take();
        if let Some(child) = child.as_mut() {
            let _ = child.start_kill();
        }
        if self.dir.as_os_str().is_empty() {
            return;
        }
        let dir = self.dir.clone();
        let permits = self.permits.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let task = runtime.spawn(async move {
                if let Some(mut child) = child {
                    let _ = child.wait().await;
                }
                let _ = tokio::fs::remove_dir_all(dir).await;
                drop(permits);
            });
            let mut tasks = self.cleanup_tasks.lock().unwrap_or_else(|e| e.into_inner());
            tasks.retain(|task| !task.is_finished());
            tasks.push(task);
        }
    }
}

struct ProbeCacheEntry {
    probe: Arc<Probe>,
    inserted: Instant,
    /// Live entries expire on the shorter live window; VOD metadata is stable.
    live: bool,
}
// Drop expired entries; anything a caller still holds stays for reuse.
fn prune_probe_cache(cache: &mut HashMap<[u8; 32], ProbeCacheEntry>, now: Instant) {
    cache.retain(|_, entry| {
        let ttl = if entry.live {
            LIVE_PROBE_CACHE_TTL
        } else {
            PROBE_CACHE_TTL
        };
        Arc::strong_count(&entry.probe) > 1 || now.duration_since(entry.inserted) < ttl
    });
}

pub struct PlaybackManager {
    config: Config,
    slots: Arc<Semaphore>,
    sessions: Mutex<HashMap<String, Session>>,
    probe_cache: Mutex<HashMap<[u8; 32], ProbeCacheEntry>>,
    lifecycle: RwLock<()>,
    initialized: OnceCell<()>,
    filters: OnceCell<HashSet<String>>,
    qsv_device: Option<PathBuf>,
    qsv_ready: OnceCell<bool>,
    cleanup_tasks: CleanupTasks,
}
pub(crate) struct SampleLimits {
    pub seconds: u64,
    pub startup_seconds: u64,
    pub budget_seconds: u64,
    pub max_bytes: usize,
}
impl PlaybackManager {
    pub fn new(config: Config) -> Arc<Self> {
        Self::new_with_qsv(config, None)
    }

    pub fn new_with_qsv(config: Config, qsv_device: Option<PathBuf>) -> Arc<Self> {
        let interval = config
            .ttl
            .min(Duration::from_secs(1))
            .max(Duration::from_millis(100));
        let manager = Arc::new(Self {
            slots: Arc::new(Semaphore::new(config.max_sessions)),
            config,
            sessions: Mutex::new(HashMap::new()),
            probe_cache: Mutex::new(HashMap::new()),
            lifecycle: RwLock::new(()),
            initialized: OnceCell::new(),
            filters: OnceCell::new(),
            qsv_device,
            qsv_ready: OnceCell::new(),
            cleanup_tasks: Arc::new(std::sync::Mutex::new(Vec::new())),
        });
        let weak = Arc::downgrade(&manager);
        tokio::spawn(async move {
            if let Some(manager) = weak.upgrade() {
                let _lifecycle = manager.lifecycle.read().await;
                let _ = manager.qsv_available().await;
                if manager.initialize().await.is_err() {
                    tracing::warn!("Playback startup cleanup could not finish");
                }
            }
            loop {
                sleep(interval).await;
                let Some(manager) = weak.upgrade() else { break };
                manager.reap().await;
            }
        });
        manager
    }

    async fn initialize(&self) -> Result<(), String> {
        self.initialized
            .get_or_try_init(|| cleanup_orphans(&self.config.root))
            .await
            .map(|_| ())
    }

    async fn qsv_available(&self) -> bool {
        *self
            .qsv_ready
            .get_or_init(|| async {
                let Some(device) = &self.qsv_device else {
                    return false;
                };
                let ready = hardware::usable(&self.config.ffmpeg, device).await;
                tracing::info!(ready, "Intel Quick Sync startup check");
                ready
            })
            .await
    }
    pub fn acceleration_status(&self) -> &'static str {
        if self.qsv_device.is_none() {
            "software"
        } else {
            match self.qsv_ready.get() {
                Some(true) => "qsv",
                Some(false) => "software_fallback",
                None => "checking",
            }
        }
    }

    pub async fn start(
        &self,
        url: String,
        headers: HashMap<String, String>,
        position: f64,
        capabilities: Option<Capabilities>,
        force: bool,
    ) -> Result<PlaybackResponse, String> {
        self.start_with_kind(url, headers, position, capabilities, force, false)
            .await
    }

    pub async fn start_with_kind(
        &self,
        url: String,
        headers: HashMap<String, String>,
        position: f64,
        capabilities: Option<Capabilities>,
        force: bool,
        live: bool,
    ) -> Result<PlaybackResponse, String> {
        self.start_with_permit(url, headers, position, capabilities, force, live, None)
            .await
    }

    /// The caller acquires provider capacity before entry. Keep it across probing,
    /// startup and the complete session lifetime; all error paths release it.
    #[allow(clippy::too_many_arguments)] // Preserve the public playback shim and explicit permit ownership.
    pub async fn start_with_permit(
        &self,
        url: String,
        headers: HashMap<String, String>,
        position: f64,
        capabilities: Option<Capabilities>,
        force: bool,
        live: bool,
        provider_permit: Option<OwnedSemaphorePermit>,
    ) -> Result<PlaybackResponse, String> {
        self.start_with_selection(
            url,
            headers,
            position,
            capabilities,
            force,
            live,
            provider_permit,
            TrackSelection {
                ..Default::default()
            },
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn start_with_selection(
        &self,
        url: String,
        headers: HashMap<String, String>,
        position: f64,
        capabilities: Option<Capabilities>,
        force: bool,
        live: bool,
        provider_permit: Option<OwnedSemaphorePermit>,
        selection: TrackSelection,
    ) -> Result<PlaybackResponse, String> {
        if selection
            .audio_track_index
            .is_some_and(|index| index > 65535)
        {
            return Err("Requested input audio track index is out of range".into());
        }
        if selection
            .subtitle_track_index
            .is_some_and(|index| index > 65535)
        {
            return Err("Requested input subtitle track index is out of range".into());
        }
        if live && position != 0.0 {
            return Err("Live playback does not support offset seeking".into());
        }
        let _lifecycle = self.lifecycle.read().await;
        if self.slots.is_closed() {
            return Err("Playback is shutting down".into());
        }
        // Never propagate parser/provider/subprocess errors: they may contain credentials.
        let validated =
            crate::util::validate_url(&url).map_err(|_| "Invalid playback URL".to_owned())?;
        if !matches!(validated.scheme(), "http" | "https")
            || !validated.username().is_empty()
            || validated.password().is_some()
            || validated.fragment().is_some()
        {
            return Err("Invalid playback URL".into());
        }
        if !position.is_finite() || !(0.0..=604800.0).contains(&position) {
            return Err("Invalid playback position".into());
        }
        let header_block = header_block(&headers)?;
        let caps = capabilities.unwrap_or_default();
        let (width, height) = dimensions(&caps)?;
        if !caps.h264 || !caps.aac {
            return Err("H264 and AAC playback support is required".into());
        }
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
            // Distinct from a provider-connection limit: this is the server's own
            // session budget (VIPTV_MAX_SESSIONS), which needs the viewer to stop
            // something rather than to retry. It is reported as 503, not 429.
            .map_err(|_| "Playback capacity reached".to_owned())?;
        let permits = Arc::new(InputPermits {
            _playback: permit,
            _provider: provider_permit,
        });
        self.initialize().await?;
        let probe_started = Instant::now();
        let probe = self
            .cached_probe(
                validated.as_str(),
                &header_block,
                live,
                Some(permits.clone()),
            )
            .await
            .ok_or_else(|| {
                tracing::warn!(live, "Playback source inspection failed");
                "Could not inspect source video safely; try another stream".to_owned()
            })?;
        let probe_ms = probe_started.elapsed().as_millis() as u64;
        // The capability envelope gates only the managed/transcode path. Original
        // delivery is decided below from the client's declared decoders, so an
        // HDR, wide-gamut or otherwise unusual source is still playable whenever
        // the client can demux and decode it.
        let selected_input = probe.select_audio(&selection)?;
        let caption_index = selection.subtitle_track_index.or_else(|| {
            let language = selection.preferred_subtitle_language.as_deref()?;
            probe
                .streams
                .iter()
                .filter(|s| s.codec_type.as_deref() == Some("subtitle") && s.text_subtitle())
                .filter(|s| {
                    s.language().is_some_and(|actual| {
                        normalize_audio_language(&actual) == normalize_audio_language(language)
                    })
                })
                .take(32)
                .find_map(|s| s.index.filter(|i| *i <= 65535))
        });
        let selected_caption = probe.select_subtitle(caption_index)?;
        let mut audio_tracks = probe.tracks("audio");
        let mut subtitle_tracks = probe.tracks("subtitle");
        for track in audio_tracks.iter_mut().chain(subtitle_tracks.iter_mut()) {
            if let Some(stream) = probe
                .streams
                .iter()
                .find(|s| s.index == Some(track.input_index))
            {
                if let Some(title) = stream
                    .tags
                    .iter()
                    .find(|(key, _)| key.eq_ignore_ascii_case("title"))
                    .map(|(_, value)| value)
                {
                    let title =
                        crate::source_display_text(title, 128, validated.as_str(), &headers);
                    if !title.is_empty() {
                        track.title = title;
                    }
                }
            }
            track.selected = selected_input.and_then(|s| s.index) == Some(track.input_index)
                || selected_caption.and_then(|s| s.index) == Some(track.input_index);
        }
        let selected_audio = selected_input.map(|audio| SelectedAudio {
            input_index: audio.index.expect("selection validates stream index"),
            output_index: 0,
            output_audio_ordinal: 0,
            output_stream_index: 1,
            language: audio.language(),
            language_status: audio.language_status().into(),
            disposition: audio.disposition.as_ref().map(ProbeDisposition::public),
            title: audio_tracks
                .iter()
                .find(|t| t.selected)
                .map(|t| t.title.clone())
                .unwrap_or_default(),
        });
        let selected_subtitle = selected_caption.map(|caption| SelectedSubtitle {
            input_index: caption.index.expect("selection validates stream index"),
            output_index: 0,
            output_stream_index: if selected_audio.is_some() { 2 } else { 1 },
            language: caption.language(),
            language_status: caption.language_status().into(),
            disposition: caption.disposition.as_ref().map(ProbeDisposition::public),
            title: subtitle_tracks
                .iter()
                .find(|t| t.selected)
                .map(|t| t.title.clone())
                .unwrap_or_default(),
        });
        if !live
            && probe
                .duration()
                .is_some_and(|duration| position >= duration)
        {
            return Err("Playback position is past the end of this source".into());
        }
        // A native client that fetches sources itself receives the original URL
        // with the server's upstream authorization. Its own decoders decide
        // playability: this server never proxies, transcodes, or applies codec
        // policy for such a client.
        if !force && caps.direct_urls == Some(true) {
            let format = if live {
                "hls"
            } else {
                probe
                    .format
                    .get("format_name")
                    .and_then(|name| name.as_str())
                    .and_then(direct_file_extension)
                    .unwrap_or("file")
            };
            let authorization = source_authorization(&headers);
            let id = Uuid::new_v4().to_string();
            let capability =
                format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
            let response = PlaybackResponse {
                id: id.clone(),
                url: validated.as_str().to_owned(),
                format: format.into(),
                mode: "direct".into(),
                video_mode: "copy".into(),
                audio_mode: "copy".into(),
                position,
                live,
                duration: if live {
                    0.0
                } else {
                    probe.duration().unwrap_or(0.0)
                },
                audio_tracks,
                subtitles_supported: subtitle_tracks.iter().any(|track| track.supported),
                subtitle_tracks,
                selected_audio,
                selected_subtitle,
                authorization,
            };
            self.sessions.lock().await.insert(
                id,
                Session {
                    _source_probe: (!live).then(|| probe.clone()),
                    direct: None,
                    capability,
                    dir: PathBuf::new(),
                    child: None,
                    touched: Instant::now(),
                    stable_target_duration: false,
                    supervised_live: false,
                    permits,
                    cleanup_tasks: self.cleanup_tasks.clone(),
                },
            );
            tracing::info!(
                mode = "direct",
                format,
                probe_ms,
                "Playback preparation completed"
            );
            return Ok(response);
        }

        // Original delivery is opt-in, uses inspected tracks, and keeps the
        // provider reservation owned by this session and in-flight reads.
        if !force && caps.direct_play && std::env::var("VIPTV_DIRECT_PLAY").as_deref() != Ok("0") {
            if let Some(format) = direct_format(&probe, &caps, selected_input, &selection) {
                if let Ok(transport) =
                    direct::Direct::prepare(validated.clone(), &headers, format, permits.clone())
                        .await
                {
                    let id = Uuid::new_v4().to_string();
                    let capability =
                        format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
                    let filename = if format == "hls" {
                        "index.m3u8"
                    } else {
                        // Original files are served under their own container
                        // name; the client demuxes them itself.
                        Box::leak(format!("source.{format}").into_boxed_str())
                    };
                    // A native file can only retain one default audio track here;
                    // arbitrary track changes deliberately return to managed HLS.
                    let response = PlaybackResponse {
                        id: id.clone(),
                        url: format!("/media/{id}/{capability}/{filename}"),
                        format: format.into(),
                        mode: "direct".into(),
                        video_mode: "copy".into(),
                        audio_mode: if selected_input.is_some() {
                            "copy"
                        } else {
                            "none"
                        }
                        .into(),
                        position,
                        live,
                        duration: if live {
                            0.0
                        } else {
                            probe.duration().unwrap_or(0.0)
                        },
                        audio_tracks,
                        subtitles_supported: subtitle_tracks.iter().any(|track| track.supported),
                        subtitle_tracks,
                        selected_audio,
                        selected_subtitle: None,
                        authorization: None,
                    };
                    self.sessions.lock().await.insert(
                        id,
                        Session {
                            _source_probe: (!live).then(|| probe.clone()),
                            direct: Some(transport),
                            capability,
                            dir: PathBuf::new(),
                            child: None,
                            touched: Instant::now(),
                            stable_target_duration: false,
                            supervised_live: false,
                            permits,
                            cleanup_tasks: self.cleanup_tasks.clone(),
                        },
                    );
                    tracing::info!(
                        mode = "direct",
                        format,
                        probe_ms,
                        "Playback preparation completed"
                    );
                    return Ok(response);
                }
                tracing::info!(
                    reason = "original_transport_unavailable",
                    "Using managed playback fallback"
                );
            }
        }
        // Managed output re-encodes into the envelope the browser declared, so the
        // inspected source must be inside it. Original delivery above is exempt:
        // there the client's own decoders, not this policy, decide.
        probe.ensure_supported()?;
        let hdr = probe.hdr_transfer()?.is_some();
        let interlaced = probe.interlaced();
        let mut transforms = Vec::new();
        if interlaced || hdr {
            let filters = self.available_filters().await?;
            if interlaced {
                let filter = if filters.contains("bwdif") {
                    "bwdif"
                } else if filters.contains("yadif") {
                    "yadif"
                } else {
                    return Err(
                        "Interlaced playback requires the bwdif or yadif FFmpeg filter".into(),
                    );
                };
                transforms.push(format!(
                    "{filter}=mode=send_frame:parity=auto:deint=all,setfield=prog"
                ));
            }
            if hdr {
                if !["zscale", "tonemap", "sidedata"]
                    .iter()
                    .all(|name| filters.contains(*name))
                {
                    return Err("HDR10/HLG conversion requires FFmpeg zscale, tonemap and sidedata filters; select an SDR source".into());
                }
                transforms.push(hdr_filter(width, height));
            }
        }
        if !hdr {
            transforms.push(scale_filter(width, height));
        }
        let duration = if live { None } else { probe.duration() };
        if duration.is_some_and(|duration| position >= duration) {
            return Err("Playback position is past the end of this source".into());
        }
        // Input-side -ss plus stream copy can start on an earlier keyframe or
        // discard reference frames. Only decoding provides accurate arbitrary seeks.
        // At offset zero, copy compatible H264 video even when audio alone needs AAC
        // conversion. This avoids wasting CPU re-encoding already-compatible pictures.
        let copy_video = !force
            && position == 0.0
            && probe.compatible_video(width, height, H264_COPY_LEVEL);
        // Copy audio independently at zero offset. After input-side seeking,
        // decoded audio is required to keep MPEGTS/WebVTT on the same clock.
        let copy_audio = !force && position == 0.0 && probe.compatible_audio_stream(selected_input);
        let remux = copy_video && copy_audio;
        let id = Uuid::new_v4().to_string();
        let capability = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        // An absolute output path keeps relative config roots independent of FFmpeg's cwd.
        tokio::fs::create_dir_all(&self.config.root)
            .await
            .map_err(|_| "Media storage unavailable".to_owned())?;
        let root = tokio::fs::canonicalize(&self.config.root)
            .await
            .map_err(|_| "Media storage unavailable".to_owned())?;
        let dir = root.join(&id);
        let mut session = Session {
            _source_probe: (!live).then(|| probe.clone()),
            direct: None,
            capability: capability.clone(),
            dir: dir.clone(),
            child: None,
            touched: Instant::now(),
            stable_target_duration: true,
            supervised_live: false,
            permits,
            cleanup_tasks: self.cleanup_tasks.clone(),
        };
        tokio::fs::create_dir(&dir)
            .await
            .map_err(|_| "Media storage unavailable".to_owned())?;
        let mut pipeline = hardware::plan(
            copy_video,
            !copy_video && self.qsv_available().await,
            probe.video()?,
            hdr,
            interlaced,
        );
        let engine_started = Instant::now();
        loop {
            let mut cmd = Command::new(&self.config.ffmpeg);
            cmd.kill_on_drop(true)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"]);
            if pipeline.accelerated() {
                hardware::device_args(
                    &mut cmd,
                    self.qsv_device.as_ref().expect("validated device"),
                );
                if pipeline == hardware::Pipeline::QsvDecode {
                    cmd.args(["-hwaccel", "qsv", "-hwaccel_output_format", "qsv"]);
                    // FFmpeg 5.x needs the explicit QSV decoder to keep frames on GPU.
                    let decoder = if probe.video()?.codec_name.as_deref() == Some("hevc") {
                        "hevc_qsv"
                    } else {
                        "h264_qsv"
                    };
                    cmd.args(["-c:v", decoder]);
                }
            }
            input_args(&mut cmd, &header_block);
            // Live inputs must stay realtime; VOD can fill the rolling window as fast
            // as FFmpeg can produce it so browser playback has actual headroom.
            if live {
                cmd.arg("-re");
            }
            if position > 0.0 {
                cmd.arg("-ss").arg(format!("{position:.3}"));
            }
            cmd.arg("-i").arg(validated.as_str());
            cmd.args(["-map", "0:v:0"]);
            if let Some(audio) = &selected_audio {
                cmd.arg("-map").arg(format!("0:{}", audio.input_index));
            }
            if let Some(subtitle) = &selected_subtitle {
                cmd.arg("-map").arg(format!("0:{}", subtitle.input_index));
                cmd.args(["-c:s", "webvtt", "-max_interleave_delta", "1000000"]);
            } else {
                cmd.arg("-sn");
            }
            cmd.args(["-dn", "-map_metadata", "-1", "-map_chapters", "-1"]);
            if let Some(audio) = &selected_audio {
                cmd.arg("-metadata:s:a:0")
                    .arg(if audio.language_status == "tagged_english" {
                        "language=eng"
                    } else {
                        "language=und"
                    });
            }
            if copy_video {
                cmd.args(["-c:v", "copy"]);
            } else {
                let gop = (HLS_SEGMENT_SECONDS * 30).to_string();
                let force_key_frames = format!(
                "expr:gte(t,if(eq(n_forced,0),0,{HLS_INITIAL_SEGMENT_SECONDS}+(n_forced-1)*{HLS_SEGMENT_SECONDS}))"
            );
                let filter = match pipeline {
                    hardware::Pipeline::QsvDecode => hardware::scale(probe.video()?, width, height),
                    hardware::Pipeline::QsvEncode => format!(
                        "{},format=nv12,hwupload=extra_hw_frames=64",
                        transforms.join(",")
                    ),
                    _ => transforms.join(","),
                };
                cmd.arg("-vf").arg(filter);
                if hdr {
                    cmd.args([
                        "-color_primaries",
                        "bt709",
                        "-color_trc",
                        "bt709",
                        "-colorspace",
                        "bt709",
                        "-color_range",
                        "tv",
                    ]);
                }
                if pipeline.accelerated() {
                    cmd.args([
                        "-c:v",
                        "h264_qsv",
                        "-preset",
                        "veryfast",
                        "-profile:v",
                        "main",
                        "-level:v",
                        "4.0",
                        "-b:v",
                        "4000k",
                        "-maxrate",
                        "5000k",
                        "-bufsize",
                        "10000k",
                        "-look_ahead",
                        "0",
                        "-async_depth",
                        "1",
                        "-bf",
                        "0",
                        "-fpsmax",
                        "30",
                        "-g",
                        &gop,
                        "-forced_idr",
                        "1",
                        "-force_key_frames",
                        &force_key_frames,
                    ]);
                } else {
                    cmd.args([
                        "-c:v",
                        "libx264",
                        "-preset",
                        "ultrafast",
                        "-tune",
                        "zerolatency",
                        "-profile:v",
                        "main",
                        "-level:v",
                        "4.0",
                        "-pix_fmt",
                        "yuv420p",
                        "-crf",
                        "23",
                        "-maxrate",
                        "5000k",
                        "-bufsize",
                        "10000k",
                        "-fpsmax",
                        "30",
                        "-g",
                        &gop,
                        "-keyint_min",
                        &gop,
                        "-sc_threshold",
                        "0",
                        "-flags",
                        "+cgop",
                        "-x264-params",
                        "open-gop=0",
                        "-forced-idr",
                        "1",
                        "-force_key_frames",
                        &force_key_frames,
                    ]);
                }
            }
            if selected_input.is_some() {
                if copy_audio {
                    cmd.args(["-c:a", "copy"]);
                } else {
                    cmd.args(["-c:a", "aac", "-b:a", "128k", "-ac", "2", "-ar", "48000"]);
                }
            }
            if let Some(subtitle) = &selected_subtitle {
                let language = subtitle.language.as_deref().unwrap_or("und");
                let audio_map = if selected_audio.is_some() { "a:0," } else { "" };
                // Keep AV and WebVTT on the same post-seek timestamp clock. Without
                // copyts, the nested MPEGTS muxer adds a private offset absent from VTT.
                cmd.args(["-hls_segment_options", "mpegts_copyts=1", "-var_stream_map"])
                    .arg(format!(
                        "v:0,{audio_map}s:0,sgroup:subs,language:{language}"
                    ));
            }
            let initial_segment_seconds = HLS_INITIAL_SEGMENT_SECONDS.to_string();
            let segment_seconds = HLS_SEGMENT_SECONDS.to_string();
            let list_size = (HLS_WINDOW_SECONDS / HLS_SEGMENT_SECONDS).to_string();
            let delete_threshold = (HLS_DELETE_GRACE_SECONDS / HLS_SEGMENT_SECONDS).to_string();
            cmd.args([
                "-max_muxing_queue_size",
                "1024",
                "-f",
                "hls",
                "-hls_init_time",
                &initial_segment_seconds,
            ]);
            let hls_flags = if copy_video {
                "delete_segments+temp_file+split_by_time"
            } else {
                "delete_segments+temp_file"
            };
            cmd.args([
                "-hls_time",
                &segment_seconds,
                "-hls_list_size",
                &list_size,
                "-hls_delete_threshold",
                &delete_threshold,
                "-hls_flags",
                hls_flags,
                "-hls_segment_filename",
            ]);
            cmd.arg(dir.join("segment-%09d.ts"))
                .arg(dir.join("index.m3u8"));
            session.child = Some(
                cmd.spawn()
                    .map_err(|_| "Playback engine unavailable".to_owned())?,
            );
            // Do not return a URL until both the playlist and first segment exist.
            let ready = timeout(Duration::from_secs(if pipeline.accelerated() { 12 } else { 30 }), async {
            loop {
                if !cache_safe(&dir, false).await {
                    return false;
                }
                if let Some(child) = session.child.as_mut() {
                    if let Ok(Some(status)) = child.try_wait() {
                        if !status.success() {
                            tracing::warn!(exit_code = ?status.code(), "Playback engine startup failed");
                            return false;
                        }
                        return playback_ready(&dir, selected_subtitle.as_ref()).await;
                    }
                }
                if playback_ready(&dir, selected_subtitle.as_ref()).await {
                    return true;
                }
                sleep(Duration::from_millis(150)).await;
            }
        })
        .await
        .unwrap_or(false);
            if ready {
                break;
            }
            if let Some(next) = pipeline.fallback() {
                if let Some(mut child) = session.child.take() {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                }
                // A failed attempt may have emitted a partial playlist. Never reuse it.
                tokio::fs::remove_dir_all(&dir)
                    .await
                    .map_err(|_| "Playback cleanup failed")?;
                tokio::fs::create_dir(&dir)
                    .await
                    .map_err(|_| "Playback storage unavailable")?;
                tracing::warn!(from = ?pipeline, to = ?next, "Retrying playback pipeline");
                pipeline = next;
                continue;
            }
            if !ready {
                session.cleanup().await;
                return Err(
                    "Playback could not start; try forced transcoding or another stream".into(),
                );
            }
        }
        let engine_ready_ms = engine_started.elapsed().as_millis() as u64;
        tracing::info!(
            probe_ms,
            engine_ready_ms,
            encoder = pipeline.encoder(),
            pipeline = ?pipeline,
            live,
            video_mode = if copy_video { "copy" } else { "encode" },
            audio_mode = if selected_input.is_none() {
                "none"
            } else if copy_audio {
                "copy"
            } else {
                "encode"
            },
            "Playback preparation completed"
        );
        session.touched = Instant::now();
        self.sessions.lock().await.insert(id.clone(), session);
        Ok(PlaybackResponse {
            url: format!(
                "/media/{id}/{capability}/{}",
                if selected_subtitle.is_some() {
                    "master.m3u8"
                } else {
                    "index.m3u8"
                }
            ),
            id,
            format: "hls".into(),
            mode: if remux { "remux" } else { "transcode" }.into(),
            video_mode: if copy_video { "copy" } else { "encode" }.into(),
            audio_mode: if selected_input.is_none() {
                "none"
            } else if copy_audio {
                "copy"
            } else {
                "encode"
            }
            .into(),
            position,
            live,
            duration: duration.unwrap_or(0.0),
            audio_tracks,
            subtitles_supported: subtitle_tracks.iter().any(|track| track.supported),
            subtitle_tracks,
            selected_audio,
            selected_subtitle,
            authorization: None,
        })
    }

    pub async fn heartbeat(&self, id: &str) -> bool {
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get_mut(id) {
            if session.touched.elapsed() < self.config.ttl {
                session.touched = Instant::now();
                return true;
            }
        }
        false
    }
    /// Call after draining HTTP requests, before shutting down the Tokio runtime.
    /// Dropping the manager kills children but only schedules best-effort deletion.
    pub async fn shutdown(&self) {
        self.slots.close();
        let _lifecycle = self.lifecycle.write().await;
        if self.initialize().await.is_err() {
            tracing::warn!("Playback startup cleanup could not finish");
        }
        let sessions = std::mem::take(&mut *self.sessions.lock().await);
        for (_, session) in sessions {
            session.cleanup().await;
        }
        let tasks =
            std::mem::take(&mut *self.cleanup_tasks.lock().unwrap_or_else(|e| e.into_inner()));
        for task in tasks {
            let _ = task.await;
        }
    }
    pub async fn stop(&self, id: &str) -> bool {
        let _lifecycle = self.lifecycle.read().await;
        let session = self.sessions.lock().await.remove(id);
        if let Some(session) = session {
            session.cleanup().await;
            true
        } else {
            false
        }
    }
    pub fn session_ttl(&self) -> Duration {
        self.config.ttl
    }
    pub fn is_shutting_down(&self) -> bool {
        self.slots.is_closed()
    }
    /// Includes successful EOF: a live input ending needs recovery even when
    /// its final cached playlist is still readable.
    pub async fn input_running(&self, id: &str) -> bool {
        let mut sessions = self.sessions.lock().await;
        let Some(session) = sessions.get_mut(id) else {
            return false;
        };
        if let Some(direct) = &session.direct {
            return !direct.closed.load(std::sync::atomic::Ordering::Acquire)
                && !direct.failed.load(std::sync::atomic::Ordering::Acquire);
        }
        matches!(
            session.child.as_mut().map(|child| child.try_wait()),
            Some(Ok(None))
        )
    }
    /// Hand progress policy to the owned live supervisor; quota/TTL watchdogs remain active.
    pub(crate) async fn supervise_live(&self, id: &str) {
        if let Some(session) = self.sessions.lock().await.get_mut(id) {
            session.supervised_live = true;
        }
    }
    /// Completed media sequence, independent of client playback position or picture content.
    pub(crate) async fn live_progress(&self, id: &str) -> Option<String> {
        let (dir, direct) = {
            let sessions = self.sessions.lock().await;
            let session = sessions.get(id)?;
            (session.dir.clone(), session.direct.clone())
        };
        if let Some(direct) = direct {
            return direct.progress().await;
        }
        let bytes = tokio::fs::read(dir.join("index.m3u8")).await.ok()?;
        if bytes.len() > 128 * 1024 {
            return None;
        }
        let playlist = String::from_utf8(bytes).ok()?;
        Some(
            playlist
                .lines()
                .filter(|line| {
                    line.starts_with("#EXT-X-MEDIA-SEQUENCE:")
                        || (!line.is_empty() && !line.starts_with('#'))
                })
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }
    /// A late on-demand viewer can reuse this origin only while its initial
    /// segments remain present in the bounded rolling playlist.
    pub(crate) async fn timeline_origin_available(&self, id: &str) -> bool {
        let dir = match self.sessions.lock().await.get(id) {
            Some(s) => {
                if s.direct.is_some() {
                    return true;
                }
                s.dir.clone()
            }
            None => return false,
        };
        let Ok(bytes) = tokio::fs::read(dir.join("index.m3u8")).await else {
            return false;
        };
        bytes.len() <= 128 * 1024
            && String::from_utf8_lossy(&bytes)
                .lines()
                .any(|line| line == "#EXT-X-MEDIA-SEQUENCE:0")
    }
    pub async fn active_count(&self) -> usize {
        self.sessions
            .lock()
            .await
            .values()
            .filter(|s| s.touched.elapsed() < self.config.ttl)
            .count()
    }
    /// Snapshot live session identifiers for pruning authorization leases.
    /// No media capabilities or upstream URLs are exposed.
    pub async fn active_ids(&self) -> Vec<String> {
        self.sessions
            .lock()
            .await
            .iter()
            .filter(|(_, session)| session.touched.elapsed() < self.config.ttl)
            .map(|(id, _)| id.clone())
            .collect()
    }
    /// A timed-out preparation may leave asynchronous kill/wait cleanup. The
    /// caller must bound this wait; capacity itself stays held until reaping.
    pub async fn settle_cancelled_inputs(&self) {
        loop {
            if self
                .cleanup_tasks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .all(|task| task.is_finished())
            {
                return;
            }
            sleep(Duration::from_millis(10)).await;
        }
    }
    /// Decode a bounded sample through ffprobe's frame decoder. The upstream
    /// response is piped once and capped independently of subprocess output.
    pub(crate) async fn sample_media(
        &self,
        url: String,
        provider: OwnedSemaphorePermit,
        proxy: Option<String>,
        limits: SampleLimits,
    ) -> Result<serde_json::Value, String> {
        use tokio::io::AsyncWriteExt;
        let seconds = limits.seconds;
        let slot = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| "deferred_capacity")?;
        let permits = Arc::new(InputPermits {
            _playback: slot,
            _provider: Some(provider),
        });
        let client = crate::provider::egress::builder(
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(limits.budget_seconds))
                .redirect(reqwest::redirect::Policy::limited(3)),
            proxy.as_deref(),
        )?
        .build()
        .map_err(|_| "request_failed")?;
        let start = Instant::now();
        let mut response = timeout(
            Duration::from_secs(limits.startup_seconds),
            client.get(url).send(),
        )
        .await
        .map_err(|_| "startup_timeout")?
        .map_err(|_| "network_failed")?;
        match response.status().as_u16() {
            200..=299 => {}
            401 | 403 => return Err("authentication_failed".into()),
            429 => return Err("rate_limited".into()),
            _ => return Err("network_failed".into()),
        }
        let mut child=Command::new(&self.config.ffprobe).args(["-v","error","-protocol_whitelist","pipe","-analyzeduration","5000000","-probesize","5000000","-read_intervals",&format!("%+{seconds}"),"-show_frames","-show_streams","-show_entries","frame=media_type,best_effort_timestamp_time:stream=codec_type,codec_name,width,height,channels","-of","json","-i","pipe:0"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true).spawn().map_err(|_|"decoder_unavailable")?;
        let mut input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let error = child.stderr.take().unwrap();
        let mut guard = ProbeChild {
            child: Some(child),
            permits: Some(permits),
            cleanup_tasks: self.cleanup_tasks.clone(),
        };
        let feeding = async {
            let mut bytes = 0usize;
            let mut first = None;
            loop {
                let chunk = if first.is_none() {
                    timeout(
                        Duration::from_secs(limits.startup_seconds).saturating_sub(start.elapsed()),
                        response.chunk(),
                    )
                    .await
                    .map_err(|_| "startup_timeout")?
                    .map_err(|_| "network_failed")?
                } else {
                    response.chunk().await.map_err(|_| "network_failed")?
                };
                let Some(chunk) = chunk else {
                    break;
                };
                if first.is_none() {
                    first = Some(start.elapsed().as_millis() as u64);
                }
                bytes = bytes.saturating_add(chunk.len());
                if bytes > limits.max_bytes {
                    return Err("sample_byte_limit");
                }
                if input.write_all(&chunk).await.is_err() {
                    break;
                }
            }
            drop(input);
            Ok::<_, &str>((bytes, first.unwrap_or(0)))
        };
        let sample=timeout(Duration::from_secs(limits.budget_seconds).saturating_sub(start.elapsed()),async {
            let (feeding,out,err,status)=tokio::join!(feeding,probe_output(output,2*1024*1024),probe_output(error,64*1024),guard.child.as_mut().unwrap().wait());
            let (bytes,startup)=feeding.map_err(str::to_owned)?;
            let out=out.map_err(|_|"invalid_media")?;let _=err;
            if !status.is_ok_and(|s|s.success()){return Err("invalid_media".to_owned());}
            let data:serde_json::Value=serde_json::from_slice(&out).map_err(|_|"invalid_media")?;
            let frames=data["frames"].as_array().ok_or("invalid_media")?;
            let timestamps=frames.iter().filter(|f|f["media_type"]=="video").filter_map(|f|f["best_effort_timestamp_time"].as_str()?.parse::<f64>().ok()).filter(|v|v.is_finite()).collect::<Vec<_>>();
            let advancing=timestamps.windows(2).filter(|v|v[1]>v[0]).count();
            let span=timestamps.last().zip(timestamps.first()).map(|(last,first)|last-first).unwrap_or(0.0);
            if advancing<8||span<(seconds as f64*0.7){return Err("media_not_advancing".to_owned());}
            let streams=data["streams"].as_array().ok_or("invalid_media")?;
            let video=streams.iter().find(|s|s["codec_type"]=="video").ok_or("invalid_media")?;
            let audio=streams.iter().find(|s|s["codec_type"]=="audio");
            Ok(serde_json::json!({"state":if audio.is_some(){"healthy"}else{"degraded"},"reason":if audio.is_some(){"decoded_advancing_media"}else{"audio_absent"},"startup_ms":startup,"sample_seconds":span,"sample_bytes":bytes,"video_codec":video["codec_name"],"width":video["width"],"height":video["height"],"audio_codec":audio.map(|s|s["codec_name"].clone()),"audio_channels":audio.map(|s|s["channels"].clone())}))
        }).await.unwrap_or_else(|_|Err("sample_timeout".into()));
        guard.reap().await;
        sample
    }
    pub async fn ffmpeg_available(&self) -> bool {
        let mut cmd = Command::new(&self.config.ffmpeg);
        cmd.arg("-version")
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        matches!(timeout(Duration::from_secs(3), cmd.status()).await, Ok(Ok(status)) if status.success())
    }
    pub(crate) async fn serve_original(
        &self,
        id: &str,
        capability: &str,
        file: &str,
        method: axum::http::Method,
        headers: axum::http::HeaderMap,
    ) -> Option<Result<axum::response::Response, String>> {
        let direct = {
            let mut sessions = self.sessions.lock().await;
            let session = sessions.get_mut(id)?;
            if session.capability != capability || session.touched.elapsed() >= self.config.ttl {
                return Some(Err("Media expired".into()));
            }
            let direct = session.direct.clone()?;
            session.touched = Instant::now();
            direct
        };
        Some(direct.serve(file, method, headers).await)
    }

    pub async fn serve(
        &self,
        id: &str,
        capability: &str,
        file: &str,
    ) -> Result<(String, Vec<u8>), String> {
        let mime = media_type(file).ok_or_else(|| "Media not found".to_owned())?;
        let (path, stable_target_duration) = {
            let mut sessions = self.sessions.lock().await;
            let session = sessions
                .get_mut(id)
                .filter(|s| {
                    s.touched.elapsed() < self.config.ttl
                        && constant_time_eq(s.capability.as_bytes(), capability.as_bytes())
                })
                .ok_or_else(|| "Media not found".to_owned())?;
            session.touched = Instant::now();
            (session.dir.join(file), session.stable_target_duration)
        };
        let metadata = tokio::fs::symlink_metadata(&path)
            .await
            .map_err(|_| "Media not found".to_owned())?;
        if !metadata.is_file() || metadata.len() > 32 * 1024 * 1024 {
            return Err("Media not found".into());
        }
        let mut bytes = tokio::fs::read(path)
            .await
            .map_err(|_| "Media not found".to_owned())?;
        if mime == "application/vnd.apple.mpegurl" && stable_target_duration {
            bytes = stable_hls_target_duration(bytes);
        }
        if mime == "text/vtt" && bytes.starts_with(b"WEBVTT\n") {
            // Caption-enabled MPEGTS uses copyts, so both renditions share clock0.
            let mut mapped = b"WEBVTT\nX-TIMESTAMP-MAP=LOCAL:00:00:00.000,MPEGTS:0\n\n".to_vec();
            mapped.extend_from_slice(&bytes[7..]);
            return Ok((mime.into(), mapped));
        }
        Ok((mime.into(), bytes))
    }
    async fn reap(&self) {
        let _lifecycle = self.lifecycle.read().await;
        let expired = {
            let mut sessions = self.sessions.lock().await;
            let mut ids = Vec::new();
            for (id, session) in sessions.iter_mut() {
                let (running, failed) = match session.child.as_mut().map(Child::try_wait) {
                    Some(Ok(Some(status))) => {
                        if !status.success() {
                            // Never log raw stderr, arguments, URLs, or provider headers.
                            tracing::warn!(exit_code = ?status.code(), "Playback engine failed");
                        }
                        (false, !status.success())
                    }
                    Some(Ok(None)) => (true, false),
                    Some(Err(_)) => (false, true),
                    None => (false, false),
                };
                if session.touched.elapsed() >= self.config.ttl
                    || failed
                    || (session.direct.is_none()
                        && !cache_safe(&session.dir, running && !session.supervised_live).await)
                {
                    ids.push(id.clone());
                }
            }
            ids.into_iter()
                .filter_map(|id| sessions.remove(&id))
                .collect::<Vec<_>>()
        };
        for session in expired {
            session.cleanup().await;
        }
    }
    async fn available_filters(&self) -> Result<&HashSet<String>, String> {
        self.filters
            .get_or_try_init(|| async {
                let mut child = Command::new(&self.config.ffmpeg)
                    .args(["-hide_banner", "-filters"])
                    .kill_on_drop(true)
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
                    .map_err(|_| "Playback filter inspection unavailable".to_owned())?;
                let stdout = child.stdout.take().unwrap();
                let mut bytes = Vec::new();
                let result = timeout(Duration::from_secs(3), async {
                    stdout
                        .take(1024 * 1024 + 1)
                        .read_to_end(&mut bytes)
                        .await
                        .ok()?;
                    if bytes.len() > 1024 * 1024 {
                        return None;
                    }
                    child.wait().await.ok().filter(|s| s.success())
                })
                .await;
                if !matches!(result, Ok(Some(_))) {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    return Err("Playback filter inspection unavailable".to_owned());
                }
                Ok(String::from_utf8_lossy(&bytes)
                    .lines()
                    .filter_map(|line| {
                        let parts: Vec<_> = line.split_whitespace().collect();
                        (parts.len() >= 3 && parts[2].contains("->")).then(|| parts[1].to_owned())
                    })
                    .collect())
            })
            .await
    }
    /// One inspection per source identity. Live and VOD entries share this
    /// bounded cache, told apart by the live discriminator in the digest, so a
    /// channel hop inside the short live TTL no longer pays a fresh ffprobe
    /// (with retries, up to ~10s) before playback can start, while VOD keeps
    /// the longer window its stable metadata allows.
    async fn cached_probe(
        &self,
        url: &str,
        headers: &str,
        live: bool,
        permits: Option<Arc<InputPermits>>,
    ) -> Option<Arc<Probe>> {
        let mut digest = Sha256::new();
        digest.update(url.as_bytes());
        digest.update([0]);
        digest.update(headers.as_bytes());
        digest.update([live as u8]);
        let key: [u8; 32] = digest.finalize().into();
        let now = Instant::now();
        {
            let mut cache = self.probe_cache.lock().await;
            prune_probe_cache(&mut cache, now);
            if let Some(entry) = cache.get(&key) {
                tracing::debug!("Reused bounded source probe metadata");
                return Some(entry.probe.clone());
            }
        }
        let probe = Arc::new(self.probe(url, headers, permits).await?);
        let mut cache = self.probe_cache.lock().await;
        prune_probe_cache(&mut cache, now);
        if cache.len() >= PROBE_CACHE_CAP {
            if let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, entry)| (Arc::strong_count(&entry.probe) > 1, entry.inserted))
                .map(|(key, _)| *key)
            {
                cache.remove(&oldest);
            }
        }
        cache.insert(
            key,
            ProbeCacheEntry {
                probe: probe.clone(),
                inserted: Instant::now(),
                live,
            },
        );
        Some(probe)
    }

    async fn probe(
        &self,
        url: &str,
        headers: &str,
        permits: Option<Arc<InputPermits>>,
    ) -> Option<Probe> {
        let mut unparsable = false;
        for attempt in 0..2 {
            let attempt_started = Instant::now();
            match self
                .probe_attempt(url, headers, permits.clone(), false)
                .await
            {
                Ok(probe) => return Some(probe),
                Err(category) => {
                    tracing::warn!(
                        ?category,
                        attempt,
                        probe_ms = attempt_started.elapsed().as_millis() as u64,
                        "Source probe failed"
                    );
                    unparsable |= matches!(
                        category,
                        ProbeFailure::InvalidJson | ProbeFailure::OversizedOutput
                    );
                    if attempt == 1 || !category.retryable() {
                        break;
                    }
                    // Caller-owned playback/provider permits remain held during this delay.
                    sleep(Duration::from_millis(1500)).await;
                }
            }
        }
        if !unparsable {
            return None;
        }
        // Unreadable metadata is usually a noisy or oversized response rather
        // than a dead source. Ask again for the smallest sufficient field set
        // instead of refusing something the server can still deliver.
        match self.probe_attempt(url, headers, permits, true).await {
            Ok(probe) => {
                tracing::info!("Reduced source probe succeeded");
                Some(probe)
            }
            Err(category) => {
                tracing::warn!(?category, "Reduced source probe failed");
                None
            }
        }
    }

    async fn probe_attempt(
        &self,
        url: &str,
        headers: &str,
        permits: Option<Arc<InputPermits>>,
        reduced: bool,
    ) -> Result<Probe, ProbeFailure> {
        let mut cmd = Command::new(&self.config.ffprobe);
        cmd.kill_on_drop(true)
            .stdin(Stdio::null())
            .stderr(Stdio::piped());
        cmd.args(["-v", "error"]);
        input_args(&mut cmd, headers);
        // The reduced form keeps every field the delivery decision needs, including
        // SDR/HDR transfer evidence, while shrinking a response that failed to parse.
        let entries = if reduced {
            "format=duration,format_name:stream=index,codec_type,codec_name,width,height,pix_fmt,channels,avg_frame_rate,color_transfer"
        } else {
            "format=duration,format_name:stream=index,codec_type,codec_name,width,height,pix_fmt,sample_aspect_ratio,profile,level,channels,avg_frame_rate,r_frame_rate,color_transfer,field_order:stream_tags=language,title:stream_disposition=default,comment,hearing_impaired,visual_impaired,forced"
        };
        cmd.args([
            "-analyzeduration",
            "5000000",
            "-probesize",
            "5000000",
            "-show_entries",
            entries,
            "-of",
            "json",
            "-i",
            url,
        ]);
        cmd.stdout(Stdio::piped());
        let child = cmd.spawn().map_err(|_| ProbeFailure::Spawn)?;
        let mut owned = ProbeChild {
            child: Some(child),
            permits,
            cleanup_tasks: self.cleanup_tasks.clone(),
        };
        let child = owned.child.as_mut().ok_or(ProbeFailure::Spawn)?;
        let stdout = child.stdout.take().ok_or(ProbeFailure::OutputRead)?;
        let stderr = child.stderr.take().ok_or(ProbeFailure::OutputRead)?;
        let result = timeout(Duration::from_secs(10), async {
            // Read both pipes concurrently. A limit violation short-circuits all readers
            // and kills the child rather than draining attacker-controlled output forever.
            tokio::try_join!(
                async {
                    probe_output(stdout, PROBE_STDOUT_LIMIT).await.map_err(
                        |failure| match failure {
                            ProbeFailure::Oversized => ProbeFailure::OversizedOutput,
                            other => other,
                        },
                    )
                },
                probe_output(stderr, PROBE_STDERR_LIMIT),
                async { child.wait().await.map_err(|_| ProbeFailure::Exit) },
            )
        })
        .await;
        owned.reap().await;
        let (bytes, stderr, status) = result.map_err(|_| ProbeFailure::Timeout)??;
        if !status.success() {
            // Failed ffprobe commonly emits an empty JSON object (or no stdout).
            // Malformed output is not evidence of a transient upstream input failure.
            if bytes.iter().any(|b| !b.is_ascii_whitespace()) {
                serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(&bytes)
                    .map_err(|_| ProbeFailure::InvalidJson)?;
            }
            return Err(probe_failure(&stderr));
        }
        serde_json::from_slice(&bytes).map_err(|_| ProbeFailure::InvalidJson)
    }
}

// Run exactly once before admitting a session. Dedicated single-owner root means
// pre-existing generated directories are crash leftovers, even if recently written.
async fn cleanup_orphans(root: &std::path::Path) -> Result<(), String> {
    let mut entries = match tokio::fs::read_dir(root).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("Media storage unavailable".into()),
    };
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|_| "Media storage unavailable".to_owned())?
    {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(id) = Uuid::parse_str(name) else {
            continue;
        };
        if id.get_version_num() != 4 || id.to_string() != name {
            continue;
        }
        if !entry
            .file_type()
            .await
            .map_err(|_| "Media storage unavailable".to_owned())?
            .is_dir()
        {
            continue;
        }
        let mut files = tokio::fs::read_dir(entry.path())
            .await
            .map_err(|_| "Media storage unavailable".to_owned())?;
        let mut generated_only = true;
        while let Some(file) = files
            .next_entry()
            .await
            .map_err(|_| "Media storage unavailable".to_owned())?
        {
            let filename = file.file_name();
            let known = filename.to_str().is_some_and(|name| {
                media_type(name.strip_suffix(".tmp").unwrap_or(name)).is_some()
            });
            if !known
                || !file
                    .file_type()
                    .await
                    .map_err(|_| "Media storage unavailable".to_owned())?
                    .is_file()
            {
                generated_only = false;
                break;
            }
        }
        if generated_only {
            tokio::fs::remove_dir_all(entry.path())
                .await
                .map_err(|_| "Media storage cleanup failed".to_owned())?;
        }
    }
    Ok(())
}

fn input_args(cmd: &mut Command, headers: &str) {
    // Restrict nested playlists/redirects to network protocols, never local files or devices.
    cmd.args([
        "-protocol_whitelist",
        "http,https,httpproxy,tcp,tls,crypto",
        "-rw_timeout",
        "10000000",
        // Recover premature HTTP bodies at their byte offset, with short bounded backoff.
        // Deliberately omit reconnect_at_eof and reconnect_on_http_error: normal VOD
        // completion and authentication failures must not restart or retry forever.
        "-reconnect",
        "1",
        "-reconnect_streamed",
        "1",
        "-reconnect_delay_max",
        "2",
    ]);
    // This server-authored transport field is consumed here, never sent upstream.
    let mut public = String::new();
    for line in headers.split("\r\n").filter(|line| !line.is_empty()) {
        if let Some(proxy) = line.strip_prefix("x-viptv-egress-proxy: ") {
            cmd.arg("-http_proxy").arg(proxy);
        } else {
            public.push_str(line);
            public.push_str("\r\n");
        }
    }
    if !public.is_empty() {
        cmd.arg("-headers").arg(public);
    }
}
fn header_block(headers: &HashMap<String, String>) -> Result<String, String> {
    let mut pairs: Vec<_> = headers.iter().collect();
    pairs.sort_by(|a, b| a.0.cmp(b.0));
    let mut out = String::new();
    if pairs.len() > 32 {
        return Err("Invalid playback headers".into());
    }
    for (key, value) in pairs {
        if key.is_empty()
            || key.len() > 128
            || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || value.len() > 4096
            || value.bytes().any(|b| b < 32 || b == 127)
        {
            return Err("Invalid playback headers".into());
        }
        out.push_str(key);
        out.push_str(": ");
        out.push_str(value);
        out.push_str("\r\n");
    }
    if out.len() > 16384 {
        return Err("Invalid playback headers".into());
    }
    Ok(out)
}
/// Formats a WebCodecs client can demux itself, mapped to their file extension.
fn direct_file_extension(format: &str) -> Option<&'static str> {
    // Exactly the WebCodecs demuxer's (mediabunny's) format set: ISOBMFF/QTFF,
    // Matroska/WebM, MPEG-TS, Ogg, ADTS, FLAC, WAVE and MP3. FLV, AVI, ASF and
    // plain MPEG are deliberately absent: this client cannot demux them, so
    // serving those files wholesale fails late with a confusing
    // unsupported-format error after several fallback restarts. A missing entry
    // is not a decode failure, only a lost direct path to managed delivery.
    for (name, extension) in [
        ("mp4", "mp4"),
        ("mov", "mov"),
        ("m4v", "m4v"),
        ("matroska", "mkv"),
        ("webm", "webm"),
        ("mpegts", "ts"),
        ("ogg", "ogg"),
        ("adts", "aac"),
        ("flac", "flac"),
        ("wav", "wav"),
        ("mp3", "mp3"),
    ] {
        if format.split(',').any(|f| f == name) {
            return Some(extension);
        }
    }
    None
}

/// ffprobe's codec name against the codec identifiers a WebCodecs client reports.
fn declared_codec(declared: &[String], codec: Option<&str>) -> bool {
    let Some(codec) = codec else {
        return false;
    };
    declared.iter().any(|name| {
        let normalized = name.trim().to_ascii_lowercase();
        let normalized = normalized.as_str();
        match normalized {
            "avc" | "h264" => codec == "h264",
            "hevc" | "h265" => codec == "hevc",
            "aac" | "mp4a" => codec == "aac",
            "dts" => codec == "dts",
            other => codec == other || codec.starts_with(other),
        }
    })
}

/// The original file may be served when the client reads the container and every
/// codec in it, and the picture is one this path can hand over unmodified.
/// The original-container path for a WebCodecs client. This client demuxes the
/// file itself and its decoders handle HDR and wide-gamut sources, so unlike the
/// native envelope it is not restricted by transfer characteristics or primaries.
/// Interlacing is still excluded because the demuxer feeds a progressive decoder.
fn direct_file_format(
    probe: &Probe,
    caps: &Capabilities,
    audio: Option<&ProbeStream>,
) -> Option<&'static str> {
    if caps.direct_files != Some(true)
        || probe
            .streams
            .iter()
            .filter(|s| s.codec_type.as_deref() == Some("video"))
            .count()
            != 1
        || probe.interlaced()
    {
        return None;
    }
    let video = probe.video().ok()?;
    let extension = direct_file_extension(probe.format.get("format_name")?.as_str()?)?;
    let width = caps.max_width.min(3840);
    let height = caps.max_height.min(2160);
    let fits =
        |value: Option<u32>, limit: u32| value.is_some_and(|v| v >= 2 && v <= limit && v % 2 == 0);
    if !fits(video.width, width) || !fits(video.height, height) {
        return None;
    }
    let video_codecs = caps.direct_video_codecs.as_deref().unwrap_or_default();
    if !declared_codec(video_codecs, video.codec_name.as_deref()) {
        return None;
    }
    let audio_codecs = caps.direct_audio_codecs.as_deref().unwrap_or_default();
    if audio.is_some_and(|stream| !declared_codec(audio_codecs, stream.codec_name.as_deref())) {
        return None;
    }
    Some(extension)
}

/// The upstream headers a direct-url client must present to fetch the source
/// itself: the provider's cookie and user agent, if the server holds any.
fn source_authorization(headers: &HashMap<String, String>) -> Option<PlaybackAuthorization> {
    let header = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    };
    // Every upstream header the engine may need (Referer and friends) rides
    // along; cookie and user agent keep their dedicated fields.
    let rest: HashMap<String, String> = headers
        .iter()
        .filter(|(key, _)| {
            !key.eq_ignore_ascii_case("Cookie") && !key.eq_ignore_ascii_case("User-Agent")
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let authorization = PlaybackAuthorization {
        cookie: header("Cookie"),
        user_agent: header("User-Agent"),
        headers: (!rest.is_empty()).then_some(rest),
    };
    (authorization.cookie.is_some()
        || authorization.user_agent.is_some()
        || authorization.headers.is_some())
    .then_some(authorization)
}

fn direct_format(
    probe: &Probe,
    caps: &Capabilities,
    audio: Option<&ProbeStream>,
    selection: &TrackSelection,
) -> Option<&'static str> {
    // Chosen subtitle tracks are delivered by a managed session, never by handing
    // the untouched file to the client.
    if selection.subtitle_track_index.is_some() || selection.preferred_subtitle_language.is_some() {
        return None;
    }
    // A WebCodecs client reads the original container itself, so it is not bound
    // by the native envelope's single-audio-stream or AAC-only rules.
    if let Some(extension) = direct_file_format(probe, caps, audio) {
        return Some(extension);
    }
    if probe
        .streams
        .iter()
        .filter(|s| s.codec_type.as_deref() == Some("audio"))
        .count()
        > 1
        || probe
            .streams
            .iter()
            .filter(|s| s.codec_type.as_deref() == Some("video"))
            .count()
            != 1
        || !probe.compatible_audio_stream(audio)
    {
        return None;
    }
    if let (Some(audio), Some(language)) = (audio, selection.preferred_audio_language.as_deref()) {
        // A known different language still remains manually playable, but do not
        // advertise a preferred-language native match without track evidence.
        if audio.language().is_some_and(|actual| {
            normalize_audio_language(&actual) != normalize_audio_language(language)
        }) {
            return None;
        }
    }
    let video = probe.video().ok()?;
    let width = caps.max_width.min(3840);
    let height = caps.max_height.min(2160);
    let h264 = probe.compatible_video(width.min(1920), height.min(1080), H264_COPY_LEVEL);
    let hevc = caps.hevc
        && caps.hevc_sdr
        && video.codec_name.as_deref() == Some("hevc")
        && video.profile.as_deref() == Some("Main")
        && video.pix_fmt.as_deref() == Some("yuv420p")
        && video.level.is_some_and(|l| l > 0 && l <= 150)
        && video
            .width
            .is_some_and(|w| w >= 2 && w <= width && w % 2 == 0)
        && video
            .height
            .is_some_and(|h| h >= 2 && h <= height && h % 2 == 0)
        && conservative_frame_rate(video.avg_frame_rate.as_deref())
        && conservative_frame_rate(video.r_frame_rate.as_deref())
        && !probe.interlaced()
        && matches!(probe.hdr_transfer(), Ok(None));
    if !h264 && !hevc {
        return None;
    }
    let format = probe.format.get("format_name")?.as_str()?;
    if format.split(',').any(|f| f == "hls") {
        caps.direct_hls.unwrap_or(true).then_some("hls")
    } else if format.split(',').any(|f| f == "mp4" || f == "mov") {
        caps.direct_mp4.unwrap_or(true).then_some("mp4")
    } else {
        None
    }
}

fn dimensions(caps: &Capabilities) -> Result<(u32, u32), String> {
    if caps.max_width < 2 || caps.max_height < 2 {
        return Err("Invalid playback dimensions".into());
    }
    Ok((
        caps.max_width.min(1920) / 2 * 2,
        caps.max_height.min(1080) / 2 * 2,
    ))
}
fn hdr_size(width: u32, height: u32) -> String {
    // Fit both axes, including portrait, without upscaling; round down to even pixels.
    format!("w='trunc(min(iw,min({width},iw*{height}/ih))/2)*2':h='trunc(min(ih,min({height},ih*{width}/iw))/2)*2'")
}
fn hdr_filter(width: u32, height: u32) -> String {
    // zimg resizes BEFORE transfer conversion when both are in one zscale. Keep
    // explicit PQ/HLG -> float-linear RGB first to avoid averaging encoded light.
    // Fuse bounded linear-light resize with the linear BT709 primaries transform;
    // the expensive tone mapper and remaining conversions then run at target size.
    // Only FFmpeg 5.1-compatible options; preserve SDR tags and drop HDR side data.
    format!("zscale=transfer=linear:npl=100,format=gbrpf32le,zscale={}:filter=bilinear:primaries=bt709,tonemap=tonemap=mobius:desat=2,zscale=transfer=bt709:matrix=bt709:range=limited,format=yuv420p,sidedata=mode=delete,setsar=1", hdr_size(width, height))
}
fn scale_filter(width: u32, height: u32) -> String {
    format!("scale=w='min(iw,{width})':h='min(ih,{height})':force_original_aspect_ratio=decrease:force_divisible_by=2:flags=fast_bilinear,setsar=1")
}
fn stable_hls_target_duration(bytes: Vec<u8>) -> Vec<u8> {
    let mut playlist = match String::from_utf8(bytes) {
        Ok(playlist) => playlist,
        Err(error) => return error.into_bytes(),
    };
    let prefix = "#EXT-X-TARGETDURATION:";
    if let Some(start) = playlist.find(prefix) {
        let value_start = start + prefix.len();
        let value_end = playlist[value_start..]
            .find(['\r', '\n'])
            .map_or(playlist.len(), |offset| value_start + offset);
        let advertised = playlist[value_start..value_end].parse::<u32>().unwrap_or(0);
        if advertised < HLS_SEGMENT_SECONDS {
            playlist.replace_range(value_start..value_end, &HLS_SEGMENT_SECONDS.to_string());
        }
    }
    playlist.into_bytes()
}

fn media_type(file: &str) -> Option<&'static str> {
    if matches!(file, "index.m3u8" | "master.m3u8" | "index_vtt.m3u8") {
        return Some("application/vnd.apple.mpegurl");
    }
    // Original containers are handed over whole; the WebCodecs client demuxes
    // them, so a neutral type is both accurate and preferable to a guess.
    if file.starts_with("source.") {
        return Some(match file.rsplit('.').next() {
            Some("mp4") => "video/mp4",
            Some("mov") => "video/quicktime",
            Some("mkv") => "video/x-matroska",
            Some("webm") => "video/webm",
            Some("ts") => "video/mp2t",
            _ => "application/octet-stream",
        });
    }
    if let Some(digits) = file
        .strip_prefix("index")
        .and_then(|f| f.strip_suffix(".vtt"))
    {
        return (!digits.is_empty()
            && digits.len() <= 16
            && digits.bytes().all(|b| b.is_ascii_digit()))
        .then_some("text/vtt");
    }
    let digits = file.strip_prefix("segment-")?.strip_suffix(".ts")?;
    if digits.len() >= 9 && digits.len() <= 16 && digits.bytes().all(|b| b.is_ascii_digit()) {
        Some("video/mp2t")
    } else {
        None
    }
}
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |diff, (a, b)| diff | (a ^ b)) == 0
}
// HLS list_size bounds completed segments, NOT an open segment waiting for a
// keyframe. Poll even while leases are refreshed. These are watchdog limits,
// not hard disk quotas: deploy the media root on a quota-limited filesystem.
async fn cache_safe(dir: &std::path::Path, require_progress: bool) -> bool {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return false;
    };
    let mut bytes = 0u64;
    let mut count = 0usize;
    loop {
        let entry = match entries.next_entry().await {
            Ok(Some(entry)) => entry,
            Ok(None) => break,
            Err(_) => return false,
        };
        count += 1;
        let Ok(meta) = entry.metadata().await else {
            continue;
        }; // deletion race
        bytes = bytes.saturating_add(meta.len());
        // The configured 120s window plus 60s delete grace can retain up to
        // 180 two-second AV/VTT files; leave a small margin for playlists.
        if count > 256 || meta.len() > 32 * 1024 * 1024 || bytes > 384 * 1024 * 1024 {
            tracing::warn!("Playback cache watchdog limit exceeded");
            return false;
        }
    }
    if require_progress {
        let fresh = tokio::fs::metadata(dir.join("index.m3u8"))
            .await
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age <= Duration::from_secs(20));
        if !fresh {
            tracing::warn!("Playback playlist stalled; closing session");
            return false;
        }
    }
    true
}

async fn playback_ready(dir: &std::path::Path, subtitle: Option<&SelectedSubtitle>) -> bool {
    if !playlist_ready(dir).await {
        return false;
    }
    let Some(subtitle) = subtitle else {
        return true;
    };
    let Ok(playlist) = tokio::fs::read_to_string(dir.join("index_vtt.m3u8")).await else {
        return false;
    };
    let mut caption_ready = false;
    for file in playlist
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        if media_type(file) == Some("text/vtt")
            && tokio::fs::metadata(dir.join(file))
                .await
                .is_ok_and(|m| m.len() > 0)
        {
            caption_ready = true;
            break;
        }
    }
    if !caption_ready {
        return false;
    }
    let Ok(av) = tokio::fs::read_to_string(dir.join("index.m3u8")).await else {
        return false;
    };
    let Some(segment) = av
        .lines()
        .find(|line| media_type(line) == Some("video/mp2t"))
    else {
        return false;
    };
    let Ok(info) = tokio::fs::metadata(dir.join(segment)).await else {
        return false;
    };
    let duration = av
        .lines()
        .find_map(|line| line.strip_prefix("#EXTINF:"))
        .and_then(|value| value.split(',').next())
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(HLS_SEGMENT_SECONDS as f64);
    let bandwidth =
        ((info.len() as f64 * 8.0 / duration * 1.25).ceil() as u64).clamp(64000, 128000000);
    let language = subtitle.language.as_deref().unwrap_or("und");
    // FFmpeg can omit STREAM-INF until a sparse subtitle's first packet arrives.
    // Publish our one-variant master from already ready local media instead. Its
    // bandwidth is an initial measured estimate, not a source/codec assertion.
    let master = format!("#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"Subtitles\",DEFAULT=YES,AUTOSELECT=YES,LANGUAGE=\"{language}\",URI=\"index_vtt.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH={bandwidth},SUBTITLES=\"subs\"\nindex.m3u8\n");
    if tokio::fs::write(dir.join("master.m3u8.tmp"), master)
        .await
        .is_err()
    {
        return false;
    }
    tokio::fs::rename(dir.join("master.m3u8.tmp"), dir.join("master.m3u8"))
        .await
        .is_ok()
}

async fn playlist_ready(dir: &std::path::Path) -> bool {
    let Ok(playlist) = tokio::fs::read_to_string(dir.join("index.m3u8")).await else {
        return false;
    };
    for line in playlist
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
    {
        if media_type(line) == Some("video/mp2t")
            && tokio::fs::metadata(dir.join(line))
                .await
                .is_ok_and(|m| m.len() > 0)
        {
            return true;
        }
    }
    false
}
#[derive(Clone, Deserialize)]
struct Probe {
    streams: Vec<ProbeStream>,
    #[serde(default)]
    format: serde_json::Value,
}
#[derive(Clone, Debug, Deserialize)]
struct ProbeStream {
    index: Option<u32>,
    disposition: Option<ProbeDisposition>,
    #[serde(default)]
    tags: HashMap<String, String>,
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    pix_fmt: Option<String>,
    sample_aspect_ratio: Option<String>,
    profile: Option<String>,
    level: Option<u32>,
    channels: Option<u32>,
    avg_frame_rate: Option<String>,
    r_frame_rate: Option<String>,
    color_transfer: Option<String>,
    field_order: Option<String>,
}
/// The highest H.264 level this engine passes through. Modern browser H.264
/// decoders cover level 5.1, and the envelope's dimension, frame-rate, profile,
/// pix-fmt, interlace and SDR gates bound the actual decode load, so a level
/// tag alone never forces a re-encode of otherwise compatible video.
const H264_COPY_LEVEL: u32 = 51;

fn conservative_frame_rate(rate: Option<&str>) -> bool {
    let Some((numerator, denominator)) = rate.and_then(|r| r.split_once('/')) else {
        return false;
    };
    if ![numerator, denominator]
        .iter()
        .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
    {
        return false;
    }
    let (Ok(numerator), Ok(denominator)) = (numerator.parse::<u64>(), denominator.parse::<u64>())
    else {
        return false;
    };
    numerator > 0 && denominator > 0 && u128::from(numerator) <= u128::from(denominator) * 60
}

impl ProbeStream {
    fn text_subtitle(&self) -> bool {
        self.codec_type.as_deref() == Some("subtitle")
            && matches!(
                self.codec_name.as_deref(),
                Some("subrip" | "ass" | "ssa" | "webvtt" | "mov_text" | "text")
            )
    }
    fn selectable(&self) -> bool {
        if self.codec_type.as_deref() == Some("subtitle") {
            self.text_subtitle()
        } else {
            self.codec_type.as_deref() == Some("audio")
        }
    }
    fn language(&self) -> Option<String> {
        self.tags
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("language"))
            .map(|(_, v)| v.trim().to_ascii_lowercase())
            .filter(|v| {
                (2..=3).contains(&v.len())
                    && v.bytes().all(|b| b.is_ascii_alphabetic())
                    && v != "und"
                    && v != "zxx"
                    && v != "mul"
            })
    }
    fn language_status(&self) -> &'static str {
        match self.language().as_deref() {
            Some("en" | "eng") => "tagged_english",
            Some(_) => "tagged_non_english",
            None => "unknown",
        }
    }
}

fn normalize_audio_language(language: &str) -> &str {
    let language = language.split('-').next().unwrap_or(language);
    match language {
        "en" | "eng" => "eng",
        "ja" | "jpn" => "jpn",
        "es" | "spa" => "spa",
        "it" | "ita" => "ita",
        "fr" | "fra" | "fre" => "fra",
        "de" | "deu" | "ger" => "deu",
        "pt" | "por" => "por",
        "ko" | "kor" => "kor",
        "zh" | "zho" | "chi" => "zho",
        "hi" | "hin" => "hin",
        "ar" | "ara" => "ara",
        _ => language,
    }
}

impl Probe {
    fn tracks(&self, kind: &str) -> Vec<MediaTrack> {
        self.streams
            .iter()
            .filter(|s| s.codec_type.as_deref() == Some(kind))
            .filter_map(|s| s.index.filter(|i| *i <= 65535).map(|index| (s, index)))
            .take(32)
            .map(|(s, input_index)| MediaTrack {
                input_index,
                codec: s
                    .codec_name
                    .as_ref()
                    .filter(|c| {
                        c.len() <= 32 && c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                    })
                    .cloned(),
                language: s.language(),
                language_status: s.language_status().into(),
                title: format!("{kind} track {input_index}"),
                disposition: s.disposition.as_ref().map(ProbeDisposition::public),
                selected: false,
                supported: s.selectable(),
                selectable: s.selectable(),
            })
            .collect()
    }
    fn select_subtitle(&self, index: Option<u32>) -> Result<Option<&ProbeStream>, String> {
        let Some(index) = index else {
            return Ok(None);
        };
        let stream = self
            .streams
            .iter()
            .filter(|s| s.codec_type.as_deref() == Some("subtitle"))
            .filter(|s| s.index.is_some_and(|i| i <= 65535))
            .take(32)
            .find(|s| s.index == Some(index))
            .ok_or("Requested input subtitle track is unavailable")?;
        if !stream.text_subtitle() {
            return Err(
                "Requested subtitle track uses an unsupported bitmap or non-text codec".into(),
            );
        }
        Ok(Some(stream))
    }
    fn select_audio(&self, selection: &TrackSelection) -> Result<Option<&ProbeStream>, String> {
        let audio: Vec<_> = self
            .streams
            .iter()
            .filter(|stream| stream.codec_type.as_deref() == Some("audio"))
            .filter(|stream| stream.index.is_some_and(|index| index <= 65535))
            .take(32)
            .collect();
        if let Some(index) = selection.audio_track_index {
            return audio
                .iter()
                .find(|stream| stream.index == Some(index))
                .copied()
                .map(Some)
                .ok_or_else(|| "Requested input audio track is unavailable".into());
        }
        if let Some(language) = selection.audio_language.as_deref() {
            let language = normalize_audio_language(language);
            let chosen = audio
                .iter()
                .copied()
                .filter(|stream| {
                    stream
                        .language()
                        .is_some_and(|actual| normalize_audio_language(&actual) == language)
                })
                .min_by_key(|stream| {
                    let d = stream.disposition.as_ref();
                    (
                        d.is_some_and(|d| {
                            d.comment == 1 || d.visual_impaired == 1 || d.hearing_impaired == 1
                        }),
                        !d.is_some_and(|d| d.default == 1),
                        stream.index,
                    )
                });
            return chosen.map(Some).ok_or_else(|| {
                "Preferred audio language is unavailable; choose a source or audio track".into()
            });
        }
        Ok(audio.into_iter().min_by_key(|stream| {
            let disposition = stream.disposition.as_ref();
            let alternate = disposition.is_some_and(|value| {
                value.comment == 1 || value.visual_impaired == 1 || value.hearing_impaired == 1
            });
            (
                alternate,
                !stream.language().is_some_and(|actual| {
                    normalize_audio_language(&actual)
                        == normalize_audio_language(
                            selection
                                .preferred_audio_language
                                .as_deref()
                                .unwrap_or("en"),
                        )
                }),
                !disposition.is_some_and(|value| value.default == 1),
                stream.index.unwrap_or(u32::MAX),
            )
        }))
    }
    fn video(&self) -> Result<&ProbeStream, String> {
        self.streams
            .iter()
            .find(|s| s.codec_type.as_deref() == Some("video"))
            .ok_or_else(|| "Source has no supported video stream".to_owned())
    }
    fn interlaced(&self) -> bool {
        self.video().is_ok_and(|video| {
            matches!(
                video.field_order.as_deref(),
                Some("tt" | "bb" | "tb" | "bt")
            )
        })
    }
    fn ensure_supported(&self) -> Result<(), String> {
        self.hdr_transfer().map(|_| ())
    }
    /// PQ or HLG transfer characteristics mean HDR no matter how complete the
    /// colour tagging is: tone-mapping handles missing primaries or matrix.
    /// Mislabeled encodes are common (bt2020 primaries or mastering-display
    /// side data on an SDR transfer), so partial HDR/wide-gamut evidence
    /// without PQ/HLG reads as SDR instead of refusing a playable source.
    /// Dolby Vision and other dynamic-HDR metadata decode as their base layer:
    /// FFmpeg drops the RPU and the transfer characteristics alone decide, so
    /// no metadata refuses playback.
    fn hdr_transfer(&self) -> Result<Option<&str>, String> {
        Ok(self
            .video()?
            .color_transfer
            .as_deref()
            .filter(|transfer| matches!(*transfer, "smpte2084" | "arib-std-b67")))
    }
    fn duration(&self) -> Option<f64> {
        let v = &self.format["duration"];
        v.as_f64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            .filter(|d| d.is_finite() && *d > 0.0 && *d <= 604800.0)
    }
    #[cfg(test)]
    fn compatible(&self, width: u32, height: u32) -> bool {
        self.compatible_audio(
            width,
            height,
            self.streams
                .iter()
                .find(|s| s.codec_type.as_deref() == Some("audio")),
        )
    }
    #[cfg(test)]
    fn compatible_audio(&self, width: u32, height: u32, audio: Option<&ProbeStream>) -> bool {
        self.compatible_video(width, height, H264_COPY_LEVEL)
            && self.compatible_audio_stream(audio)
    }
    fn compatible_video(&self, width: u32, height: u32, max_h264_level: u32) -> bool {
        if self.interlaced() || !matches!(self.hdr_transfer(), Ok(None)) {
            return false;
        }
        let Some(video) = self
            .streams
            .iter()
            .find(|s| s.codec_type.as_deref() == Some("video"))
        else {
            return false;
        };
        video.codec_name.as_deref() == Some("h264")
            && video.pix_fmt.as_deref() == Some("yuv420p")
            && matches!(
                video.profile.as_deref(),
                Some("Constrained Baseline" | "Baseline" | "Main" | "High")
            )
            && video
                .level
                .is_some_and(|level| level > 0 && level <= max_h264_level)
            && conservative_frame_rate(video.avg_frame_rate.as_deref())
            && conservative_frame_rate(video.r_frame_rate.as_deref())
            && video
                .width
                .is_some_and(|w| w >= 2 && w <= width && w % 2 == 0)
            && video
                .height
                .is_some_and(|h| h >= 2 && h <= height && h % 2 == 0)
    }
    fn compatible_audio_stream(&self, audio: Option<&ProbeStream>) -> bool {
        audio.is_none_or(|stream| {
            stream.codec_name.as_deref() == Some("aac")
                && stream.profile.as_deref() == Some("LC")
                && stream
                    .channels
                    .is_some_and(|channels| channels > 0 && channels <= 2)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_transport_capabilities_preserve_legacy_and_gate_inspected_format() {
        let mut probe: Probe = serde_json::from_value(serde_json::json!({
            "streams": [
                {"codec_type":"video","codec_name":"h264","width":1280,"height":720,
                 "pix_fmt":"yuv420p","profile":"High","level":41,
                 "avg_frame_rate":"30/1","r_frame_rate":"30/1"},
                {"codec_type":"audio","codec_name":"aac","profile":"LC","channels":2}
            ],
            "format":{"format_name":"mov,mp4"}
        }))
        .unwrap();
        let legacy: Capabilities = serde_json::from_value(serde_json::json!({
            "direct_play":true,"h264":true,"aac":true
        }))
        .unwrap();
        assert!(legacy.direct_play);
        assert!(!Capabilities::default().direct_play);
        let selection = TrackSelection::default();
        for (format, expected) in [("mov,mp4", "mp4"), ("hls", "hls")] {
            probe.format["format_name"] = serde_json::json!(format);
            assert_eq!(
                direct_format(&probe, &legacy, probe.streams.get(1), &selection),
                Some(expected)
            );
            for (mp4, hls) in [(true, false), (false, true), (false, false)] {
                let caps = Capabilities {
                    direct_mp4: Some(mp4),
                    direct_hls: Some(hls),
                    ..legacy.clone()
                };
                let supported = if expected == "mp4" { mp4 } else { hls };
                assert_eq!(
                    direct_format(&probe, &caps, probe.streams.get(1), &selection),
                    supported.then_some(expected)
                );
            }
        }
    }

    #[test]
    fn a_webcodecs_client_gets_the_original_container_it_declares() {
        let probe: Probe = serde_json::from_value(serde_json::json!({
            "streams": [
                {"codec_type":"video","codec_name":"h264","width":1920,"height":1080,
                 "pix_fmt":"yuv420p","profile":"High","level":41,
                 "avg_frame_rate":"24/1","r_frame_rate":"24/1"},
                {"codec_type":"audio","codec_name":"eac3","channels":6}
            ],
            "format":{"format_name":"matroska,webm"}
        }))
        .unwrap();
        let selection = TrackSelection::default();
        let declared: Capabilities = serde_json::from_value(serde_json::json!({
            "direct_play": true, "h264": true, "aac": true,
            "max_width": 3840, "max_height": 2160,
            "direct_files": true,
            "direct_video_codecs": ["avc", "hevc", "av1"],
            "direct_audio_codecs": ["aac", "ac3", "eac3", "dts"]
        }))
        .unwrap();
        // Matroska with Dolby audio was previously remuxed; a client that reads the
        // container and both codecs now receives the original file.
        assert_eq!(
            direct_format(&probe, &declared, probe.streams.get(1), &selection),
            Some("mkv")
        );
        for (caps, reason) in [
            (
                Capabilities {
                    direct_files: Some(false),
                    ..declared.clone()
                },
                "no file path",
            ),
            (
                Capabilities {
                    direct_video_codecs: Some(vec!["hevc".into()]),
                    ..declared.clone()
                },
                "video codec not declared",
            ),
            (
                Capabilities {
                    direct_audio_codecs: Some(vec!["aac".into()]),
                    ..declared.clone()
                },
                "audio codec not declared",
            ),
            (
                Capabilities {
                    max_height: 720,
                    ..declared.clone()
                },
                "above the declared envelope",
            ),
            (
                Capabilities {
                    direct_files: Some(true),
                    h264: true,
                    aac: true,
                    ..Capabilities::default()
                },
                "nothing declared",
            ),
        ] {
            assert_eq!(
                direct_format(&probe, &caps, probe.streams.get(1), &selection),
                None,
                "{reason} must stay on managed delivery"
            );
        }
        // An interlaced picture is still converted rather than handed over raw.
        let interlaced: Probe = serde_json::from_value(serde_json::json!({
            "streams": [
                {"codec_type":"video","codec_name":"h264","width":1920,"height":1080,
                 "pix_fmt":"yuv420p","profile":"High","level":41,"field_order":"tt",
                 "avg_frame_rate":"25/1","r_frame_rate":"50/1"},
                {"codec_type":"audio","codec_name":"eac3","channels":6}
            ],
            "format":{"format_name":"matroska,webm"}
        }))
        .unwrap();
        assert_eq!(
            direct_format(
                &interlaced,
                &declared,
                interlaced.streams.get(1),
                &selection
            ),
            None
        );
    }

    #[test]
    fn original_file_containers_cover_the_observed_catalogue() {
        // Every container in the WebCodecs demuxer's format set must have a
        // direct path; a missing entry silently costs the raw-file rung.
        for (format, expected) in [
            ("mov,mp4,m4a,3gp,3g2,mj2", "mp4"),
            ("matroska,webm", "mkv"),
            ("mpegts", "ts"),
            ("ogg", "ogg"),
            ("wav", "wav"),
            ("mp3", "mp3"),
            ("flac", "flac"),
            ("adts", "aac"),
        ] {
            assert_eq!(direct_file_extension(format), Some(expected), "{format}");
        }
        // Containers outside the demuxer's set keep managed delivery rather
        // than failing in a client that cannot read them.
        for format in ["avi", "flv", "asf", "mpeg", "unknown,container"] {
            assert_eq!(direct_file_extension(format), None, "{format}");
        }
    }

    /// A real live 720p60 channel must be stream-copied, not re-encoded.
    ///
    /// Every field below is the value ffprobe reports for production IPTV live
    /// sources (H.264 High 4.1, yuv420p, progressive, BT.709 SDR, 1280x720 at
    /// 59.94fps, AAC-LC stereo in MPEG-TS). Re-encoding this in realtime cannot
    /// keep up, so the managed HLS window underruns and playback stutters.
    #[test]
    fn a_720p60_live_channel_is_copied_instead_of_re_encoded() {
        let probe: Probe = serde_json::from_value(serde_json::json!({
            "streams": [
                {"codec_type":"video","codec_name":"h264","profile":"High","level":41,
                 "pix_fmt":"yuv420p","width":1280,"height":720,"field_order":"progressive",
                 "color_transfer":"bt709","color_primaries":"bt709","color_space":"bt709",
                 "avg_frame_rate":"60000/1001","r_frame_rate":"60000/1001"},
                {"codec_type":"audio","codec_name":"aac","profile":"LC","channels":2}
            ],
            "format":{"format_name":"mpegts"}
        }))
        .unwrap();
        assert!(
            probe.compatible_audio(1280, 720, probe.streams.get(1)),
            "a 720p60 H.264/AAC live channel must take the copy/remux path"
        );
    }

    /// A level tag alone no longer gates copying: modern browsers decode H.264
    /// level 5.1, and the envelope's dimension/fps/profile/pix-fmt/interlace/
    /// SDR gates bound the real decode load. Level-4.2 720p60 and 1080p60
    /// sources — exactly what a flat per-resolution level cap re-encoded —
    /// must stream-copy instead.
    #[test]
    fn level_42_720p60_and_1080p60_sources_copy_without_re_encode() {
        for (width, height) in [(1280, 720), (1920, 1080)] {
            let probe: Probe = serde_json::from_value(serde_json::json!({
                "streams": [
                    {"codec_type":"video","codec_name":"h264","profile":"High","level":42,
                     "pix_fmt":"yuv420p","width":width,"height":height,"field_order":"progressive",
                     "color_transfer":"bt709","color_primaries":"bt709","color_space":"bt709",
                     "avg_frame_rate":"60000/1001","r_frame_rate":"60000/1001"},
                    {"codec_type":"audio","codec_name":"aac","profile":"LC","channels":2}
                ],
                "format":{"format_name":"mpegts"}
            }))
            .unwrap();
            assert!(
                probe.compatible_audio(width, height, probe.streams.get(1)),
                "{width}x{height} level 4.2 at 60fps must take the copy/remux path"
            );
        }
        // Frame rate ceiling admits 60fps but still refuses far-out values.
        for rate in ["60000/1001", "60/1", "30/1", "24/1", "25/1", "50/1"] {
            assert!(
                conservative_frame_rate(Some(rate)),
                "{rate} must pass through"
            );
        }
        for rate in ["120/1", "0/1", "bogus", ""] {
            assert!(
                !conservative_frame_rate(Some(rate)),
                "{rate} must not be treated as a copyable rate"
            );
        }
        assert!(!conservative_frame_rate(None));
    }

    #[test]
    fn an_hdr_original_file_is_delivered_to_a_client_that_can_decode_it() {
        // Production sources are commonly HDR10/HLG with partial or absent colour
        // tags. A WebCodecs client decodes these itself, so the native SDR
        // envelope must not refuse the original file before it is offered.
        let selection = TrackSelection::default();
        let declared: Capabilities = serde_json::from_value(serde_json::json!({
            "direct_play": true, "h264": true, "aac": true,
            "max_width": 3840, "max_height": 2160, "hevc": true, "hevc_sdr": false,
            "direct_files": true,
            "direct_video_codecs": ["avc", "hevc", "av1"],
            "direct_audio_codecs": ["aac", "ac3", "eac3", "dts"]
        }))
        .unwrap();
        for (label, colour) in [
            (
                "tagged HDR10",
                serde_json::json!({"color_transfer":"smpte2084","color_primaries":"bt2020","color_space":"bt2020nc"}),
            ),
            (
                "untagged HDR10",
                serde_json::json!({"color_transfer":"smpte2084"}),
            ),
            ("HLG", serde_json::json!({"color_transfer":"arib-std-b67"})),
            (
                "wide gamut without transfer",
                serde_json::json!({"color_primaries":"bt2020","color_space":"bt2020nc"}),
            ),
            (
                "mastering-display side data only",
                serde_json::json!({"side_data_list":[{"side_data_type":"Mastering display metadata"}]}),
            ),
        ] {
            let mut video = serde_json::json!({
                "codec_type":"video","codec_name":"hevc","width":3840,"height":1608,
                "pix_fmt":"yuv420p10le","profile":"Main 10","level":153,
                "avg_frame_rate":"24/1","r_frame_rate":"24/1"
            });
            for (key, value) in colour.as_object().unwrap() {
                video[key] = value.clone();
            }
            let probe: Probe = serde_json::from_value(serde_json::json!({
                "streams": [
                    video,
                    {"codec_type":"audio","codec_name":"eac3","channels":6}
                ],
                "format":{"format_name":"matroska,webm"}
            }))
            .unwrap();
            assert_eq!(
                direct_format(&probe, &declared, probe.streams.get(1), &selection),
                Some("mkv"),
                "{label} must reach the client as the original file"
            );
        }
    }

    /// Interlacing still forces conversion even for a WebCodecs client: the
    /// demuxer feeds a progressive decoder that cannot reconstruct fields.
    #[test]
    fn hdr_is_permitted_for_original_files_but_never_above_the_declared_envelope() {
        let selection = TrackSelection::default();
        let declared: Capabilities = serde_json::from_value(serde_json::json!({
            "direct_play": true, "h264": true, "aac": true,
            "max_width": 1920, "max_height": 1080,
            "direct_files": true,
            "direct_video_codecs": ["hevc"],
            "direct_audio_codecs": ["eac3"]
        }))
        .unwrap();
        let probe: Probe = serde_json::from_value(serde_json::json!({
            "streams": [
                {"codec_type":"video","codec_name":"hevc","width":3840,"height":2160,
                 "pix_fmt":"yuv420p10le","profile":"Main 10","level":153,
                 "color_transfer":"smpte2084","color_primaries":"bt2020","color_space":"bt2020nc",
                 "avg_frame_rate":"24/1","r_frame_rate":"24/1"},
                {"codec_type":"audio","codec_name":"eac3","channels":6}
            ],
            "format":{"format_name":"matroska,webm"}
        }))
        .unwrap();
        // HDR is no longer a refusal, but a resolution above the declared
        // envelope still is.
        assert_eq!(
            direct_format(&probe, &declared, probe.streams.get(1), &selection),
            None
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn qsv_failure_retries_with_same_reservation_and_copies_compatible_audio() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let ffprobe = root.path().join("probe");
        std::fs::write(&ffprobe, r#"#!/bin/sh
printf '%s' '{"streams":[{"index":0,"codec_type":"video","codec_name":"hevc","width":640,"height":360,"pix_fmt":"yuv420p","sample_aspect_ratio":"1:1","field_order":"progressive"},{"index":1,"codec_type":"audio","codec_name":"aac","profile":"LC","channels":2,"tags":{"language":"eng"}}],"format":{"duration":"10"}}'
"#).unwrap();
        let ffmpeg = root.path().join("encoder");
        std::fs::write(
            &ffmpeg,
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$0.attempts"
case "$*" in
 *h264_qsv*) exit 1 ;;
esac
for last do :; done
dir=${last%/*}
printf x > "$dir/segment-000000000.ts"
printf '#EXTM3U\n#EXTINF:1,\nsegment-000000000.ts\n' > "$last"
"#,
        )
        .unwrap();
        for path in [&ffprobe, &ffmpeg] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let manager = PlaybackManager::new_with_qsv(
            Config {
                ffmpeg: ffmpeg.clone(),
                ffprobe,
                root: root.path().join("media"),
                max_sessions: 1,
                ttl: Duration::from_secs(60),
            },
            Some("/dev/dri/renderD128".into()),
        );
        manager.qsv_ready.set(true).unwrap();
        let provider = Arc::new(Semaphore::new(1));
        let result = manager
            .start_with_permit(
                "http://fixture.invalid/video".into(),
                HashMap::new(),
                0.0,
                None,
                false,
                false,
                Some(provider.clone().acquire_owned().await.unwrap()),
            )
            .await
            .unwrap();
        assert_eq!(result.video_mode, "encode");
        assert_eq!(result.audio_mode, "copy");
        assert_eq!(provider.available_permits(), 0);
        let attempts = std::fs::read_to_string(ffmpeg.with_extension("attempts")).unwrap();
        let attempts: Vec<_> = attempts.lines().collect();
        assert_eq!(attempts.len(), 3);
        assert!(attempts[0].contains("-c:v hevc_qsv"));
        assert!(attempts[1].contains("hwupload="));
        assert!(attempts[2].contains("libx264"));
        assert!(!attempts[2].contains("-init_hw_device"));
        assert!(attempts.iter().all(|a| a.contains("-c:a copy")));
        assert!(manager.stop(&result.id).await);
        assert_eq!(provider.available_permits(), 1);
        manager.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn direct_url_clients_receive_the_original_source_instead_of_a_session() {
        let root = tempfile::tempdir().unwrap();
        // HDR HEVC with Dolby audio in Matroska: the declared envelope refuses
        // to hand this over, so anything but direct-url delivery would
        // transcode it. A native client fetches the source itself instead.
        let manager = scripted_probe(
            root.path(),
            r#"printf '%s' '{"streams":[
                {"index":0,"codec_type":"video","codec_name":"hevc","width":3840,"height":1608,
                 "pix_fmt":"yuv420p10le","profile":"Main 10","level":153,
                 "color_transfer":"smpte2084","avg_frame_rate":"24/1","r_frame_rate":"24/1"},
                {"index":1,"codec_type":"audio","codec_name":"eac3","channels":6}
            ],"format":{"format_name":"matroska,webm","duration":"123.5"}}'"#,
        );
        let caps: Capabilities = serde_json::from_value(serde_json::json!({
            "h264": true, "aac": true, "max_width": 3840, "max_height": 2160,
            "direct_play": false, "direct_urls": true
        }))
        .unwrap();
        let mut headers = HashMap::new();
        headers.insert("Cookie".to_owned(), "session=opaque".to_owned());
        headers.insert("user-agent".to_owned(), "viptv-native/1".to_owned());
        headers.insert("Referer".to_owned(), "https://provider.example/watch".to_owned());
        let response = manager
            .start(
                "http://example.com/video".to_owned(),
                headers,
                0.0,
                Some(caps),
                false,
            )
            .await
            .unwrap();
        // The client's own engine fetches the original URL: no proxy session,
        // no transcode, and no codec-policy refusal.
        assert_eq!(response.url, "http://example.com/video");
        assert_eq!(response.mode, "direct");
        assert_eq!(response.video_mode, "copy");
        assert_eq!(response.audio_mode, "copy");
        assert_eq!(response.format, "mkv");
        assert!((response.duration - 123.5).abs() < 0.01);
        let authorization = response.authorization.expect("upstream authorization");
        assert_eq!(authorization.cookie.as_deref(), Some("session=opaque"));
        assert_eq!(authorization.user_agent.as_deref(), Some("viptv-native/1"));
        let forwarded = authorization.headers.expect("upstream header set");
        assert_eq!(
            forwarded.get("Referer").map(String::as_str),
            Some("https://provider.example/watch")
        );
        assert!(!forwarded.contains_key("Cookie"));
        assert!(!forwarded.contains_key("user-agent"));
        // The transport-less session still answers heartbeats.
        assert!(manager.heartbeat(&response.id).await);
        manager.shutdown().await;
    }

    #[cfg(unix)]
    fn scripted_probe(root: &std::path::Path, body: &str) -> Arc<PlaybackManager> {
        use std::os::unix::fs::PermissionsExt;
        let script = root.join("probe.sh");
        std::fs::write(
            &script,
            format!("#!/bin/sh\nprintf x >> \"$0.count\"\n{body}\n"),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        PlaybackManager::new(Config {
            ffmpeg: root.join("missing-ffmpeg"),
            ffprobe: script,
            root: root.join("media"),
            max_sessions: 1,
            ttl: Duration::from_secs(60),
        })
    }

    #[cfg(unix)]
    async fn wait_probe_file(path: &std::path::Path) -> String {
        timeout(Duration::from_secs(3), async {
            loop {
                if let Ok(value) = tokio::fs::read_to_string(path).await {
                    if !value.is_empty() {
                        return value;
                    }
                }
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bounded_probe_cache_reuses_identical_sources_within_their_ttl() {
        let root = tempfile::tempdir().unwrap();
        let manager = scripted_probe(
            root.path(),
            r#"printf '%s' '{"streams":[{"index":0,"codec_type":"video","codec_name":"h264"}]}'"#,
        );
        let first = manager
            .cached_probe(
                "http://example.com/video",
                "Authorization: opaque",
                false,
                None,
            )
            .await
            .unwrap();
        let second = manager
            .cached_probe(
                "http://example.com/video",
                "Authorization: opaque",
                false,
                None,
            )
            .await
            .unwrap();
        assert_eq!(first.streams.len(), second.streams.len());
        assert_eq!(
            std::fs::read_to_string(root.path().join("probe.sh.count")).unwrap(),
            "x",
            "a seek/restart of the same authorized VOD must not repeat ffprobe"
        );
        // Live sources share the cache now: a channel change inside the short
        // live TTL must not pay another full ffprobe before playback starts.
        manager
            .cached_probe("http://example.com/live", "", true, None)
            .await
            .unwrap();
        manager
            .cached_probe("http://example.com/live", "", true, None)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.path().join("probe.sh.count"))
                .unwrap()
                .len(),
            2,
            "a live channel change within the TTL must not repeat ffprobe"
        );
        // The live discriminator in the digest keeps live and VOD identities
        // apart even for one URL.
        manager
            .cached_probe("http://example.com/video", "Authorization: opaque", true, None)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.path().join("probe.sh.count"))
                .unwrap()
                .len(),
            3,
            "live playback of a VOD-probed URL must not reuse the VOD entry"
        );
        // Header changes still bypass the cached identity.
        manager
            .cached_probe("http://example.com/video", "Authorization: changed", false, None)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.path().join("probe.sh.count"))
                .unwrap()
                .len(),
            4,
            "header changes must bypass the cached identity"
        );
        // Live entries expire on their own shorter TTL, not the VOD window.
        for entry in manager.probe_cache.lock().await.values_mut() {
            if entry.live {
                entry.inserted = Instant::now() - LIVE_PROBE_CACHE_TTL - Duration::from_secs(1);
            }
        }
        manager
            .cached_probe("http://example.com/live", "", true, None)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.path().join("probe.sh.count"))
                .unwrap()
                .len(),
            5,
            "an expired live entry must be inspected again"
        );
        assert!(manager
            .probe_cache
            .lock()
            .await
            .keys()
            .all(|key| key.len() == 32));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn playing_vod_keeps_probe_until_its_last_session_releases_it() {
        let root = tempfile::tempdir().unwrap();
        let manager = scripted_probe(
            root.path(),
            r#"printf '%s' '{"streams":[{"index":0,"codec_type":"video","codec_name":"h264"}]}'"#,
        );
        let playing = manager
            .cached_probe("http://example.com/movie", "", false, None)
            .await
            .unwrap();
        for entry in manager.probe_cache.lock().await.values_mut() {
            entry.inserted = Instant::now() - PROBE_CACHE_TTL - Duration::from_secs(1);
        }
        let seek = manager
            .cached_probe("http://example.com/movie", "", false, None)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.path().join("probe.sh.count")).unwrap(),
            "x",
            "a seek during playback repeated inspection after the short idle cache TTL"
        );
        drop(playing);
        drop(seek);
        manager
            .cached_probe("http://example.com/movie", "", false, None)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.path().join("probe.sh.count")).unwrap(),
            "xx",
            "an idle expired source must be inspected again"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn probe_retries_one_transient_failure_and_holds_admission() {
        let root = tempfile::tempdir().unwrap();
        let manager = scripted_probe(
            root.path(),
            r#"
if [ "$(wc -c < "$0.count")" -eq 1 ]; then
    printf 'HTTP error 404 Not Found\n' >&2
    exit 1
fi
printf '%s' '{"streams":[{"codec_type":"video","codec_name":"h264","width":960,"height":540,"pix_fmt":"yuv420p"},{"codec_type":"audio","codec_name":"aac","profile":"HE-AAC","channels":2}]}'
"#,
        );
        let capacity = Arc::new(Semaphore::new(1));
        let permit = capacity.clone().acquire_owned().await.unwrap();
        let worker = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .start_with_permit(
                        "http://example.com/video".into(),
                        HashMap::new(),
                        0.0,
                        None,
                        false,
                        true,
                        Some(permit),
                    )
                    .await
            })
        };
        wait_probe_file(&root.path().join("probe.sh.count")).await;
        assert_eq!(capacity.available_permits(), 0);
        assert_eq!(manager.slots.available_permits(), 0);
        let result = timeout(Duration::from_secs(5), worker)
            .await
            .unwrap()
            .unwrap();
        // The valid second probe advances to ffmpeg startup (fixture ffmpeg deliberately absent).
        assert!(!result.unwrap_err().contains("inspect source"));
        assert_eq!(
            std::fs::read(root.path().join("probe.sh.count"))
                .unwrap()
                .len(),
            2
        );
        let cleanup = std::mem::take(&mut *manager.cleanup_tasks.lock().unwrap());
        for task in cleanup {
            timeout(Duration::from_secs(3), task)
                .await
                .unwrap()
                .unwrap();
        }
        assert_eq!(capacity.available_permits(), 1);
        assert_eq!(manager.slots.available_permits(), 1);
        assert!(manager
            .probe("http://example.com/video", "", None)
            .await
            .is_some());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn probe_attempts_are_bounded_and_permanent_failures_never_retry() {
        for (body, count) in [
            (
                "printf 'HTTP error 503 Service Unavailable\\n' >&2; exit 1",
                2,
            ),
            ("printf 'Connection reset by peer\\n' >&2; exit 1", 2),
            ("printf 'HTTP error 401 Unauthorized\\n' >&2; exit 1", 1),
            ("printf 'Server returned 403 Forbidden\\n' >&2; exit 1", 1),
            (
                "printf 'Protocol not on whitelist\\nHTTP error 404 Not Found\\n' >&2; exit 1",
                1,
            ),
            // Unreadable or oversized metadata earns exactly one reduced-entry
            // retry: a noisy response is not evidence of a dead source.
            ("printf 'not json'", 2),
            (
                "printf 'not json'; printf 'HTTP error 404 Not Found\\n' >&2; exit 1",
                2,
            ),
            ("head -c 1048577 /dev/zero", 2),
            ("head -c 65537 /dev/zero >&2; exit 1", 1),
        ] {
            let root = tempfile::tempdir().unwrap();
            let manager = scripted_probe(root.path(), body);
            assert!(timeout(
                Duration::from_secs(5),
                manager.probe("http://example.com/video", "", None)
            )
            .await
            .unwrap()
            .is_none());
            assert_eq!(
                std::fs::read(root.path().join("probe.sh.count"))
                    .unwrap()
                    .len(),
                count
            );
        }
        let root = tempfile::tempdir().unwrap();
        let manager = scripted_probe(root.path(), "exit 0");
        std::fs::remove_file(root.path().join("probe.sh")).unwrap();
        assert!(matches!(
            manager
                .probe_attempt("http://example.com/video", "", None, false)
                .await,
            Err(ProbeFailure::Spawn)
        ));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn unreadable_probe_output_falls_back_to_a_reduced_entry_set() {
        let root = tempfile::tempdir().unwrap();
        // The full scheme answers with something the parser cannot read; the
        // reduced scheme (three `-show_entries`, detected by its own count file)
        // must still describe the source instead of failing playback.
        let manager = scripted_probe(
            root.path(),
            r#"for arg in "$@"; do case "$arg" in *stream_disposition*) printf '%s' 'not json'; exit 0; esac; done; printf '%s' '{"format":{"format_name":"matroska,webm","duration":"120"},"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":1920,"height":1080,"pix_fmt":"yuv420p"},{"index":1,"codec_type":"audio","codec_name":"aac","channels":2}]}'"#,
        );
        let probe = manager
            .probe("http://example.com/video", "", None)
            .await
            .expect("a reduced probe must describe a source whose full probe was unreadable");
        assert_eq!(
            probe.format.get("format_name").and_then(|v| v.as_str()),
            Some("matroska,webm")
        );
        assert_eq!(probe.duration(), Some(120.0));
        assert_eq!(
            std::fs::read(root.path().join("probe.sh.count"))
                .unwrap()
                .len(),
            2,
            "one full attempt then one reduced attempt"
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn cancelled_probe_kills_and_reaps_child() {
        let root = tempfile::tempdir().unwrap();
        let manager = scripted_probe(root.path(), "printf '%s' $$ > \"$0.pid\"; exec sleep 60");
        let worker = {
            let manager = manager.clone();
            tokio::spawn(async move { manager.probe("http://example.com/video", "", None).await })
        };
        let pid = wait_probe_file(&root.path().join("probe.sh.pid")).await;
        let pid: u32 = pid.parse().unwrap();
        worker.abort();
        assert!(matches!(worker.await, Err(error) if error.is_cancelled()));
        let cleanup = std::mem::take(&mut *manager.cleanup_tasks.lock().unwrap());
        assert!(!cleanup.is_empty());
        for task in cleanup {
            timeout(Duration::from_secs(3), task)
                .await
                .unwrap()
                .unwrap();
        }
        assert!(
            !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "probe child must be reaped, not merely killed"
        );
        assert_eq!(
            std::fs::read(root.path().join("probe.sh.count"))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn probe_diagnostic_categories_are_closed_and_conservative() {
        for code in [404, 408, 429, 500, 503, 599] {
            let category = probe_failure(format!("HTTP error {code} failure").as_bytes());
            assert_eq!(category, ProbeFailure::Http(code));
            assert!(category.retryable());
        }
        assert_eq!(
            probe_failure(b"HTTP error 404\nServer returned 403 Forbidden"),
            ProbeFailure::Http(403)
        );
        assert_eq!(probe_failure(b"unknown diagnostic"), ProbeFailure::Exit);
        assert!(!ProbeFailure::InvalidJson.retryable());
        assert!(!ProbeFailure::Oversized.retryable());
        assert!(ProbeFailure::Timeout.retryable());
    }

    #[test]
    fn dynamic_hdr_metadata_reads_as_its_transfer() {
        // PQ/HLG transfer characteristics are HDR with or without complete
        // colour tags; tone-mapping does not need the primaries to be present.
        for transfer in ["smpte2084", "arib-std-b67"] {
            let probe: Probe = serde_json::from_value(serde_json::json!({"streams":[{
                "codec_type":"video", "pix_fmt":"yuv420p", "color_transfer": transfer
            }]}))
            .unwrap();
            assert_eq!(probe.hdr_transfer().unwrap(), Some(transfer));
        }
        // Mislabeled encodes carry bt2020 primaries or mastering-display side
        // data on an SDR transfer; treating those as HDR refused playable
        // sources outright.
        let mislabeled: Probe = serde_json::from_value(serde_json::json!({"streams":[{
            "codec_type":"video", "pix_fmt":"yuv420p",
            "color_transfer":"bt709", "color_primaries":"bt2020", "color_space":"bt2020nc",
            "side_data_list":[{"side_data_type":"Mastering display metadata"}]
        }]}))
        .unwrap();
        assert_eq!(mislabeled.hdr_transfer().unwrap(), None);
        // Dolby Vision and other dynamic-HDR side data no longer refuse
        // playback: the decoder drops the RPU and the base layer's transfer
        // decides, so a DoVi wrap around PQ tone-maps and DoVi SDR plays.
        let dynamic_pq: Probe = serde_json::from_value(serde_json::json!({"streams":[{
            "codec_type":"video", "color_transfer":"smpte2084",
            "side_data_list":[{"side_data_type":"DOVI configuration record"}]
        }]}))
        .unwrap();
        assert_eq!(dynamic_pq.hdr_transfer().unwrap(), Some("smpte2084"));
        let dynamic_sdr: Probe = serde_json::from_value(serde_json::json!({"streams":[{
            "codec_type":"video",
            "side_data_list":[{"side_data_type":"DOVI configuration record"}]
        }]}))
        .unwrap();
        assert_eq!(dynamic_sdr.hdr_transfer().unwrap(), None);
        assert!(dynamic_sdr.ensure_supported().is_ok());
    }
    #[tokio::test]
    async fn open_segment_growth_and_stalled_leased_process_are_reaped() {
        let root = tempfile::tempdir().unwrap();
        let manager = PlaybackManager::new(Config {
            ffmpeg: "missing".into(),
            ffprobe: "missing".into(),
            root: root.path().into(),
            max_sessions: 1,
            ttl: Duration::from_secs(60),
        });
        manager.initialize().await.unwrap();
        let dir = root.path().join("fixture");
        tokio::fs::create_dir(&dir).await.unwrap();
        let segment = tokio::fs::File::create(dir.join("segment-000000000.ts.tmp"))
            .await
            .unwrap();
        segment.set_len(32 * 1024 * 1024 + 1).await.unwrap();
        assert!(
            !cache_safe(&dir, false).await,
            "open segments must be counted"
        );
        segment.set_len(1).await.unwrap();
        assert!(cache_safe(&dir, false).await);
        assert!(
            !cache_safe(&dir, true).await,
            "active writer with no playlist has stalled"
        );
        let child = Command::new("/bin/sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        manager.sessions.lock().await.insert(
            "id".into(),
            Session {
                _source_probe: None,
                direct: None,
                capability: "cap".into(),
                dir: dir.clone(),
                child: Some(child),
                touched: Instant::now(),
                stable_target_duration: false,
                supervised_live: false,
                permits: Arc::new(InputPermits {
                    _playback: manager.slots.clone().try_acquire_owned().unwrap(),
                    _provider: None,
                }),
                cleanup_tasks: manager.cleanup_tasks.clone(),
            },
        );
        assert!(manager.heartbeat("id").await);
        manager.reap().await;
        assert!(!dir.exists());
        assert_eq!(manager.active_count().await, 0);
        assert!(manager.slots.clone().try_acquire_owned().is_ok());
        manager.shutdown().await;
    }
    #[test]
    fn full_source_duration_is_optional_and_validated() {
        for value in [serde_json::json!("20.5"), serde_json::json!(20.5)] {
            let probe: Probe = serde_json::from_value(
                serde_json::json!({"streams":[],"format":{"duration":value}}),
            )
            .unwrap();
            assert_eq!(probe.duration(), Some(20.5));
        }
        for value in [
            serde_json::json!("N/A"),
            serde_json::json!("NaN"),
            serde_json::json!(-1),
            serde_json::Value::Null,
        ] {
            let probe: Probe = serde_json::from_value(
                serde_json::json!({"streams":[],"format":{"duration":value}}),
            )
            .unwrap();
            assert_eq!(probe.duration(), None);
        }
    }
    #[tokio::test]
    async fn live_sources_reject_resume_offsets() {
        let root = tempfile::tempdir().unwrap();
        let manager = PlaybackManager::new(Config {
            ffmpeg: "missing".into(),
            ffprobe: "missing".into(),
            root: root.path().into(),
            max_sessions: 1,
            ttl: Duration::from_secs(30),
        });
        let error = manager
            .start_with_kind(
                "https://example.com/live.ts".into(),
                HashMap::new(),
                10.0,
                None,
                false,
                true,
            )
            .await
            .unwrap_err();
        assert!(error.contains("Live playback"));
        manager.shutdown().await;
    }
    #[test]
    fn rejects_header_injection() {
        assert!(header_block(&HashMap::from([(
            "User-Agent".into(),
            "test\r\nHost: evil".into()
        )]))
        .is_err());
        assert!(header_block(&HashMap::from([("Bad:Key".into(), "test".into())])).is_err());
        assert_eq!(
            header_block(&HashMap::from([("User-Agent".into(), "player".into())])).unwrap(),
            "User-Agent: player\r\n"
        );
    }
    #[test]
    fn served_hls_target_duration_is_stable_from_initial_to_steady_segments() {
        for (advertised, expected) in [("1", "2"), ("2", "2"), ("99", "99")] {
            let input = format!(
                "#EXTM3U\n#EXT-X-TARGETDURATION:{advertised}\n#EXTINF:1.0,\nsegment-000000000.ts\n"
            );
            let output = String::from_utf8(stable_hls_target_duration(input.into_bytes())).unwrap();
            assert!(output.contains(&format!("#EXT-X-TARGETDURATION:{expected}\n")));
        }
        let master = b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nindex.m3u8\n".to_vec();
        assert_eq!(stable_hls_target_duration(master.clone()), master);
    }
    #[test]
    fn serving_only_accepts_generated_names() {
        for bad in [
            "../index.m3u8",
            "segment-../../etc/passwd.ts",
            "segment-000000001.ts.tmp",
            "/index.m3u8",
            "segment-%2e%2e.ts",
            "index../private.vtt",
            "index.vtt",
            "index0.vtt.tmp",
            "index00000000000000000.vtt",
            "master.m3u8.tmp",
            "secret.txt",
        ] {
            assert!(media_type(bad).is_none());
        }
        assert!(media_type("segment-000000001.ts").is_some());
        assert_eq!(media_type("index0.vtt"), Some("text/vtt"));
        assert_eq!(
            media_type("index_vtt.m3u8"),
            Some("application/vnd.apple.mpegurl")
        );
        assert_eq!(
            media_type("master.m3u8"),
            Some("application/vnd.apple.mpegurl")
        );
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secrex"));
    }
    #[test]
    fn dimensions_are_bounded_even_and_filter_never_upscales() {
        let mut caps = Capabilities {
            max_width: 9999,
            max_height: 721,
            ..Capabilities::default()
        };
        assert_eq!(dimensions(&caps).unwrap(), (1920, 720));
        assert!(scale_filter(1280, 720).contains("min(iw,1280)"));
        assert!(scale_filter(1280, 720).contains("force_divisible_by=2"));
        caps.max_height = 1;
        assert!(dimensions(&caps).is_err());
    }
    #[test]
    fn remux_rates_admit_up_to_sixty_frames_per_second() {
        for rate in [
            "24/1",
            "24000/1001",
            "30000/1001",
            "30/1",
            "50/1",
            // A 720p60 channel is an ordinary source; the old 30fps ceiling
            // silently forced a full re-encode of it.
            "60000/1001",
            "60/1",
        ] {
            assert!(conservative_frame_rate(Some(rate)), "{rate}");
        }
        for rate in [
            "0/0",
            "0/1",
            "30/0",
            "120/1",
            "120000/1001",
            "NaN",
            "30",
            "-1/1",
            "1/1/1",
            "18446744073709551616/1",
            "1/",
        ] {
            assert!(!conservative_frame_rate(Some(rate)), "{rate}");
        }
        assert!(!conservative_frame_rate(None));
    }
    #[test]
    fn private_egress_is_an_input_option_not_an_upstream_header() {
        let mut cmd = Command::new("ffmpeg");
        input_args(
            &mut cmd,
            "x-viptv-egress-proxy: http://warp:8899\r\nUser-Agent: VIPTV\r\n",
        );
        let args = cmd
            .as_std()
            .get_args()
            .map(|v| v.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(args
            .windows(2)
            .any(|v| v == ["-http_proxy", "http://warp:8899"]));
        assert!(args
            .windows(2)
            .any(|v| v == ["-headers", "User-Agent: VIPTV\r\n"]));
        assert!(!args.iter().any(|v| v.contains("x-viptv-egress-proxy")));
    }
    #[test]
    fn input_protocols_allow_network_crypto_but_not_local_files() {
        let mut cmd = Command::new("ffmpeg");
        input_args(&mut cmd, "");
        let args: Vec<_> = cmd
            .as_std()
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect();
        let whitelist = args[args
            .iter()
            .position(|arg| *arg == "-protocol_whitelist")
            .unwrap()
            + 1];
        assert_eq!(whitelist, "http,https,httpproxy,tcp,tls,crypto");
        for (key, value) in [
            ("-reconnect", "1"),
            ("-reconnect_streamed", "1"),
            ("-reconnect_delay_max", "2"),
            ("-rw_timeout", "10000000"),
        ] {
            let index = args.iter().position(|arg| *arg == key).unwrap();
            assert_eq!(args[index + 1], value);
        }
        assert!(!args.contains(&"-reconnect_at_eof"));
        assert!(!args.contains(&"-reconnect_on_http_error"));
        assert!(!whitelist
            .split(',')
            .any(|p| matches!(p, "file" | "pipe" | "concat" | "subfile")));
    }
    #[test]
    fn remux_requires_known_compatible_streams() {
        let good = r#"{"streams":[{"codec_type":"video","codec_name":"h264","width":1280,"height":720,"pix_fmt":"yuv420p","profile":"High","level":40,"avg_frame_rate":"30000/1001","r_frame_rate":"30000/1001"},{"codec_type":"audio","codec_name":"aac","profile":"LC","channels":2}]}"#;
        let probe: Probe = serde_json::from_str(good).unwrap();
        assert!(probe.compatible(1280, 720));
        assert!(probe.compatible_video(1280, 720, 40));
        assert!(probe.compatible_audio_stream(
            probe
                .streams
                .iter()
                .find(|stream| stream.codec_type.as_deref() == Some("audio"))
        ));
        assert!(!probe.compatible(640, 480));
        let multichannel: Probe =
            serde_json::from_str(&good.replace("\"channels\":2", "\"channels\":6")).unwrap();
        let multichannel_audio = multichannel
            .streams
            .iter()
            .find(|stream| stream.codec_type.as_deref() == Some("audio"));
        assert!(multichannel.compatible_video(1280, 720, 40));
        assert!(!multichannel.compatible_audio_stream(multichannel_audio));
        assert!(!multichannel.compatible(1280, 720));
        let level_41: Probe =
            serde_json::from_str(&good.replace("\"level\":40", "\"level\":41")).unwrap();
        // A 720p source at level 4.1 is the common live case and must be copied.
        assert!(level_41.compatible(1280, 720));
        assert!(level_41.compatible(1920, 1080));
        // Level 5.1 is the modern browser ceiling and now copies as-is; only
        // levels beyond it were authored for hardware beyond that baseline.
        let level_51: Probe =
            serde_json::from_str(&good.replace("\"level\":40", "\"level\":51")).unwrap();
        assert!(level_51.compatible(1280, 720));
        assert!(level_51.compatible(1920, 1080));
        let level_52: Probe =
            serde_json::from_str(&good.replace("\"level\":40", "\"level\":52")).unwrap();
        assert!(!level_52.compatible(1280, 720));
        assert!(!level_52.compatible(1920, 1080));
        for bad in [
            good.replace("h264", "hevc"),
            good.replace("yuv420p", "yuv420p10le"),
            good.replace("\"channels\":2", "\"channels\":6"),
            good.replace("\"level\":40", "\"level\":52"),
            // Above the copyable rate, not merely above 30fps: a 720p60 channel
            // is now copied, so 120fps is the case that must still convert.
            good.replace(
                "\"avg_frame_rate\":\"30000/1001\"",
                "\"avg_frame_rate\":\"120/1\"",
            ),
            good.replace(
                "\"r_frame_rate\":\"30000/1001\"",
                "\"r_frame_rate\":\"120000/1001\"",
            ),
            good.replace(",\"avg_frame_rate\":\"30000/1001\"", ""),
            good.replace(",\"r_frame_rate\":\"30000/1001\"", ""),
        ] {
            assert!(!serde_json::from_str::<Probe>(&bad)
                .unwrap()
                .compatible(1920, 1080));
        }
        assert!(!serde_json::from_str::<Probe>(r#"{"streams":[]}"#)
            .unwrap()
            .compatible(1280, 720));
    }
    #[tokio::test]
    async fn startup_cleanup_only_removes_generated_session_shapes_once() {
        let root = tempfile::tempdir().unwrap();
        let stale = root.path().join(Uuid::new_v4().to_string());
        let empty = root.path().join(Uuid::new_v4().to_string());
        let unrelated = root.path().join(Uuid::new_v4().to_string());
        let named = root.path().join("not-a-session");
        for dir in [&stale, &empty, &unrelated, &named] {
            tokio::fs::create_dir(dir).await.unwrap();
        }
        for file in [
            "index.m3u8",
            "index.m3u8.tmp",
            "segment-000000001.ts",
            "segment-000000002.ts.tmp",
        ] {
            tokio::fs::write(stale.join(file), b"fixture")
                .await
                .unwrap();
        }
        tokio::fs::write(unrelated.join("precious.txt"), b"keep")
            .await
            .unwrap();
        tokio::fs::write(named.join("index.m3u8"), b"keep")
            .await
            .unwrap();
        #[cfg(unix)]
        let links = {
            let outside = tempfile::tempdir().unwrap();
            std::fs::write(outside.path().join("index.m3u8"), b"keep").unwrap();
            let directory_link = root.path().join(Uuid::new_v4().to_string());
            std::os::unix::fs::symlink(outside.path(), &directory_link).unwrap();
            let with_link = root.path().join(Uuid::new_v4().to_string());
            std::fs::create_dir(&with_link).unwrap();
            std::os::unix::fs::symlink(
                outside.path().join("index.m3u8"),
                with_link.join("index.m3u8"),
            )
            .unwrap();
            (outside, directory_link, with_link)
        };
        let manager = PlaybackManager::new(Config {
            ffmpeg: "ffmpeg".into(),
            ffprobe: "ffprobe".into(),
            root: root.path().into(),
            max_sessions: 1,
            ttl: Duration::from_secs(60),
        });
        manager.initialize().await.unwrap();
        assert!(!stale.exists() && !empty.exists());
        assert!(unrelated.join("precious.txt").exists() && named.join("index.m3u8").exists());
        #[cfg(unix)]
        assert!(
            links.0.path().join("index.m3u8").exists()
                && links.1.exists()
                && links.2.join("index.m3u8").exists()
        );
        let fresh = root.path().join(Uuid::new_v4().to_string());
        tokio::fs::create_dir(&fresh).await.unwrap();
        manager.initialize().await.unwrap();
        assert!(
            fresh.exists(),
            "initialization must never sweep an active generation twice"
        );
        manager.shutdown().await;
    }
    #[tokio::test]
    async fn capabilities_expiry_and_cleanup_are_enforced() {
        let root = tempfile::tempdir().unwrap();
        let manager = PlaybackManager::new(Config {
            ffmpeg: "ffmpeg".into(),
            ffprobe: "ffprobe".into(),
            root: root.path().into(),
            max_sessions: 1,
            ttl: Duration::from_secs(60),
        });
        let dir = root.path().join("session");
        tokio::fs::create_dir(&dir).await.unwrap();
        tokio::fs::write(dir.join("index.m3u8"), b"#EXTM3U\n")
            .await
            .unwrap();
        let permit = manager.slots.clone().try_acquire_owned().unwrap();
        manager.sessions.lock().await.insert(
            "id".into(),
            Session {
                _source_probe: None,
                direct: None,
                capability: "capability".into(),
                dir: dir.clone(),
                child: None,
                touched: Instant::now(),
                stable_target_duration: false,
                supervised_live: false,
                permits: Arc::new(InputPermits {
                    _playback: permit,
                    _provider: None,
                }),
                cleanup_tasks: manager.cleanup_tasks.clone(),
            },
        );
        assert_eq!(manager.active_count().await, 1);
        assert_eq!(manager.active_ids().await, vec!["id".to_owned()]);
        assert!(manager.slots.clone().try_acquire_owned().is_err());
        assert!(manager.serve("id", "wrong", "index.m3u8").await.is_err());
        assert!(manager
            .serve("id", "capability", "../index.m3u8")
            .await
            .is_err());
        let (mime, bytes) = manager
            .serve("id", "capability", "index.m3u8")
            .await
            .unwrap();
        assert_eq!(mime, "application/vnd.apple.mpegurl");
        assert_eq!(bytes, b"#EXTM3U\n");
        assert!(manager.heartbeat("id").await);
        manager.sessions.lock().await.get_mut("id").unwrap().touched =
            Instant::now() - Duration::from_secs(61);
        assert!(!manager.heartbeat("id").await);
        assert!(manager
            .serve("id", "capability", "index.m3u8")
            .await
            .is_err());
        manager.reap().await;
        assert_eq!(manager.active_count().await, 0);
        assert!(!dir.exists());
        assert!(manager.slots.clone().try_acquire_owned().is_ok());
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn shutdown_reaps_children_and_cancelled_start_artifacts() {
        let root = tempfile::tempdir().unwrap();
        let manager = PlaybackManager::new(Config {
            ffmpeg: "ffmpeg".into(),
            ffprobe: "ffprobe".into(),
            root: root.path().into(),
            max_sessions: 2,
            ttl: Duration::from_secs(60),
        });
        let provider_slots = Arc::new(Semaphore::new(2));
        let mut pids = Vec::new();
        let mut dirs = Vec::new();
        for index in 0..2 {
            let dir = root.path().join(format!("session-{index}"));
            tokio::fs::create_dir(&dir).await.unwrap();
            tokio::fs::write(dir.join("index.m3u8"), b"#EXTM3U\n")
                .await
                .unwrap();
            let child = Command::new("/bin/sleep")
                .arg("60")
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            pids.push(child.id().unwrap());
            dirs.push(dir.clone());
            let session = Session {
                _source_probe: None,
                direct: None,
                capability: "capability".into(),
                dir,
                child: Some(child),
                touched: Instant::now(),
                stable_target_duration: false,
                supervised_live: false,
                permits: Arc::new(InputPermits {
                    _playback: manager.slots.clone().try_acquire_owned().unwrap(),
                    _provider: Some(provider_slots.clone().try_acquire_owned().unwrap()),
                }),
                cleanup_tasks: manager.cleanup_tasks.clone(),
            };
            if index == 0 {
                manager.sessions.lock().await.insert("id".into(), session);
            } else {
                drop(session);
            } // Simulate a cancelled startup request.
        }
        manager.shutdown().await;
        assert_eq!(manager.active_count().await, 0);
        assert_eq!(provider_slots.available_permits(), 2);
        for dir in dirs {
            assert!(!dir.exists());
        }
        for pid in pids {
            assert!(!PathBuf::from(format!("/proc/{pid}")).exists());
        }
        assert!(manager
            .start(
                "https://example.com/movie".into(),
                HashMap::new(),
                0.0,
                None,
                false
            )
            .await
            .unwrap_err()
            .contains("shutting down"));
        manager.shutdown().await; // Idempotent.
    }
    #[tokio::test]
    #[ignore = "requires a real ffprobe binary"]
    async fn real_ffprobe_rejects_nested_local_segments_and_aes_keys() {
        let ffprobe =
            PathBuf::from(std::env::var("VIPTV_TEST_FFPROBE").expect("VIPTV_TEST_FFPROBE"));
        for encrypted in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let contents = if encrypted {
                format!("#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-KEY:METHOD=AES-128,URI=\"file:///viptv-protocol-test.ts\",IV=0x00000000000000000000000000000001\n#EXTINF:4,\n{base}/segment.ts\n#EXT-X-ENDLIST\n")
            } else {
                "#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4,\nfile:///viptv-protocol-test.ts\n#EXT-X-ENDLIST\n"
                    .to_owned()
            };
            let router = axum::Router::new()
                .route(
                    "/index.m3u8",
                    axum::routing::get(move || {
                        let contents = contents.clone();
                        async move { contents }
                    }),
                )
                .route(
                    "/segment.ts",
                    axum::routing::get(|| async { vec![0u8; 188] }),
                );
            let server = tokio::spawn(async move {
                axum::serve(listener, router).await.unwrap();
            });
            let mut cmd = Command::new(&ffprobe);
            cmd.kill_on_drop(true)
                .args(["-v", "error", "-allowed_extensions", "ALL"]);
            input_args(&mut cmd, "");
            cmd.args(["-show_streams", "-i"])
                .arg(format!("{base}/index.m3u8"));
            let result = timeout(Duration::from_secs(10), cmd.output()).await;
            server.abort();
            let _ = server.await;
            let output = result.unwrap().unwrap();
            let diagnostic = String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success());
            assert!(
                diagnostic.contains("not on whitelist") && diagnostic.contains("file"),
                "local protocol should be blocked (AES={encrypted}): {diagnostic}"
            );
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum HttpBodyFault {
        None,
        Once,
        Permanent,
        IgnoreRange,
        Forbidden,
    }

    struct BodyFaultServer {
        url: String,
        trace: Arc<std::sync::Mutex<Vec<(usize, usize, bool)>>>,
        task: tokio::task::JoinHandle<()>,
        cut: usize,
    }
    impl Drop for BodyFaultServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn body_fault_server(
        bytes: Arc<Vec<u8>>,
        moov: usize,
        mode: HttpBodyFault,
    ) -> BodyFaultServer {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/fixture.mp4", listener.local_addr().unwrap());
        let trace = Arc::new(std::sync::Mutex::new(Vec::new()));
        let state = trace.clone();
        let cut = moov + (bytes.len() - moov) / 2;
        let failed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((mut socket,_)) = accepted else { break; };
                        let bytes = bytes.clone();
                        let state = state.clone();
                        let failed = failed.clone();
                        connections.spawn(async move {
                            let mut request = Vec::new();
                            while !request.ends_with(b"\r\n\r\n") && request.len()<8192 {
                                let mut byte = [0];
                                match timeout(Duration::from_secs(3),socket.read(&mut byte)).await {
                                    Ok(Ok(1)) => request.push(byte[0]),
                                    _ => return,
                                }
                            }
                            let request = String::from_utf8_lossy(&request);
                            let range = request.lines().find_map(|line| {
                                let (name,value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("range").then_some(value.trim())
                            });
                            let start = range.and_then(|value| value.strip_prefix("bytes="))
                                .and_then(|value| value.split('-').next()).and_then(|value| value.parse::<usize>().ok()).unwrap_or(0);
                            if mode == HttpBodyFault::Forbidden {
                                state.lock().unwrap().push((start,0,false));
                                let _ = socket.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                                return;
                            }
                            if start >= bytes.len() {
                                let _ = socket.write_all(b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                                return;
                            }
                            let already_failed = failed.load(std::sync::atomic::Ordering::SeqCst);
                            let ignore = mode == HttpBodyFault::IgnoreRange && already_failed && start >= cut;
                            let fault = start >= moov && mode != HttpBodyFault::None && !ignore &&
                                (mode == HttpBodyFault::Permanent || !failed.swap(true,std::sync::atomic::Ordering::SeqCst));
                            let send_start = if ignore {0} else {start};
                            let end = if fault {cut.max(start)} else {bytes.len()};
                            state.lock().unwrap().push((start,end-send_start,fault));
                            let response = if range.is_some() && !ignore {
                                format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{}/{}\r\n",bytes.len()-1,bytes.len())
                            } else {"HTTP/1.1 200 OK\r\n".to_owned()};
                            let header = format!("{response}Content-Type: video/mp4\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",bytes.len()-send_start);
                            if socket.write_all(header.as_bytes()).await.is_ok() {
                                let _ = socket.write_all(&bytes[send_start..end]).await;
                            }
                            let _ = socket.shutdown().await;
                        });
                    }
                    Some(_) = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        });
        BodyFaultServer {
            url,
            trace,
            task,
            cut,
        }
    }

    async fn tail_moov_fixture(root: &std::path::Path, ffmpeg: &str) -> (Arc<Vec<u8>>, usize) {
        let path = root.join("tail-moov.mp4");
        let output = timeout(
            Duration::from_secs(20),
            Command::new(ffmpeg)
                .kill_on_drop(true)
                .args([
                    "-v",
                    "error",
                    "-nostdin",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=640x360:rate=25",
                    "-t",
                    "6",
                    "-an",
                    "-c:v",
                    "libx264",
                    "-threads",
                    "2",
                    "-preset",
                    "ultrafast",
                    "-g",
                    "25",
                    "-bf",
                    "0",
                ])
                .arg(&path)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(output.status.success(), "fixture generation failed");
        let bytes = tokio::fs::read(path).await.unwrap();
        let mut position = 0;
        let mut moov = None;
        while position + 8 <= bytes.len() {
            let size =
                u32::from_be_bytes(bytes[position..position + 4].try_into().unwrap()) as usize;
            if &bytes[position + 4..position + 8] == b"moov" {
                moov = Some(position);
                break;
            }
            assert!(size >= 8);
            position += size;
        }
        let moov = moov.expect("tail moov");
        assert!(moov > 32768 && moov > bytes.len() / 2);
        (Arc::new(bytes), moov)
    }

    fn recovery_fixture_args(command: &mut Command, reconnect: bool) {
        if reconnect {
            input_args(command, "");
        } else {
            // Control group: the production flags before HTTP recovery was added.
            command.args([
                "-protocol_whitelist",
                "http,https,httpproxy,tcp,tls,crypto",
                "-rw_timeout",
                "10000000",
            ]);
        }
    }

    #[tokio::test]
    #[ignore = "requires configured real FFmpeg/ffprobe; local raw HTTP fault fixture only"]
    async fn real_http_premature_body_recovery() {
        let ffmpeg = std::env::var("VIPTV_TEST_FFMPEG").unwrap();
        let ffprobe = std::env::var("VIPTV_TEST_FFPROBE").unwrap();
        let root = tempfile::tempdir().unwrap();
        let (bytes, moov) = tail_moov_fixture(root.path(), &ffmpeg).await;
        for reconnect in [false, true] {
            let server = body_fault_server(bytes.clone(), moov, HttpBodyFault::Once).await;
            let mut command = Command::new(&ffprobe);
            command.kill_on_drop(true).args(["-v", "warning"]);
            recovery_fixture_args(&mut command, reconnect);
            command
                .args(["-show_streams", "-of", "json", "-i"])
                .arg(&server.url);
            let output = timeout(Duration::from_secs(12), command.output())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                output.status.success(),
                reconnect,
                "one-shot premature-body outcome"
            );
            let trace = server.trace.lock().unwrap();
            assert!(
                trace.iter().any(|entry| entry.2),
                "fault must actually trigger"
            );
            if reconnect {
                assert!(
                    trace.iter().any(|entry| entry.0 == server.cut),
                    "resume must request exact missing byte"
                );
                assert!(
                    String::from_utf8_lossy(&output.stderr).contains("Will reconnect"),
                    "actual reconnect required"
                );
                let probe: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(probe["streams"][0]["codec_name"], "h264");
            }
            eprintln!(
                "HTTP_RECOVERY probe reconnect={reconnect} success={} requests={}",
                output.status.success(),
                trace.len()
            );
        }
        for (mode, seconds) in [(HttpBodyFault::Once, "3"), (HttpBodyFault::None, "10")] {
            let server = body_fault_server(bytes.clone(), moov, mode).await;
            let mut command = Command::new(&ffmpeg);
            command
                .kill_on_drop(true)
                .args(["-v", "warning", "-nostdin", "-nostats"]);
            recovery_fixture_args(&mut command, true);
            command.args(["-re", "-i"]).arg(&server.url).args([
                "-map",
                "0:v:0",
                "-t",
                seconds,
                "-c",
                "copy",
                "-progress",
                "pipe:1",
                "-f",
                "null",
                "-",
            ]);
            let started = Instant::now();
            let output = timeout(Duration::from_secs(12), command.output())
                .await
                .unwrap()
                .unwrap();
            assert!(output.status.success(), "paced copy must complete");
            let progress = String::from_utf8(output.stdout).unwrap();
            let times: Vec<i64> = progress
                .lines()
                .filter_map(|line| line.strip_prefix("out_time_us="))
                .filter_map(|value| value.parse().ok())
                .collect();
            assert!(
                times.windows(2).all(|pair| pair[0] <= pair[1]),
                "timestamps must not rewind"
            );
            let last = *times.last().unwrap();
            if mode == HttpBodyFault::Once {
                assert!(last >= 2_900_000);
                assert!(String::from_utf8_lossy(&output.stderr).contains("Will reconnect"));
                assert!(server
                    .trace
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|entry| entry.0 == server.cut));
            } else {
                assert!(
                    (5_800_000..=6_100_000).contains(&last),
                    "natural EOF must end at fixture duration"
                );
                assert!(started.elapsed() < Duration::from_secs(9));
                assert!(!String::from_utf8_lossy(&output.stderr).contains("Will reconnect"));
            }
            eprintln!(
                "HTTP_RECOVERY paced fault={} final_us={last} elapsed={:.3}",
                mode == HttpBodyFault::Once,
                started.elapsed().as_secs_f64()
            );
        }
        for mode in [
            HttpBodyFault::Permanent,
            HttpBodyFault::IgnoreRange,
            HttpBodyFault::Forbidden,
        ] {
            let server = body_fault_server(bytes.clone(), moov, mode).await;
            let mut command = Command::new(&ffprobe);
            command.kill_on_drop(true).args(["-v", "warning"]);
            recovery_fixture_args(&mut command, true);
            command
                .args(["-show_streams", "-of", "json", "-i"])
                .arg(&server.url);
            let started = Instant::now();
            let output = timeout(Duration::from_secs(12), command.output())
                .await
                .unwrap()
                .unwrap();
            assert!(
                !output.status.success(),
                "permanent/ignored range/auth must fail closed"
            );
            assert!(started.elapsed() < Duration::from_secs(10));
            if mode == HttpBodyFault::Forbidden {
                assert_eq!(server.trace.lock().unwrap().len(), 1, "no HTTP auth retry");
            }
            eprintln!(
                "HTTP_RECOVERY failure case={} bounded_elapsed={:.3}",
                if mode == HttpBodyFault::Permanent {
                    "permanent"
                } else if mode == HttpBodyFault::IgnoreRange {
                    "ignored_range"
                } else {
                    "forbidden"
                },
                started.elapsed().as_secs_f64()
            );
        }
    }

    #[test]
    fn hdr_resize_precedes_expensive_float_filters() {
        let filter = hdr_filter(1280, 720);
        assert!(filter.starts_with("zscale=transfer=linear:npl=100,format=gbrpf32le,zscale=w='trunc(min(iw,min(1280,iw*720/ih))/2)*2':h='trunc(min(ih,min(720,ih*1280/iw))/2)*2'"));
        assert!(filter.find("format=gbrpf32le").unwrap() < filter.find("w=").unwrap());
        assert!(filter.find("w=").unwrap() < filter.find("primaries=bt709").unwrap());
        assert!(filter.find("format=gbrpf32le").unwrap() < filter.find("primaries=bt709").unwrap());
        assert!(
            filter.find("primaries=bt709").unwrap()
                < filter.find("tonemap=tonemap=mobius").unwrap()
        );
        assert!(filter.ends_with("format=yuv420p,sidedata=mode=delete,setsar=1"));
        assert!(!filter.contains("force_original_aspect_ratio"));
    }

    fn hdr_fixture_transform(transfer: &str) -> String {
        format!("zscale=transferin=bt709:primariesin=bt709:matrixin=bt709:transfer={transfer}:primaries=bt2020:matrix=bt2020nc:npl=100,format=yuv420p10le")
    }

    async fn hdr_test_command(command: &mut Command) -> std::process::Output {
        command.kill_on_drop(true);
        let output = timeout(Duration::from_secs(45), command.output())
            .await
            .expect("bounded FFmpeg test")
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    #[tokio::test]
    #[ignore = "requires configured real FFmpeg zscale/tonemap; no network"]
    async fn real_hdr_resize_linear_reference() {
        let ffmpeg = std::env::var("VIPTV_TEST_FFMPEG").unwrap();
        for transfer in ["smpte2084", "arib-std-b67"] {
            for fixture in [
                "nullsrc=s=128x96:r=1,geq=lum='if(mod(X,2),235,16)':cb=128:cr=128",
                "testsrc2=size=128x96:rate=1",
            ] {
                let production = hdr_filter(32, 24);
                let reference = format!(
                "zscale=transfer=linear:npl=100,format=gbrpf32le,zscale={}:filter=bilinear,zscale=primaries=bt709,tonemap=tonemap=mobius:desat=2,zscale=transfer=bt709:matrix=bt709:range=limited,format=yuv420p,sidedata=mode=delete,setsar=1",
                hdr_size(32, 24)
            );
                let mut pixels = Vec::new();
                for filter in [&production, &reference] {
                    let mut command = Command::new(&ffmpeg);
                    command
                        .args([
                            "-v",
                            "error",
                            "-nostdin",
                            "-filter_threads",
                            "2",
                            "-threads",
                            "2",
                            "-f",
                            "lavfi",
                            "-i",
                            fixture,
                            "-vf",
                        ])
                        .arg(format!("{},{filter}", hdr_fixture_transform(transfer)))
                        .args([
                            "-frames:v",
                            "1",
                            "-threads",
                            "2",
                            "-pix_fmt",
                            "rgb24",
                            "-f",
                            "rawvideo",
                            "pipe:1",
                        ]);
                    pixels.push(hdr_test_command(&mut command).await.stdout);
                }
                assert_eq!(pixels[0].len(), 32 * 24 * 3);
                assert_eq!(pixels[0].len(), pixels[1].len());
                let mean_error = pixels[0]
                    .iter()
                    .zip(&pixels[1])
                    .map(|(a, b)| a.abs_diff(*b) as f64)
                    .sum::<f64>()
                    / pixels[0].len() as f64;
                eprintln!(
                    "{transfer} fused vs explicit linear resize mean byte error={mean_error:.4}"
                );
                assert!(
                    mean_error <= 2.0,
                    "linear-light resize mismatch: {mean_error}"
                );
            }
        }
    }

    #[tokio::test]
    #[ignore = "requires configured real FFmpeg/ffprobe libx264 and HDR filters; no network"]
    async fn real_hdr_resize_dimensions_and_sdr_tags() {
        let ffmpeg = std::env::var("VIPTV_TEST_FFMPEG").unwrap();
        let ffprobe = std::env::var("VIPTV_TEST_FFPROBE").unwrap();
        let root = tempfile::tempdir().unwrap();
        for (index, (iw, ih, w, h, expected_w, expected_h)) in [
            (3840, 1598, 1280, 720, 1280, 532),
            (3840, 1598, 1920, 1080, 1920, 798),
            (1080, 1920, 1280, 720, 404, 720),
            (320, 240, 1280, 720, 320, 240),
        ]
        .into_iter()
        .enumerate()
        {
            let path = root.path().join(format!("hdr-{index}.mp4"));
            let transfer = if index % 2 == 0 {
                "smpte2084"
            } else {
                "arib-std-b67"
            };
            let mut command = Command::new(&ffmpeg);
            command
                .args([
                    "-v",
                    "error",
                    "-nostdin",
                    "-filter_threads",
                    "2",
                    "-threads",
                    "2",
                    "-f",
                    "lavfi",
                    "-i",
                ])
                .arg(format!("testsrc2=size={iw}x{ih}:rate=1"))
                .arg("-vf")
                .arg(format!(
                    "{},{}",
                    hdr_fixture_transform(transfer),
                    hdr_filter(w, h)
                ))
                .args([
                    "-frames:v",
                    "1",
                    "-c:v",
                    "libx264",
                    "-threads",
                    "2",
                    "-preset",
                    "ultrafast",
                    "-color_trc",
                    "bt709",
                    "-color_primaries",
                    "bt709",
                    "-colorspace",
                    "bt709",
                ])
                .arg(&path);
            hdr_test_command(&mut command).await;
            let mut command = Command::new(&ffprobe);
            command
                .args(["-v", "error", "-show_streams", "-of", "json"])
                .arg(&path);
            let output = hdr_test_command(&mut command).await;
            let data: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            let video = &data["streams"][0];
            assert_eq!(video["width"], expected_w);
            assert_eq!(video["height"], expected_h);
            assert_eq!(video["pix_fmt"], "yuv420p");
            assert_eq!(video["sample_aspect_ratio"], "1:1");
            assert_eq!(video["field_order"], "progressive");
            for field in ["color_space", "color_transfer", "color_primaries"] {
                assert_eq!(video[field], "bt709");
            }
            assert!(!video["side_data_list"].to_string().contains("Mastering"));
        }
    }

    #[tokio::test]
    #[ignore = "requires configured real FFmpeg/ffprobe libx264; no network"]
    async fn real_hdr10_file_is_delivered_as_original_to_a_webcodecs_client() {
        // End-to-end proof of the reported failure: a real, genuinely tagged HDR10
        // file must reach a client that can demux and decode it. The old policy
        // refused this source before original delivery was ever considered.
        let ffmpeg = std::env::var("VIPTV_TEST_FFMPEG").unwrap();
        let ffprobe = std::env::var("VIPTV_TEST_FFPROBE").unwrap();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("hdr10.mp4");
        let mut command = Command::new(&ffmpeg);
        command
            .args([
                "-v", "error", "-nostdin", "-filter_threads", "2", "-threads", "2",
                "-f", "lavfi", "-i", "testsrc2=size=192x108:rate=24",
                "-frames:v", "4",
                "-vf", "format=yuv420p10le,setparams=color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc",
                "-c:v", "libx264", "-preset", "ultrafast", "-profile:v", "high10",
                "-pix_fmt", "yuv420p10le",
                "-color_trc", "smpte2084", "-color_primaries", "bt2020", "-colorspace", "bt2020nc",
            ])
            .arg(&path);
        hdr_test_command(&mut command).await;

        // Inspect it exactly as the server does.
        let mut command = Command::new(&ffprobe);
        command
            .args([
                "-v",
                "error",
                "-show_streams",
                "-show_format",
                "-of",
                "json",
            ])
            .arg(&path);
        let output = hdr_test_command(&mut command).await;
        let data: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let video = &data["streams"][0];
        assert_eq!(
            video["color_transfer"], "smpte2084",
            "fixture must be real HDR10"
        );
        assert_eq!(video["color_primaries"], "bt2020");

        let probe: Probe = serde_json::from_value(data.clone()).unwrap();
        // The transfer function alone decides HDR. A fully tagged HDR10 file is
        // inside the managed envelope (it gets tone-mapped), and so is the
        // partially tagged shape: ffprobe frequently reports the transfer
        // without primaries and matrix, and that shape used to be refused
        // outright before original delivery was ever considered.
        assert!(
            probe.ensure_supported().is_ok(),
            "a completely tagged HDR source is inside the managed envelope"
        );
        let mut untagged = data.clone();
        let fields = untagged["streams"][0].as_object_mut().unwrap();
        fields.remove("color_primaries");
        fields.remove("color_space");
        let partial: Probe = serde_json::from_value(untagged).unwrap();
        assert!(
            partial.ensure_supported().is_ok(),
            "a partially tagged HDR source must not be refused outright"
        );
        // A WebCodecs client that declares HDR-capable decoders receives the
        // original file for both shapes instead of an error.
        let declared: Capabilities = serde_json::from_value(serde_json::json!({
            "direct_play": true, "h264": true, "aac": true,
            "max_width": 3840, "max_height": 2160, "hevc": true,
            "direct_files": true,
            "direct_video_codecs": ["avc", "hevc", "av1"],
            "direct_audio_codecs": ["aac", "ac3", "eac3", "dts"]
        }))
        .unwrap();
        for (label, source) in [("fully tagged", &probe), ("partially tagged", &partial)] {
            let audio = source
                .streams
                .iter()
                .find(|stream| stream.codec_type.as_deref() == Some("audio"));
            assert_eq!(
                direct_format(source, &declared, audio, &TrackSelection::default()),
                Some("mp4"),
                "{label} HDR10 must be delivered as the original container"
            );
        }
    }

    #[tokio::test]
    #[ignore = "bounded synthetic 4K benchmark with configured FFmpeg; no network"]
    async fn real_hdr_resize_benchmark() {
        let ffmpeg = std::env::var("VIPTV_TEST_FFMPEG").unwrap();
        let old = format!("zscale=transfer=linear:npl=100,format=gbrpf32le,zscale=primaries=bt709,tonemap=tonemap=mobius:desat=2,zscale=transfer=bt709:matrix=bt709:range=limited,format=yuv420p,sidedata=mode=delete,{}",scale_filter(1280,720));
        for (label, filter) in [("old-fullres", old), ("new-bounded", hdr_filter(1280, 720))] {
            let mut command = Command::new(&ffmpeg);
            command.args(["-v","error","-nostdin","-filter_threads","2","-threads","2","-f","lavfi","-i","testsrc2=size=3840x1598:rate=24","-vf"])
                .arg(format!("format=yuv420p10le,setparams=color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc,{filter}"))
                .args(["-frames:v","24","-threads","2","-f","null","-"]);
            let started = Instant::now();
            hdr_test_command(&mut command).await;
            eprintln!(
                "HDR_BENCH {label} 3840x1598 ->1280x532 24frames filter_threads=2 wall={:.3}s",
                started.elapsed().as_secs_f64()
            );
        }
    }

    #[tokio::test]
    #[ignore = "requires real FFmpeg with libx264, zscale, tonemap and bwdif"]
    async fn real_common_format_conversion() {
        let ffmpeg = PathBuf::from(std::env::var("VIPTV_TEST_FFMPEG").unwrap());
        let ffprobe = PathBuf::from(std::env::var("VIPTV_TEST_FFPROBE").unwrap());
        for kind in ["interlaced", "smpte2084", "arib-std-b67"] {
            let root = tempfile::tempdir().unwrap();
            let fixture = root.path().join("source.mp4");
            let mut generate = Command::new(&ffmpeg);
            generate.args([
                "-v",
                "error",
                "-filter_threads",
                "2",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x240:rate=60",
                "-t",
                "2",
            ]);
            if kind == "interlaced" {
                generate.args([
                    "-vf",
                    "tinterlace=mode=interleave_top",
                    "-flags",
                    "+ilme+ildct",
                    "-x264-params",
                    "tff=1",
                    "-pix_fmt",
                    "yuv420p",
                ]);
            } else {
                generate.arg("-vf").arg(format!("zscale=transferin=bt709:primariesin=bt709:matrixin=bt709:transfer={kind}:primaries=bt2020:matrix=bt2020nc:npl=100,format=yuv420p10le"));
                generate.args([
                    "-color_trc",
                    kind,
                    "-color_primaries",
                    "bt2020",
                    "-colorspace",
                    "bt2020nc",
                ]);
            }
            let status = generate
                .args([
                    "-c:v",
                    "libx264",
                    "-threads",
                    "2",
                    "-preset",
                    "ultrafast",
                    "-movflags",
                    "+faststart",
                ])
                .arg(&fixture)
                .status()
                .await
                .unwrap();
            assert!(status.success(), "generate {kind}");
            let bytes = tokio::fs::read(&fixture).await.unwrap();
            let router = axum::Router::new().route(
                "/source.mp4",
                axum::routing::get(move || {
                    let bytes = bytes.clone();
                    async move { bytes }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/source.mp4", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                axum::serve(listener, router).await.unwrap();
            });
            let manager = PlaybackManager::new(Config {
                ffmpeg: ffmpeg.clone(),
                ffprobe: ffprobe.clone(),
                root: root.path().join("media"),
                max_sessions: 1,
                ttl: Duration::from_secs(60),
            });
            let source = manager.probe(&url, "", None).await.unwrap();
            assert_eq!(source.interlaced(), kind == "interlaced");
            assert_eq!(
                source.hdr_transfer().unwrap(),
                if kind == "interlaced" {
                    None
                } else {
                    Some(kind)
                }
            );
            let response = manager
                .start(url, HashMap::new(), 0.0, None, false)
                .await
                .unwrap();
            assert_eq!(response.mode, "transcode");
            let capability = response.url.split('/').nth(3).unwrap();
            let (_, served_playlist) = manager
                .serve(&response.id, capability, "index.m3u8")
                .await
                .unwrap();
            assert!(String::from_utf8(served_playlist)
                .unwrap()
                .contains("#EXT-X-TARGETDURATION:2"));
            let dir = root.path().join("media").join(&response.id);
            let playlist = tokio::fs::read_to_string(dir.join("index.m3u8"))
                .await
                .unwrap();
            let segment = dir.join(
                playlist
                    .lines()
                    .find(|line| media_type(line) == Some("video/mp2t"))
                    .unwrap(),
            );
            let output = Command::new(&ffprobe)
                .args(["-v", "error", "-show_streams", "-of", "json"])
                .arg(&segment)
                .output()
                .await
                .unwrap();
            assert!(output.status.success());
            let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            let video = &value["streams"][0];
            assert_eq!(video["codec_name"], "h264");
            assert_eq!(video["pix_fmt"], "yuv420p");
            assert_eq!(video["field_order"], "progressive");
            assert!(conservative_frame_rate(video["r_frame_rate"].as_str()));
            if kind != "interlaced" {
                for field in ["color_transfer", "color_primaries", "color_space"] {
                    assert_eq!(video[field], "bt709");
                }
                assert!(!video["side_data_list"].to_string().contains("Mastering"));
            }
            let decoded = Command::new(&ffmpeg)
                .args(["-v", "error", "-xerror", "-i"])
                .arg(&segment)
                .args([
                    "-frames:v",
                    "1",
                    "-vf",
                    "scale=32:24",
                    "-pix_fmt",
                    "rgb24",
                    "-f",
                    "rawvideo",
                    "pipe:1",
                ])
                .output()
                .await
                .unwrap();
            assert!(decoded.status.success(), "decode {kind}");
            assert_eq!(decoded.stdout.len(), 32 * 24 * 3);
            assert!(decoded.stdout.iter().copied().max().unwrap() > 100);
            assert!(manager.stop(&response.id).await);
            assert!(!dir.exists());
            manager.shutdown().await;
            server.abort();
            let _ = server.await;
        }
    }

    /// Run explicitly with VIPTV_TEST_FFMPEG and VIPTV_TEST_FFPROBE set to binaries.
    #[tokio::test]
    #[ignore = "requires real FFmpeg/libx264 and ffprobe binaries"]
    async fn real_ffmpeg_remux_transcode_and_cleanup() {
        let ffmpeg = PathBuf::from(std::env::var("VIPTV_TEST_FFMPEG").expect("VIPTV_TEST_FFMPEG"));
        let ffprobe =
            PathBuf::from(std::env::var("VIPTV_TEST_FFPROBE").expect("VIPTV_TEST_FFPROBE"));
        let root = tempfile::tempdir().unwrap();
        let fixture = root.path().join("fixture.mp4");
        let status = Command::new(&ffmpeg)
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=320x240:rate=24",
                "-t",
                "6",
                "-c:v",
                "libx264",
                "-threads",
                "2",
                "-pix_fmt",
                "yuv420p",
                "-profile:v",
                "main",
                "-level:v",
                "3.1",
                "-g",
                "48",
                "-movflags",
                "+faststart",
            ])
            .arg(&fixture)
            .status()
            .await
            .unwrap();
        assert!(status.success());
        let bytes = tokio::fs::read(&fixture).await.unwrap();
        let router = axum::Router::new().route(
            "/fixture.mp4",
            axum::routing::get(move || {
                let bytes = bytes.clone();
                async move { bytes }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/fixture.mp4", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let media_root = root.path().join("media");
        let manager = PlaybackManager::new(Config {
            ffmpeg: ffmpeg.clone(),
            ffprobe: ffprobe.clone(),
            root: media_root.clone(),
            max_sessions: 1,
            ttl: Duration::from_secs(60),
        });
        let provider_slots = Arc::new(Semaphore::new(1));
        for (force, position) in [(false, 0.0), (true, 0.0), (false, 1.25)] {
            let caps = if force {
                Capabilities {
                    max_width: 200,
                    max_height: 100,
                    ..Capabilities::default()
                }
            } else {
                Capabilities::default()
            };
            let response = manager
                .start_with_permit(
                    url.clone(),
                    HashMap::new(),
                    position,
                    Some(caps),
                    force,
                    false,
                    Some(provider_slots.clone().try_acquire_owned().unwrap()),
                )
                .await
                .unwrap();
            assert_eq!(provider_slots.available_permits(), 0);
            assert_eq!(
                response.mode,
                if force || position > 0.0 {
                    "transcode"
                } else {
                    "remux"
                }
            );
            assert_eq!(
                response.video_mode,
                if force || position > 0.0 {
                    "encode"
                } else {
                    "copy"
                }
            );
            assert_eq!(response.audio_mode, "none");
            assert_eq!(response.position, position);
            assert!(!response.live);
            assert!((5.0..=7.0).contains(&response.duration));
            let capability = response.url.split('/').nth(3).unwrap();
            let (_, playlist) = manager
                .serve(&response.id, capability, "index.m3u8")
                .await
                .unwrap();
            let playlist = String::from_utf8(playlist).unwrap();
            let segment = playlist
                .lines()
                .find(|line| media_type(line) == Some("video/mp2t"))
                .unwrap();
            assert!(!manager
                .serve(&response.id, capability, segment)
                .await
                .unwrap()
                .1
                .is_empty());
            let output = Command::new(&ffprobe)
                .args([
                    "-v",
                    "quiet",
                    "-show_entries",
                    "stream=width,height",
                    "-of",
                    "json",
                ])
                .arg(media_root.join(&response.id).join(segment))
                .output()
                .await
                .unwrap();
            assert!(
                output.status.success(),
                "fixture probe failed: {} {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
            let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            let width = value["streams"][0]["width"].as_u64().unwrap();
            let height = value["streams"][0]["height"].as_u64().unwrap();
            assert!(
                width.is_multiple_of(2)
                    && height.is_multiple_of(2)
                    && width <= 320
                    && height <= 240
            );
            if force {
                assert!(width <= 200 && height <= 100);
            }
            if position > 0.0 {
                async fn frame(ffmpeg: &PathBuf, path: &std::path::Path, seek: f64) -> Vec<u8> {
                    let output = Command::new(ffmpeg)
                        .args(["-v", "error", "-xerror", "-ss", &seek.to_string(), "-i"])
                        .arg(path)
                        .args([
                            "-frames:v",
                            "1",
                            "-vf",
                            "scale=32:24",
                            "-pix_fmt",
                            "rgb24",
                            "-f",
                            "rawvideo",
                            "pipe:1",
                        ])
                        .output()
                        .await
                        .unwrap();
                    assert!(
                        output.status.success(),
                        "first segment must decode independently"
                    );
                    assert_eq!(output.stdout.len(), 32 * 24 * 3);
                    output.stdout
                }
                let actual =
                    frame(&ffmpeg, &media_root.join(&response.id).join(segment), 0.0).await;
                let expected = frame(&ffmpeg, &fixture, position).await;
                let earlier_keyframe = frame(&ffmpeg, &fixture, 0.0).await;
                let error = |reference: &[u8]| {
                    actual
                        .iter()
                        .zip(reference)
                        .map(|(a, b)| (*a as f64 - *b as f64).abs())
                        .sum::<f64>()
                        / actual.len() as f64
                };
                assert!(
                    error(&expected) < 8.0,
                    "seek frame mismatch: {}",
                    error(&expected)
                );
                assert!(
                    error(&expected) < error(&earlier_keyframe),
                    "must not echo offset while starting at prior keyframe"
                );
            }
            assert!(manager.stop(&response.id).await);
            assert_eq!(provider_slots.available_permits(), 1);
            assert!(!media_root.join(&response.id).exists());
        }
        manager.shutdown().await;
        assert!(tokio::fs::read_dir(media_root)
            .await
            .unwrap()
            .next_entry()
            .await
            .unwrap()
            .is_none());
        server.abort();
        let _ = server.await;
    }

    /// A real 720p60 H.264/AAC source must be stream-copied into HLS.
    ///
    /// This is the reported live stutter: the old policy capped 720p at level 4.0
    /// and silently capped frame rate at 30, so an ordinary 720p60 channel was
    /// fully re-encoded in realtime. Encoding could not keep up, the managed HLS
    /// window underran, and FFmpeg exited and restarted in a loop.
    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "requires real FFmpeg/ffprobe binaries"]
    async fn a_720p60_source_is_stream_copied_into_hls() {
        use std::os::unix::fs::PermissionsExt;
        let ffmpeg = PathBuf::from(std::env::var("VIPTV_TEST_FFMPEG").expect("VIPTV_TEST_FFMPEG"));
        let ffprobe =
            PathBuf::from(std::env::var("VIPTV_TEST_FFPROBE").expect("VIPTV_TEST_FFPROBE"));
        let root = tempfile::tempdir().unwrap();
        let fixture = root.path().join("live720p60.mkv");
        // Level 4.1 at 59.94fps, exactly like the production live channels.
        let status = Command::new(&ffmpeg)
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=1280x720:rate=60000/1001",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000",
                "-t",
                "4",
                "-map",
                "0:v:0",
                "-map",
                "1:a:0",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-profile:v",
                "high",
                "-level:v",
                "4.1",
                "-g",
                "120",
                "-keyint_min",
                "120",
                "-sc_threshold",
                "0",
                "-force_key_frames",
                "0,2",
                "-c:a",
                "aac",
                "-ac",
                "2",
                "-shortest",
            ])
            .arg(&fixture)
            .status()
            .await
            .unwrap();
        assert!(status.success());

        let probe = Command::new(&ffprobe)
            .args(["-v", "error", "-show_streams", "-of", "json"])
            .arg(&fixture)
            .output()
            .await
            .unwrap();
        let probed: Probe = serde_json::from_slice(&probe.stdout).unwrap();
        let video = probed
            .streams
            .iter()
            .find(|s| s.codec_type.as_deref() == Some("video"))
            .unwrap();
        assert_eq!(video.level, Some(41), "fixture must be level 4.1");
        // FFmpeg builds spell the same NTSC rate differently (60000/1001 vs
        // 19001/317); assert the parsed rate, not ffprobe's rational form.
        let frame_rate = video.avg_frame_rate.as_deref().and_then(|rate| {
            let (numerator, denominator) = rate.split_once('/')?;
            match (numerator.parse::<f64>(), denominator.parse::<f64>()) {
                (Ok(numerator), Ok(denominator)) => Some(numerator / denominator),
                _ => None,
            }
        });
        assert!(
            frame_rate.is_some_and(|rate| (59.9..=60.1).contains(&rate)),
            "fixture must be ~59.94fps: {:?}",
            video.avg_frame_rate
        );
        assert!(
            video.width == Some(1280) && video.height == Some(720),
            "fixture must be 720p"
        );

        let bytes = tokio::fs::read(&fixture).await.unwrap();
        let router = axum::Router::new().route(
            "/live.mkv",
            axum::routing::get(move || {
                let bytes = bytes.clone();
                async move { bytes }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/live.mkv", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        let arguments = root.path().join("ffmpeg-arguments.txt");
        let wrapper = root.path().join("ffmpeg-wrapper.sh");
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"{}\"\nexec \"{}\" \"$@\"\n",
                arguments.display(),
                ffmpeg.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();

        let manager = PlaybackManager::new(Config {
            ffmpeg: wrapper,
            ffprobe,
            root: root.path().join("media"),
            max_sessions: 1,
            ttl: Duration::from_secs(60),
        });
        let response = manager
            .start_with_permit(url, HashMap::new(), 0.0, None, false, true, None)
            .await
            .unwrap();
        let args = tokio::fs::read_to_string(&arguments).await.unwrap();
        let args: Vec<_> = args.lines().collect();
        let value_after = |name: &str| {
            args.iter()
                .position(|arg| *arg == name)
                .map(|index| args[index + 1])
        };
        // The whole point: no video encoder runs for this source.
        assert_eq!(
            value_after("-c:v"),
            Some("copy"),
            "720p60 video must be copied"
        );
        assert_eq!(
            value_after("-c:a"),
            Some("copy"),
            "AAC-LC audio must be copied"
        );
        assert_eq!(
            response.video_mode, "copy",
            "a 720p60 channel must not be re-encoded"
        );
        assert!(
            !args.contains(&"libx264") && !args.contains(&"scale") && !args.contains(&"-vf"),
            "no encoder or filter may run for a directly compatible source: {args:?}"
        );
        assert!(manager.stop(&response.id).await);
        manager.shutdown().await;
        server.abort();
        let _ = server.await;
    }

    /// Run explicitly with VIPTV_TEST_FFMPEG and VIPTV_TEST_FFPROBE set to binaries.
    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "requires real FFmpeg/ffprobe binaries"]
    async fn compatible_video_is_copied_while_incompatible_audio_is_encoded() {
        use std::os::unix::fs::PermissionsExt;
        let ffmpeg = PathBuf::from(std::env::var("VIPTV_TEST_FFMPEG").expect("VIPTV_TEST_FFMPEG"));
        let ffprobe =
            PathBuf::from(std::env::var("VIPTV_TEST_FFPROBE").expect("VIPTV_TEST_FFPROBE"));
        let root = tempfile::tempdir().unwrap();
        let fixture = root.path().join("hybrid.mkv");
        let status = Command::new(&ffmpeg)
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=320x240:rate=24",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=880:sample_rate=48000",
                "-t",
                "6",
                "-map",
                "0:v:0",
                "-map",
                "1:a:0",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-profile:v",
                "high",
                "-level:v",
                "4.0",
                "-g",
                "240",
                "-keyint_min",
                "240",
                "-sc_threshold",
                "0",
                "-force_key_frames",
                "0,2,5",
                "-c:a",
                "ac3",
                "-ac",
                "2",
                "-metadata:s:a:0",
                "language=eng",
                "-shortest",
            ])
            .arg(&fixture)
            .status()
            .await
            .unwrap();
        assert!(status.success());
        let bytes = tokio::fs::read(&fixture).await.unwrap();
        let router = axum::Router::new().route(
            "/hybrid.mkv",
            axum::routing::get(move || {
                let bytes = bytes.clone();
                async move { bytes }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/hybrid.mkv", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let arguments = root.path().join("ffmpeg-arguments.txt");
        let wrapper = root.path().join("ffmpeg-wrapper.sh");
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"{}\"\nexec \"{}\" \"$@\"\n",
                arguments.display(),
                ffmpeg.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
        let manager = PlaybackManager::new(Config {
            ffmpeg: wrapper,
            ffprobe,
            root: root.path().join("media"),
            max_sessions: 1,
            ttl: Duration::from_secs(60),
        });
        let response = manager
            .start_with_permit(url, HashMap::new(), 0.0, None, false, false, None)
            .await
            .unwrap();
        assert_eq!(response.mode, "transcode");
        assert_eq!(response.video_mode, "copy");
        assert_eq!(response.audio_mode, "encode");
        let args = tokio::fs::read_to_string(&arguments).await.unwrap();
        let args: Vec<_> = args.lines().collect();
        let value_after = |name: &str| {
            args.iter()
                .position(|arg| *arg == name)
                .map(|index| args[index + 1])
        };
        assert_eq!(value_after("-c:v"), Some("copy"));
        assert_eq!(value_after("-c:a"), Some("aac"));
        assert!(!args.contains(&"libx264"));
        assert_eq!(value_after("-hls_init_time"), Some("1"));
        assert!(value_after("-hls_flags").is_some_and(|flags| flags.contains("split_by_time")));
        assert!(!args.contains(&"-threads"));
        assert!(!args.contains(&"-filter_threads"));
        let playlist_path = root
            .path()
            .join("media")
            .join(&response.id)
            .join("index.m3u8");
        let disk_playlist = timeout(Duration::from_secs(10), async {
            loop {
                if let Ok(text) = tokio::fs::read_to_string(&playlist_path).await {
                    if text.contains("#EXT-X-ENDLIST") {
                        return text;
                    }
                }
                sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("variable-GOP copy must finish");
        let durations: Vec<f64> = disk_playlist
            .lines()
            .filter_map(|line| line.strip_prefix("#EXTINF:"))
            .filter_map(|value| value.trim_end_matches(',').parse().ok())
            .collect();
        assert!(durations.len() >= 3, "{disk_playlist}");
        assert!(
            durations.iter().copied().fold(0.0, f64::max) <= 2.1,
            "{durations:?}"
        );
        let capability = response.url.split('/').nth(3).unwrap();
        let served = String::from_utf8(
            manager
                .serve(&response.id, capability, "index.m3u8")
                .await
                .unwrap()
                .1,
        )
        .unwrap();
        assert!(served.contains("#EXT-X-TARGETDURATION:2"), "{served}");
        let decoded = Command::new(&ffmpeg)
            .args(["-v", "error", "-xerror", "-i"])
            .arg(&playlist_path)
            .args(["-map", "0:v:0", "-map", "0:a:0", "-f", "null", "-"])
            .output()
            .await
            .unwrap();
        assert!(
            decoded.status.success(),
            "{}",
            String::from_utf8_lossy(&decoded.stderr)
        );
        assert!(manager.stop(&response.id).await);
        manager.shutdown().await;
        server.abort();
        let _ = server.await;
    }

    #[test]
    fn continuation_audio_language_is_verified_and_manual_index_overrides() {
        let probe: Probe = serde_json::from_value(serde_json::json!({"streams":[
            {"index":1,"codec_type":"audio","tags":{"language":"jpn"},"disposition":{"default":1}},
            {"index":2,"codec_type":"audio","tags":{"language":"eng"}},
            {"index":3,"codec_type":"audio","tags":{"language":"eng"},"disposition":{"comment":1}}
        ]}))
        .unwrap();
        let mut selection = TrackSelection {
            audio_language: Some("en-US".into()),
            ..Default::default()
        };
        assert_eq!(
            probe.select_audio(&selection).unwrap().unwrap().index,
            Some(2)
        );
        selection.audio_language = None;
        selection.preferred_audio_language = Some("ja".into());
        assert_eq!(
            probe.select_audio(&selection).unwrap().unwrap().index,
            Some(1)
        );
        selection.preferred_audio_language = Some("fr".into());
        assert!(
            probe.select_audio(&selection).unwrap().is_some(),
            "Unavailable profile preference is best-effort, unlike strict continuation matching"
        );
        selection.audio_language = Some("ita".into());
        assert!(probe.select_audio(&selection).is_err());
        selection.audio_track_index = Some(1);
        assert_eq!(
            probe.select_audio(&selection).unwrap().unwrap().index,
            Some(1)
        );
    }

    #[test]
    fn default_main_precedes_commentary_and_reports_dispositions() {
        let mut probe:Probe=serde_json::from_value(serde_json::json!({"streams":[
            {"index":1,"codec_type":"audio","tags":{"language":"eng","title":"Commentary"},"disposition":{"default":1,"comment":1}},
            {"index":2,"codec_type":"audio","tags":{"language":"eng","title":"Main"},"disposition":{"default":0,"comment":0}},
            {"index":3,"codec_type":"audio","tags":{"language":"eng","title":"Default main"},"disposition":{"default":1,"comment":0}},
            {"index":4,"codec_type":"audio","tags":{"language":"ita"},"disposition":{"default":1,"comment":0}}
        ]})).unwrap();
        assert_eq!(
            probe
                .select_audio(&TrackSelection::default())
                .unwrap()
                .unwrap()
                .index,
            Some(3)
        );
        probe.streams.retain(|stream| stream.index != Some(3));
        assert_eq!(
            probe
                .select_audio(&TrackSelection::default())
                .unwrap()
                .unwrap()
                .index,
            Some(2),
            "English main audio outranks a different-language default"
        );
        probe.streams.retain(|stream| stream.index != Some(4));
        assert_eq!(
            probe
                .select_audio(&TrackSelection::default())
                .unwrap()
                .unwrap()
                .index,
            Some(2),
            "a clean main track must outrank default commentary"
        );
        let explicit = probe
            .select_audio(&TrackSelection {
                audio_track_index: Some(1),
                ..Default::default()
            })
            .unwrap()
            .unwrap();
        assert!(explicit.disposition.as_ref().unwrap().public().commentary);
        let value = serde_json::to_value(probe.tracks("audio")).unwrap();
        assert_eq!(value[0]["disposition"]["default"], true);
        assert_eq!(value[0]["disposition"]["commentary"], true);
        assert_eq!(value[1]["disposition"]["commentary"], false);
        assert_eq!(value[0]["disposition"]["forced"], false);
    }

    #[test]
    fn audio_selection_honors_explicit_track_then_smart_defaults() {
        let p: Probe = serde_json::from_value(serde_json::json!({"streams":[
            {"index":0,"codec_type":"video","codec_name":"h264","pix_fmt":"yuv420p","profile":"Main","level":30,"width":160,"height":90,"avg_frame_rate":"24/1","r_frame_rate":"24/1"},
            {"index":1,"codec_type":"audio","codec_name":"mp3","tags":{"language":"ita","title":"English"}},
            {"index":2,"codec_type":"audio","codec_name":"aac","profile":"LC","channels":2,"tags":{"language":"ENG"}},
            {"index":3,"codec_type":"audio","tags":{"title":"English","language":"und"}},
            {"index":4,"codec_type":"subtitle","codec_name":"subrip","tags":{"language":"eng"}}
        ]})).unwrap();
        assert_eq!(
            p.select_audio(&TrackSelection::default())
                .unwrap()
                .unwrap()
                .index,
            Some(2)
        );
        assert_eq!(
            p.select_audio(&TrackSelection {
                audio_track_index: Some(1),
                ..Default::default()
            })
            .unwrap()
            .unwrap()
            .index,
            Some(1)
        );
        assert!(p
            .select_audio(&TrackSelection {
                audio_track_index: Some(0),
                ..Default::default()
            })
            .is_err());
        assert_eq!(
            p.select_audio(&TrackSelection {
                audio_track_index: Some(3),
                ..Default::default()
            })
            .unwrap()
            .unwrap()
            .language_status(),
            "unknown"
        );
        assert!(!p.compatible(1280, 720));
        assert!(p.compatible_audio(
            1280,
            720,
            p.select_audio(&TrackSelection::default()).unwrap()
        ));
        let tracks = p.tracks("audio");
        assert!(tracks.iter().all(|track| track.selectable));
        assert_eq!(p.tracks("subtitle")[0].input_index, 4);
        assert!(p.tracks("subtitle")[0].selectable);
        assert_eq!(p.select_subtitle(Some(4)).unwrap().unwrap().index, Some(4));
        assert!(p.select_subtitle(Some(2)).is_err());
        assert!(p.select_subtitle(None).unwrap().is_none());
        let bitmap: Probe = serde_json::from_value(serde_json::json!({"streams":[{"index":4,"codec_type":"subtitle","codec_name":"hdmv_pgs_subtitle"}]})).unwrap();
        assert!(!bitmap.tracks("subtitle")[0].supported);
        assert!(bitmap
            .select_subtitle(Some(4))
            .unwrap_err()
            .contains("unsupported bitmap"));
    }

    #[tokio::test]
    #[ignore = "requires real FFmpeg/libx264 and ffprobe; loopback fixtures only"]
    async fn real_audio_selection_accepts_tagged_and_unknown_languages() {
        let ffmpeg = PathBuf::from(std::env::var("VIPTV_TEST_FFMPEG").unwrap());
        let ffprobe = PathBuf::from(std::env::var("VIPTV_TEST_FFPROBE").unwrap());
        let root = tempfile::tempdir().unwrap();
        for (case, languages) in [
            ("dual", vec!["ita", "eng"]),
            ("english", vec!["eng"]),
            ("italian", vec!["ita"]),
            ("unknown", vec!["und"]),
        ] {
            let fixture = root.path().join(format!("{case}.mp4"));
            let mut command = Command::new(&ffmpeg);
            command.args([
                "-v",
                "error",
                "-nostdin",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=160x90:rate=24",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=880:sample_rate=48000",
                "-map",
                "0:v",
            ]);
            for (i, language) in languages.iter().enumerate() {
                command.args(["-map", if *language == "ita" { "1:a" } else { "2:a" }]);
                command
                    .arg(format!("-metadata:s:a:{i}"))
                    .arg(format!("language={language}"));
            }
            command
                .args([
                    "-t",
                    "6",
                    "-c:v",
                    "libx264",
                    "-threads",
                    "2",
                    "-pix_fmt",
                    "yuv420p",
                    "-profile:v",
                    "main",
                    "-level:v",
                    "3.0",
                    "-g",
                    "24",
                    "-c:a",
                    "aac",
                    "-ac",
                    "2",
                    "-movflags",
                    "+faststart",
                ])
                .arg(&fixture);
            assert!(timeout(Duration::from_secs(30), command.status())
                .await
                .unwrap()
                .unwrap()
                .success());
            let bytes = tokio::fs::read(&fixture).await.unwrap();
            let router = axum::Router::new().route(
                "/fixture.mp4",
                axum::routing::get(move || {
                    let b = bytes.clone();
                    async move { b }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/fixture.mp4", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                axum::serve(listener, router).await.unwrap();
            });
            let media = root.path().join(format!("media-{case}"));
            let manager = PlaybackManager::new(Config {
                ffmpeg: ffmpeg.clone(),
                ffprobe: ffprobe.clone(),
                root: media.clone(),
                max_sessions: 1,
                ttl: Duration::from_secs(60),
            });
            let response = manager
                .start_with_selection(
                    url.clone(),
                    HashMap::new(),
                    0.0,
                    None,
                    false,
                    false,
                    None,
                    TrackSelection::default(),
                )
                .await
                .unwrap();
            let selected = response.selected_audio.as_ref().unwrap();
            assert_eq!(selected.input_index, if case == "dual" { 2 } else { 1 });
            assert_eq!(selected.output_index, 0);
            assert_eq!(selected.output_audio_ordinal, 0);
            assert_eq!(selected.output_stream_index, 1);
            assert!(selected.disposition.is_some());
            assert_eq!(
                selected.language_status,
                match case {
                    "italian" => "tagged_non_english",
                    "dual" | "english" => "tagged_english",
                    "unknown" => "unknown",
                    _ => unreachable!(),
                }
            );
            let capability = response.url.split('/').nth(3).unwrap();
            let (_, playlist) = manager
                .serve(&response.id, capability, "index.m3u8")
                .await
                .unwrap();
            let playlist = String::from_utf8(playlist).unwrap();
            let segment = playlist
                .lines()
                .find(|line| media_type(line) == Some("video/mp2t"))
                .unwrap();
            let output = Command::new(&ffmpeg)
                .args(["-v", "error", "-nostdin", "-i"])
                .arg(media.join(&response.id).join(segment))
                .args([
                    "-map", "0:a:0", "-t", "1", "-ac", "1", "-ar", "8000", "-f", "s16le", "pipe:1",
                ])
                .output()
                .await
                .unwrap();
            assert!(output.status.success());
            let samples: Vec<i16> = output
                .stdout
                .as_chunks::<2>()
                .0
                .iter()
                .map(|s| i16::from_le_bytes(*s))
                .collect();
            assert!(samples.len() > 4000);
            let crossings = samples.windows(2).filter(|s| s[0] <= 0 && s[1] > 0).count();
            let frequency = crossings as f64 * 8000.0 / samples.len() as f64;
            let expected_frequency = if case == "italian" { 440.0 } else { 880.0 };
            assert!(
                ((expected_frequency - 40.0)..(expected_frequency + 40.0)).contains(&frequency),
                "Wrong selected audio frequency for {case}: {frequency}"
            );
            manager.stop(&response.id).await;
            assert_eq!(manager.active_count().await, 0);
            server.abort();
        }
    }

    #[tokio::test]
    #[ignore = "requires real FFmpeg/libx264/WebVTT and ffprobe; loopback only"]
    async fn real_text_subtitle_hls_selection_seek_and_sparse_startup() {
        async fn served_media(
            manager: &PlaybackManager,
            id: &str,
            capability: &str,
            path: &str,
            expected_mime: &str,
        ) -> Vec<u8> {
            let file = path.rsplit('/').next().unwrap();
            let (mime, bytes) = manager.serve(id, capability, file).await.unwrap();
            assert_eq!(mime, expected_mime);
            bytes
        }
        let ffmpeg = PathBuf::from(std::env::var("VIPTV_TEST_FFMPEG").unwrap());
        let ffprobe = PathBuf::from(std::env::var("VIPTV_TEST_FFPROBE").unwrap());
        let root = tempfile::tempdir().unwrap();
        for late in [false, true] {
            let suffix = if late { "late" } else { "normal" };
            let captions = root.path().join(format!("{suffix}.srt"));
            tokio::fs::write(&captions,if late {"1\n00:00:30,000 --> 00:00:35,000\nLate first caption.\n"} else {"1\n00:00:00,000 --> 00:00:03,000\nEnglish fixture caption.\n\n2\n00:00:04,000 --> 00:00:07,000\nSecond caption after seek.\n"}).await.unwrap();
            let fixture = root.path().join(format!("{suffix}.mkv"));
            let status = Command::new(&ffmpeg)
                .args([
                    "-v",
                    "error",
                    "-nostdin",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=160x90:rate=24",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=880:sample_rate=48000",
                    "-i",
                ])
                .arg(&captions)
                .args([
                    "-t",
                    if late { "36" } else { "8" },
                    "-map",
                    "0:v",
                    "-map",
                    "1:a",
                    "-map",
                    "2:s",
                    "-c:v",
                    "libx264",
                    "-threads",
                    "2",
                    "-g",
                    "24",
                    "-c:a",
                    "aac",
                    "-c:s",
                    "srt",
                    "-metadata:s:a:0",
                    "language=eng",
                    "-metadata:s:s:0",
                    "language=eng",
                    "-metadata:s:s:0",
                    "title=English CC",
                ])
                .arg(&fixture)
                .status()
                .await
                .unwrap();
            assert!(status.success());
            if !late {
                let rolling = root.path().join("rolling");
                tokio::fs::create_dir(&rolling).await.unwrap();
                let mut command = Command::new(&ffmpeg);
                command
                    .args(["-v", "error", "-nostdin", "-stream_loop", "40", "-i"])
                    .arg(&fixture)
                    .args([
                        "-map",
                        "0:v",
                        "-map",
                        "0:a",
                        "-map",
                        "0:s",
                        "-c:v",
                        "copy",
                        "-c:a",
                        "copy",
                        "-c:s",
                        "webvtt",
                        "-max_interleave_delta",
                        "1000000",
                        "-hls_segment_options",
                        "mpegts_copyts=1",
                        "-f",
                        "hls",
                        "-hls_time",
                        &HLS_SEGMENT_SECONDS.to_string(),
                        "-hls_list_size",
                        &(HLS_WINDOW_SECONDS / HLS_SEGMENT_SECONDS).to_string(),
                        "-hls_delete_threshold",
                        &(HLS_DELETE_GRACE_SECONDS / HLS_SEGMENT_SECONDS).to_string(),
                        "-hls_flags",
                        "delete_segments+temp_file",
                        "-var_stream_map",
                        "v:0,a:0,s:0,sgroup:subs,language:eng",
                        "-hls_segment_filename",
                    ])
                    .arg(rolling.join("segment-%09d.ts"))
                    .arg(rolling.join("index.m3u8"));
                assert!(timeout(Duration::from_secs(30), command.status())
                    .await
                    .unwrap()
                    .unwrap()
                    .success());
                let mut entries = tokio::fs::read_dir(&rolling).await.unwrap();
                let mut total = 0;
                let mut captions = 0;
                while let Some(entry) = entries.next_entry().await.unwrap() {
                    total += 1;
                    if media_type(&entry.file_name().to_string_lossy()) == Some("text/vtt") {
                        captions += 1;
                    }
                }
                let retained =
                    (HLS_WINDOW_SECONDS + HLS_DELETE_GRACE_SECONDS) / HLS_SEGMENT_SECONDS;
                assert!(
                    total <= retained * 2 + 3 && captions > 0 && captions <= retained,
                    "VTT segments leaked past rolling retention"
                );
                assert!(cache_safe(&rolling, false).await);
            }
            let bytes = tokio::fs::read(&fixture).await.unwrap();
            let router = axum::Router::new().route(
                "/fixture.mkv",
                axum::routing::get(move || {
                    let b = bytes.clone();
                    async move { b }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/fixture.mkv", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                axum::serve(listener, router).await.unwrap();
            });
            let media = root.path().join(format!("media-{suffix}"));
            let manager = PlaybackManager::new(Config {
                ffmpeg: ffmpeg.clone(),
                ffprobe: ffprobe.clone(),
                root: media.clone(),
                max_sessions: 1,
                ttl: Duration::from_secs(60),
            });
            let provider = Arc::new(Semaphore::new(1));
            let cases = if late {
                vec![(true, 0.0)]
            } else {
                vec![(true, 0.0), (true, 2.0), (false, 2.0)]
            };
            for (enabled, position) in cases {
                let start = Instant::now();
                let response = manager
                    .start_with_selection(
                        url.clone(),
                        HashMap::new(),
                        position,
                        None,
                        false,
                        false,
                        Some(provider.clone().try_acquire_owned().unwrap()),
                        TrackSelection {
                            audio_track_index: Some(1),
                            subtitle_track_index: (enabled && position > 0.0).then_some(2),
                            preferred_subtitle_language: (enabled && position == 0.0)
                                .then(|| "en".into()),
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap();
                assert!(
                    start.elapsed() < Duration::from_secs(12),
                    "Sparse captions delayed AV startup"
                );
                assert_eq!(provider.available_permits(), 0);
                assert_eq!(response.position, position);
                assert!(response.subtitles_supported);
                let track = &response.subtitle_tracks[0];
                assert_eq!(track.title, "English CC");
                assert!(track.supported);
                assert_eq!(track.selected, enabled);
                let cap = response.url.split('/').nth(3).unwrap();
                if enabled {
                    assert!(response.url.ends_with("master.m3u8"));
                    assert_eq!(response.selected_subtitle.as_ref().unwrap().input_index, 2);
                    assert_eq!(
                        response
                            .selected_subtitle
                            .as_ref()
                            .unwrap()
                            .output_stream_index,
                        2
                    );
                    let base =
                        reqwest::Url::parse(&format!("http://client.invalid{}", response.url))
                            .unwrap();
                    let master = served_media(
                        &manager,
                        &response.id,
                        cap,
                        base.path(),
                        "application/vnd.apple.mpegurl",
                    )
                    .await;
                    let master = String::from_utf8(master).unwrap();
                    assert!(
                        master.contains("SUBTITLES=\"subs\""),
                        "Incomplete master at position {position}: {master}"
                    );
                    let quoted = master
                        .split("URI=\"")
                        .nth(1)
                        .unwrap()
                        .split('"')
                        .next()
                        .unwrap();
                    let subtitle_url = base.join(quoted).unwrap();
                    assert_eq!(
                        subtitle_url.path(),
                        format!("/media/{}/{cap}/index_vtt.m3u8", response.id)
                    );
                    let playlist = served_media(
                        &manager,
                        &response.id,
                        cap,
                        subtitle_url.path(),
                        "application/vnd.apple.mpegurl",
                    )
                    .await;
                    let playlist = String::from_utf8(playlist).unwrap();
                    let file = playlist
                        .lines()
                        .find(|line| media_type(line) == Some("text/vtt"))
                        .unwrap();
                    let vtt_url = subtitle_url.join(file).unwrap();
                    assert_eq!(
                        vtt_url.path(),
                        format!("/media/{}/{cap}/{file}", response.id)
                    );
                    let vtt =
                        served_media(&manager, &response.id, cap, vtt_url.path(), "text/vtt").await;
                    let mut vtt = String::from_utf8(vtt).unwrap();
                    // With2s segments the cue at output2s belongs to the NEXT
                    // rendition segment, not the first ready segment. Observe its
                    // publication without delaying or weakening startup readiness.
                    if !late && position > 0.0 {
                        for _ in 0..30 {
                            if vtt.contains("Second caption after seek.") {
                                break;
                            }
                            sleep(Duration::from_millis(100)).await;
                            let latest = served_media(
                                &manager,
                                &response.id,
                                cap,
                                subtitle_url.path(),
                                "application/vnd.apple.mpegurl",
                            )
                            .await;
                            let latest = String::from_utf8(latest).unwrap();
                            for name in latest
                                .lines()
                                .filter(|line| media_type(line) == Some("text/vtt"))
                            {
                                let caption_url = subtitle_url.join(name).unwrap();
                                let bytes = served_media(
                                    &manager,
                                    &response.id,
                                    cap,
                                    caption_url.path(),
                                    "text/vtt",
                                )
                                .await;
                                vtt.push_str(&String::from_utf8(bytes).unwrap());
                            }
                        }
                    }
                    assert!(vtt.starts_with("WEBVTT"));
                    assert!(vtt.contains("X-TIMESTAMP-MAP=LOCAL:00:00:00.000,MPEGTS:0"));
                    assert!(manager.serve(&response.id, "wrong", file).await.is_err());
                    if !late {
                        assert!(vtt.contains(if position > 0.0 {
                            "Second caption after seek."
                        } else {
                            "English fixture caption."
                        }));
                        if position > 0.0 {
                            assert!(
                                vtt.contains("00:02."),
                                "Caption timestamps were not rebased after seek: {vtt}"
                            );
                        }
                    }
                    let av_uri = master
                        .lines()
                        .find(|line| !line.is_empty() && !line.starts_with('#'))
                        .unwrap();
                    let av_url = base.join(av_uri).unwrap();
                    assert_eq!(
                        av_url.path(),
                        format!("/media/{}/{cap}/index.m3u8", response.id)
                    );
                    let av = served_media(
                        &manager,
                        &response.id,
                        cap,
                        av_url.path(),
                        "application/vnd.apple.mpegurl",
                    )
                    .await;
                    let av = String::from_utf8(av).unwrap();
                    let segment = av
                        .lines()
                        .find(|line| media_type(line) == Some("video/mp2t"))
                        .unwrap();
                    let probe = Command::new(&ffprobe)
                        .args([
                            "-v",
                            "error",
                            "-select_streams",
                            "v:0",
                            "-show_entries",
                            "packet=pts_time",
                            "-of",
                            "json",
                        ])
                        .arg(media.join(&response.id).join(segment))
                        .output()
                        .await
                        .unwrap();
                    assert!(probe.status.success());
                    let packets: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
                    let pts = packets["packets"][0]["pts_time"]
                        .as_str()
                        .unwrap()
                        .parse::<f64>()
                        .unwrap();
                    assert!(
                        (0.0..0.2).contains(&pts),
                        "AV/VTT clocks disagree: first video PTS {pts}"
                    );
                } else {
                    assert!(response.url.ends_with("index.m3u8"));
                    assert!(response.selected_subtitle.is_none());
                    assert!(!media.join(&response.id).join("index_vtt.m3u8").exists());
                }
                manager.stop(&response.id).await;
                assert_eq!(provider.available_permits(), 1);
                assert_eq!(manager.active_count().await, 0);
                assert!(!media.join(&response.id).exists());
            }
            server.abort();
        }
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_probe_keeps_reservation_until_child_reaped() {
        let root = tempfile::tempdir().unwrap();
        let manager = scripted_probe(root.path(), "printf '%s' $$ > \"$0.pid\"; exec sleep 60");
        let slots = Arc::new(Semaphore::new(1));
        let mut preparation = Box::pin(manager.start_with_permit(
            "http://example.com/live".into(),
            HashMap::new(),
            0.0,
            None,
            false,
            true,
            Some(slots.clone().try_acquire_owned().unwrap()),
        ));
        let probe_pid = root.path().join("probe.sh.pid");
        tokio::select! {
            result = &mut preparation => panic!("fixture must stay in probing: {result:?}"),
            _ = wait_probe_file(&probe_pid) => {}
        }
        drop(preparation);
        // No await: the cleanup owner has not run on this single-thread runtime.
        // Admission must remain closed until that owner confirms child teardown.
        assert_eq!(
            slots.available_permits(),
            0,
            "cancelled input still owns its connection"
        );
        manager.shutdown().await;
        assert_eq!(slots.available_permits(), 1);
    }

    #[tokio::test]
    async fn provider_permit_released_on_probe_failure() {
        let root = tempfile::tempdir().unwrap();
        let slots = Arc::new(Semaphore::new(1));
        let manager = PlaybackManager::new(Config {
            ffmpeg: "/missing/ffmpeg".into(),
            ffprobe: "/missing/ffprobe".into(),
            root: root.path().into(),
            max_sessions: 1,
            ttl: Duration::from_secs(60),
        });
        let result = manager
            .start_with_permit(
                "https://example.com/movie".into(),
                HashMap::new(),
                0.0,
                None,
                false,
                false,
                Some(slots.clone().try_acquire_owned().unwrap()),
            )
            .await;
        assert!(result.unwrap_err().contains("inspect"));
        assert_eq!(slots.available_permits(), 1);
        manager.shutdown().await;
    }
    #[tokio::test]
    async fn validates_before_spawning_and_redacts_errors() {
        let manager = PlaybackManager::new(Config {
            ffmpeg: "/missing/ffmpeg".into(),
            ffprobe: "/missing/ffprobe".into(),
            root: std::env::temp_dir().join(Uuid::new_v4().to_string()),
            max_sessions: 0,
            ttl: Duration::from_secs(1),
        });
        assert!(manager
            .start(
                "file:///etc/passwd".into(),
                HashMap::new(),
                0.0,
                None,
                false
            )
            .await
            .is_err());
        let error = manager
            .start(
                "https://user:secret@example.com/movie".into(),
                HashMap::new(),
                0.0,
                None,
                false,
            )
            .await
            .unwrap_err();
        assert!(!error.contains("secret"));
        assert!(manager
            .start(
                "https://example.com/movie".into(),
                HashMap::new(),
                f64::NAN,
                None,
                false
            )
            .await
            .is_err());
        assert_eq!(manager.active_count().await, 0);
        assert!(!manager.heartbeat("unknown").await);
        assert!(!manager.stop("unknown").await);
        assert!(!manager.ffmpeg_available().await);
    }
}
