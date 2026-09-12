//! Isolated physical-TV fixture. No production database, provider or credentials.
//! Bind only on a trusted test LAN; generated login material is written privately.
use axum::{
    body::{Body, Bytes},
    extract::{Path, State},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use viptv_server::{
    playback::{Config, PlaybackManager},
    router, App,
};
#[derive(Clone)]
struct Fixture {
    primary: Arc<Vec<u8>>,
    backup: Arc<Vec<u8>>,
    active: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    first: Arc<AtomicUsize>,
    second: Arc<AtomicUsize>,
    disconnected: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
    stalled: Arc<AtomicBool>,
    scenario: String,
}
struct Connected(Arc<AtomicUsize>);
impl Drop for Connected {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
async fn stream(State(f): State<Fixture>, Path(file): Path<String>) -> Response {
    let primary = file == "1.ts";
    if primary {
        f.first.fetch_add(1, Ordering::SeqCst);
    } else {
        f.second.fetch_add(1, Ordering::SeqCst);
    }
    if !primary && f.scenario == "failed-backup" {
        return axum::http::StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    if primary && f.disconnected.load(Ordering::SeqCst) {
        return axum::http::StatusCode::GONE.into_response();
    }
    let active = f.active.fetch_add(1, Ordering::SeqCst) + 1;
    f.peak.fetch_max(active, Ordering::SeqCst);
    let guard = Connected(f.active.clone());
    let bytes = if primary {
        f.primary.clone()
    } else {
        f.backup.clone()
    };
    let delay = Duration::from_secs_f64(20.0 * 1316.0 / bytes.len() as f64);
    let body = async_stream::stream! {
        let _guard=guard;
        'media: loop {
            for chunk in bytes.chunks(188*7) {
                if f.stopping.load(Ordering::SeqCst) || (primary && f.disconnected.load(Ordering::SeqCst)){break 'media;}
                while (primary && f.stalled.load(Ordering::SeqCst)) || (!primary && f.scenario=="cancel-backup") {
                    if f.stopping.load(Ordering::SeqCst) { break 'media; }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                tokio::time::sleep(delay).await;
                yield Ok::<_,std::io::Error>(Bytes::copy_from_slice(chunk));
            }
        }
    };
    ([("content-type", "video/mp2t")], Body::from_stream(body)).into_response()
}
async fn guide() -> axum::Json<Value> {
    let now = now();
    axum::Json(
        json!({"epg_listings":[{"id":"pilot","title":"Family recovery pilot","start_timestamp":now-3600,"stop_timestamp":now+3600}]}),
    )
}
async fn xmltv() -> String {
    let start = chrono::DateTime::<chrono::Utc>::from_timestamp(now() as i64 - 3600, 0).unwrap();
    let end = start + chrono::Duration::hours(24);
    format!("<tv><channel id=\"pilot-east\"><display-name>Pilot East</display-name></channel><programme channel=\"pilot-east\" start=\"{}\" stop=\"{}\"><title>Shared family playback pilot</title><desc>Persistent guide independent of the selected video input.</desc></programme></tv>",start.format("%Y%m%d%H%M%S %z"),end.format("%Y%m%d%H%M%S %z"))
}
#[derive(Clone)]
struct Observations(Arc<std::sync::Mutex<Value>>);
async fn observe(
    State(observations): State<Observations>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let path = request.uri().path().to_owned();
    let response = next.run(request).await;
    if !path.starts_with("/api/playback") {
        return response;
    }
    let (parts, body) = response.into_parts();
    let bytes = axum::body::to_bytes(body, 2 * 1024 * 1024)
        .await
        .unwrap_or_default();
    if let Ok(data) = serde_json::from_slice::<Value>(&bytes) {
        if data["managed_live"] == true {
            let playback = if data["playback"].is_object() {
                &data["playback"]
            } else {
                &data
            };
            let engine = playback["url"]
                .as_str()
                .and_then(|url| url.strip_prefix("/media/"))
                .and_then(|url| url.split('/').next());
            *observations.0.lock().unwrap() = json!({"at_ms":SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis(),"logical_id":playback["id"],"channel_id":playback["channel_id"],"generation":data["generation"],"state":data["state"],"reason":data["reason"],"mode":playback["mode"],"engine_id":engine});
        }
    }
    Response::from_parts(parts, Body::from(bytes))
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
fn private_write(path: impl AsRef<std::path::Path>, value: &Value) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(value.to_string().as_bytes())
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter("viptv_server=info")
        .init();
    let directory = PathBuf::from(std::env::var("VIPTV_PILOT_DIR")?);
    let base = std::env::var("VIPTV_PILOT_ORIGIN")?;
    std::fs::create_dir_all(&directory)?;
    let ffmpeg = PathBuf::from(std::env::var("VIPTV_TEST_FFMPEG")?);
    let ffprobe = PathBuf::from(std::env::var("VIPTV_TEST_FFPROBE")?);
    let scenario = std::env::var("VIPTV_PILOT_SCENARIO").unwrap_or_else(|_| "disconnect".into());
    for (name, color, tone) in [("primary", "blue", "440"), ("backup", "green", "660")] {
        let mixed = name == "backup" && scenario == "stall-mixed";
        let output = tokio::time::timeout(
            Duration::from_secs(30),
            tokio::process::Command::new(&ffmpeg)
                .kill_on_drop(true)
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &format!(
                        "color=c={color}:size={}:rate=30",
                        if mixed { "854x480" } else { "640x360" }
                    ),
                    "-f",
                    "lavfi",
                    "-i",
                    &format!("sine=frequency={tone}:sample_rate=48000"),
                    "-t",
                    "20",
                    "-c:v",
                    if mixed { "mpeg2video" } else { "libx264" },
                    "-threads",
                    "1",
                    "-preset",
                    "ultrafast",
                    "-pix_fmt",
                    "yuv420p",
                    "-g",
                    "30",
                    "-c:a",
                    if mixed { "ac3" } else { "aac" },
                    "-f",
                    "mpegts",
                ])
                .arg(directory.join(format!("{name}.ts")))
                .output(),
        )
        .await??;
        if !output.status.success() {
            return Err("Pilot media generation failed".into());
        }
    }
    let f = Fixture {
        primary: Arc::new(std::fs::read(directory.join("primary.ts"))?),
        backup: Arc::new(std::fs::read(directory.join("backup.ts"))?),
        active: Arc::new(AtomicUsize::new(0)),
        peak: Arc::new(AtomicUsize::new(0)),
        first: Arc::new(AtomicUsize::new(0)),
        second: Arc::new(AtomicUsize::new(0)),
        disconnected: Arc::new(AtomicBool::new(false)),
        stopping: Arc::new(AtomicBool::new(false)),
        stalled: Arc::new(AtomicBool::new(false)),
        scenario,
    };
    let upstream = tokio::net::TcpListener::bind("127.0.0.1:18090").await?;
    let upstream_router = Router::new()
        .route("/live/fixture/fixture/:file", get(stream))
        .route("/player_api.php", get(guide))
        .route("/xmltv.php", get(xmltv))
        .with_state(f.clone());
    let upstream_task = tokio::spawn(async move { axum::serve(upstream, upstream_router).await });
    let playback = PlaybackManager::new(Config {
        ffmpeg,
        ffprobe,
        root: directory.join("hls"),
        max_sessions: 2,
        ttl: Duration::from_secs(60),
    });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(50))
        .build()?;
    let app = App::new(
        rusqlite::Connection::open(directory.join("pilot.sqlite"))?,
        client.clone(),
        playback.clone(),
    )?;
    let access = uuid::Uuid::new_v4().to_string();
    let refresh = uuid::Uuid::new_v4().to_string();
    let hash = |value: &str| format!("{:x}", Sha256::digest(value.as_bytes()));
    {
        let db = app.db.lock().unwrap();
        db.execute_batch("DELETE FROM addons;
            INSERT INTO auth_accounts(id,username,name,password_hash,role,recovery_hash,created_at) VALUES(1,'family-pilot','Family Pilot','unusable','owner','unusable',0);
            INSERT INTO profiles(id,name,avatar_style,avatar_seed,presentation_complete,created_at,updated_at) VALUES(1,'Family Pilot','critters','pilot',1,0,0);
            INSERT INTO profile_owners(profile_id,account_id,created_at) VALUES(1,1,0);
            INSERT INTO auth_profiles(account_id,profile_id) VALUES(1,1);
            INSERT INTO providers(id,name,url,username,password,max_connections) VALUES(1,'Isolated pilot','http://127.0.0.1:18090','fixture','fixture',1);
            INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES('iptv:1:1',1,'1','Pilot East'),('iptv:1:2',1,'2','Pilot East');")?;
        db.execute("INSERT INTO auth_sessions(id,account_id,access_hash,refresh_hash,csrf_hash,profile_id,kind,device_name,access_expires,refresh_expires,created_at) VALUES('pilot-session',1,?1,?2,'unused-csrf',1,'browser','Isolated pilot',?3,?3,0)",rusqlite::params![hash(&access),hash(&refresh),now()+3600])?;
    }
    let listener = tokio::net::TcpListener::bind("0.0.0.0:18089").await?;
    let observations = Observations(Arc::new(std::sync::Mutex::new(Value::Null)));
    let captured = observations.clone();
    let pilot_db = app.db.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            router(app, None).layer(axum::middleware::from_fn_with_state(captured, observe)),
        )
        .await
    });
    let channel:Value=client.post("http://127.0.0.1:18089/api/lineup").bearer_auth(&access).json(&json!({"name":"Recovery Pilot East","network":"Pilot","feed":"east","market":"","category":"Pilot","number":1,"enabled":true,"candidates":[{"id":"iptv:1:1","name":"Pilot East","verified":true},{"id":"iptv:1:2","name":"Pilot East","verified":true}]})).send().await?.error_for_status()?.json().await?;
    client
        .patch("http://127.0.0.1:18089/api/lineup/settings")
        .bearer_auth(&access)
        .json(&json!({"enabled":true,"limit":100,"recovery":{"stall_seconds":10,"attempt_seconds":20,"deadline_seconds":40,"max_recoveries":2}}))
        .send()
        .await?
        .error_for_status()?;
    client
        .post("http://127.0.0.1:18089/api/guides/sources")
        .bearer_auth(&access)
        .json(&json!({"name":"Pilot guide","provider_id":1}))
        .send()
        .await?
        .error_for_status()?;
    client
        .post("http://127.0.0.1:18089/api/guides/run")
        .bearer_auth(&access)
        .send()
        .await?
        .error_for_status()?;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let state: Value = client
                .get("http://127.0.0.1:18089/api/guides")
                .bearer_auth(&access)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            if state["last_run"]["state"] == "completed" {
                break Ok::<(), reqwest::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await??;
    client.patch(format!("http://127.0.0.1:18089/api/guides/channels/{}",channel["id"].as_str().unwrap())).bearer_auth(&access).json(&json!({"source_id":1,"guide_id":"pilot-east","observed_name":"Pilot East","verified":true,"priority":0})).send().await?.error_for_status()?;
    pilot_db.lock().unwrap().execute(
        "UPDATE auth_sessions SET kind='device' WHERE id='pilot-session'",
        [],
    )?;
    private_write(
        directory.join("pilot-session.json"),
        &json!({"base":base,"auth_origin":base,"access_token":access,"refresh_token":refresh,"account_id":"1","last_profile_id":"1","auth_version":3,"expires_at":now()+3600,"channel_id":channel["id"]}),
    )?;
    println!("FAMILY_TV_PILOT_READY");
    loop {
        if directory.join("stop").exists() {
            break;
        }
        f.disconnected
            .store(directory.join("disconnect").exists(), Ordering::SeqCst);
        f.stalled
            .store(directory.join("stall").exists(), Ordering::SeqCst);
        let metrics = json!({"at":now(),"active":f.active.load(Ordering::SeqCst),"peak":f.peak.load(Ordering::SeqCst),"primary_requests":f.first.load(Ordering::SeqCst),"backup_requests":f.second.load(Ordering::SeqCst),"sessions":playback.active_count().await,"disconnected":f.disconnected.load(Ordering::SeqCst),"stalled":f.stalled.load(Ordering::SeqCst),"scenario":f.scenario,"last_playback":observations.0.lock().unwrap().clone()});
        std::fs::write(directory.join("metrics.tmp"), metrics.to_string())?;
        std::fs::rename(
            directory.join("metrics.tmp"),
            directory.join("metrics.json"),
        )?;
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    f.stopping.store(true, Ordering::SeqCst);
    playback.shutdown().await;
    server.abort();
    upstream_task.abort();
    let _ = server.await;
    let _ = upstream_task.await;
    println!(
        "FAMILY_TV_PILOT_STOPPED active={}",
        f.active.load(Ordering::SeqCst)
    );
    Ok(())
}
