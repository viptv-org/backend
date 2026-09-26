use std::{path::PathBuf, time::Duration};
use viptv_server::{
    playback::{Config, PlaybackManager},
    router_with_tv, App,
};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The playback engine is its own crate, so its target must be listed too:
    // without it the per-session probe_ms/engine_ready_ms/encoder lines vanish.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "viptv_server=info,viptv_playback_engine=info",
        ))
        .init();
    let database = std::env::var("VIPTV_DATABASE").unwrap_or_else(|_| "data/viptv.sqlite".into());
    if let Some(parent) = std::path::Path::new(&database).parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }
    let root =
        PathBuf::from(std::env::var("VIPTV_MEDIA_DIR").unwrap_or_else(|_| "data/hls".into()));
    let max_sessions = std::env::var("VIPTV_MAX_SESSIONS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3)
        .clamp(1, 32);
    let ttl = std::env::var("VIPTV_SESSION_TTL")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(120u64)
        .clamp(30, 3600);
    let device = |name: &str| {
        std::env::var(name)
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
    };
    let playback = PlaybackManager::new_with_hardware(
        Config {
            ffmpeg: std::env::var("VIPTV_FFMPEG")
                .unwrap_or_else(|_| "ffmpeg".into())
                .into(),
            ffprobe: std::env::var("VIPTV_FFPROBE")
                .unwrap_or_else(|_| "ffprobe".into())
                .into(),
            root,
            max_sessions,
            ttl: Duration::from_secs(ttl),
        },
        device("VIPTV_QSV_DEVICE"),
        device("VIPTV_VAAPI_DEVICE"),
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(25))
        .connect_timeout(Duration::from_secs(8))
        .user_agent("VIPTV/0.1")
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5
                || viptv_server::util::validate_url(attempt.url().as_str()).is_err()
            {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
        .build()?;
    let db = rusqlite::Connection::open(&database)?;
    #[cfg(unix)]
    if database != ":memory:" {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o600)).await?;
    }
    let app = App::new(db, client, playback.clone())?;
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args
        .first()
        .is_some_and(|argument| argument == "create-admin")
    {
        if args.len() != 3 {
            return Err(
                "usage: viptv-server create-admin USERNAME DISPLAY_NAME (password on stdin)".into(),
            );
        }
        use std::io::Read;
        let mut password = String::new();
        std::io::stdin().read_to_string(&mut password)?;
        let password = password.trim_end_matches(['\r', '\n']);
        let recovery = {
            let mut database = app.db.lock().map_err(|_| "Database lock failed")?;
            viptv_server::auth::create_owner_offline(&mut database, &args[1], &args[2], password)
                .map_err(|error| format!("admin creation failed: {}", error.1))?
        };
        println!("Owner created. Store this one-time recovery code securely: {recovery}");
        playback.shutdown().await;
        return Ok(());
    }
    if !args.is_empty() {
        return Err("unknown command".into());
    }
    // Empty values disable the mounts: server-only images keep booting clean.
    let dashboard = std::env::var_os("VIPTV_DASHBOARD_DIST")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let tv_dashboard = std::env::var_os("VIPTV_TV_DIST")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let bind = std::env::var("VIPTV_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!("VIPTV server listening");
    axum::serve(listener, router_with_tv(app, dashboard, tv_dashboard))
        .with_graceful_shutdown(async {
            #[cfg(unix)]
            {
                let mut term =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("signal handler");
                tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
            }
            #[cfg(not(unix))]
            {
                let _ = tokio::signal::ctrl_c().await;
            }
        })
        .await?;
    // Explicitly reap subprocesses and finish filesystem cleanup before Tokio exits.
    playback.shutdown().await;
    Ok(())
}
