use super::*;

pub struct PlaybackManager {
    pub(super) config: Config,
    pub(super) slots: Arc<Semaphore>,
    pub(super) sessions: Mutex<HashMap<String, Session>>,
    pub(super) probe_cache: Mutex<HashMap<[u8; 32], ProbeCacheEntry>>,
    pub(super) lifecycle: RwLock<()>,
    pub(super) initialized: OnceCell<()>,
    pub(super) filters: OnceCell<HashSet<String>>,
    pub(super) qsv_device: Option<PathBuf>,
    pub(super) qsv_ready: OnceCell<bool>,
    pub(super) cleanup_tasks: CleanupTasks,
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

    pub(super) async fn initialize(&self) -> Result<(), String> {
        self.initialized
            .get_or_try_init(|| cleanup_orphans(&self.config.root))
            .await
            .map(|_| ())
    }

    pub(super) async fn qsv_available(&self) -> bool {
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

    pub(super) async fn reap(&self) {
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
                        // A transport-less direct-URL session has no directory
                        // and no encoder: only its TTL retires it, and every
                        // heartbeat renews that TTL.
                        && !session.dir.as_os_str().is_empty()
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

    pub(super) async fn available_filters(&self) -> Result<&HashSet<String>, String> {
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
}
