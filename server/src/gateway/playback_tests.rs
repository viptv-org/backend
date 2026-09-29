use super::*;
use crate::{
    app_state::ResourceLease, auth, auth_integration_tests::fixture, test_support::request, App,
};
use axum::{extract::Path, http::StatusCode, Json, Router};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

struct Peer {
    task: tokio::task::JoinHandle<()>,
    starts: Arc<Mutex<Vec<String>>>,
    stops: Arc<AtomicUsize>,
    mode: Arc<AtomicUsize>,
    hold: Arc<tokio::sync::Notify>,
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn lease() -> ResourceLease {
    ResourceLease {
        policy_revision: 0,
        principal: auth::Principal::Account {
            account_id: 1,
            role: "member".into(),
            profile_id: Some(1),
            session_id: Some("s1".into()),
        },
        session_id: Some("s1".into()),
    }
}
fn source(app: &App) -> String {
    app.db.lock().unwrap().execute("INSERT OR IGNORE INTO addons(id,name,manifest_url,enabled,manifest,account_id) VALUES(1,'Fixture','https://addon.fixture.invalid/manifest.json',1,'{}',1)",[]).unwrap();
    let scoped = app.clone().with_lease(lease());
    let (sources, _) = scoped.register(
        "addon:1",
        vec![json!({"url":"http://source.fixture.invalid/movie.mp4","name":"Fixture"})],
        "movie",
    );
    sources[0]["id"].as_str().unwrap().into()
}
fn body(source: &str, id: &str, platform: &str) -> Value {
    json!({"request_id":id,"stream_id":source,"client":{"platform":platform,"can_play_direct":true,"max_width":3840,"max_height":2160,"video_codecs":["h264"],"audio_codecs":["aac"]},"position":0})
}
fn remote(id: &str) -> Value {
    json!({"id":id,"job_id":"job_fixture","status":"ready","expires_at":crate::util::now()+60,"error_code":null,"playback":{"id":id,"url":format!("/media/{id}/pgm_fixture/index.m3u8"),"format":"hls","mode":"remux","video_mode":"copy","audio_mode":"copy","position":0,"duration":600,"live":false,"audio_tracks":[],"subtitle_tracks":[],"selected_audio":null,"selected_subtitle":null,"subtitles_supported":false}})
}
async fn setup() -> (App, Peer) {
    let mut app = fixture();
    app.secret_vault = Some(Arc::new(
        crate::secret_store::Vault::from_json(
            &json!({"active":"test","keys":{"test":STANDARD.encode([7u8;32])}}).to_string(),
        )
        .unwrap(),
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    app.gateway_client = client::Client::fixture(
        format!("http://{}/", listener.local_addr().unwrap())
            .parse()
            .unwrap(),
    );
    let starts = Arc::new(Mutex::new(Vec::new()));
    let stops = Arc::new(AtomicUsize::new(0));
    let mode = Arc::new(AtomicUsize::new(0));
    let hold = Arc::new(tokio::sync::Notify::new());
    let created = starts.clone();
    let deleted = stops.clone();
    let state = mode.clone();
    let waiting = hold.clone();
    let capacity_mode = mode.clone();
    let routes=Router::new().route("/v1/capabilities",axum::routing::get(move|headers:axum::http::HeaderMap|{let mode=capacity_mode.clone();async move{
        let first=headers.get("authorization").and_then(|value|value.to_str().ok()).is_some_and(|value|value.ends_with('a'));
        let capacity=if mode.load(Ordering::SeqCst)==4 && first{0}else{2};
        Json(json!({"version":1,"ready":true,"protocols":["hls"],"namespaces":["first","second"],"scopes":["capabilities","create","read","renew","release"],"available":{"inputs":capacity,"outputs":capacity,"viewers":5}}))
    }}))
        .route("/v1/sessions",axum::routing::post(move|Json(value):Json<Value>|{let created=created.clone();let state=state.clone();let waiting=waiting.clone();async move{
            let id={let mut created=created.lock().unwrap();created.push(value["namespace"].as_str().unwrap().into());format!("viewer_{}",created.len())};
            if state.load(Ordering::SeqCst)==2 {return (StatusCode::TOO_MANY_REQUESTS,Json(json!({"error":{"code":"source_connection_limit","message":"never expose provider-private-credential"}})));}
            if state.load(Ordering::SeqCst)==3 {waiting.notified().await;}
            let mut value=remote(&id);if state.load(Ordering::SeqCst)==1 {value["playback"]["url"]=json!("https://foreign.invalid/private-path");}
            (StatusCode::CREATED,Json(value))
        }}))
        .route("/v1/sessions/:id",axum::routing::get(|Path(id):Path<String>|async move{Json(remote(&id))}).delete(move||{let deleted=deleted.clone();async move{deleted.fetch_add(1,Ordering::SeqCst);StatusCode::NO_CONTENT}}))
        .route("/v1/sessions/:id/renew",axum::routing::post(|Path(id):Path<String>|async move{Json(remote(&id))}));
    let task = tokio::spawn(async move {
        axum::serve(listener, routes).await.unwrap();
    });
    (
        app,
        Peer {
            task,
            starts,
            stops,
            mode,
            hold,
        },
    )
}
fn gateway(app: &App, namespace: &str, priority: i64) -> String {
    let value=serde_json::from_value(json!({"name":namespace,"endpoint":format!("https://{namespace}.gateway.invalid/base/"),"namespace":namespace,"priority":priority,"integration_key":format!("pgk_{}",if namespace=="first"{"a"}else{"b"}.repeat(64))})).unwrap();
    registry::register(
        &app.db.lock().unwrap(),
        app.secret_vault.as_ref().unwrap(),
        1,
        value,
    )
    .unwrap()
    .id
}
async fn settled(app: &App, id: &str) -> Value {
    settled_for(app, "member-token-1", id).await
}
async fn settled_for(app: &App, token: &str, id: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (status, value) = request(
                app,
                token,
                "GET",
                &format!("/api/v2/playback/{id}"),
                Value::Null,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{value}");
            if value["status"] != "starting" {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn playback_never_falls_back_to_another_accounts_gateway_without_a_grant() {
    let (app, peer) = setup().await;
    let gateway = gateway(&app, "first", 0);
    app.db.lock().unwrap().execute_batch("INSERT INTO profiles(id,name,avatar_seed,presentation_complete) VALUES(2,'Second','two',1); INSERT INTO profile_owners VALUES(2,2,0); INSERT INTO auth_profiles VALUES(2,2); UPDATE auth_sessions SET profile_id=2 WHERE account_id=2; INSERT INTO addons(id,name,manifest_url,enabled,manifest,account_id) VALUES(2,'Second','https://second.fixture.invalid/manifest.json',1,'{}',2);").unwrap();
    let lease = ResourceLease {
        policy_revision: 0,
        principal: auth::Principal::Account {
            account_id: 2,
            role: "member".into(),
            profile_id: Some(2),
            session_id: Some("s2".into()),
        },
        session_id: Some("s2".into()),
    };
    let (items, _) = app.clone().with_lease(lease).register(
        "addon:2",
        vec![json!({"url":"http://second.source.invalid/movie.mp4"})],
        "movie",
    );
    let source = items[0]["id"].as_str().unwrap();
    let (status, error) = request(
        &app,
        "member-token-2",
        "POST",
        "/api/v2/playback",
        body(source, "private", "roku"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error_code"], "gateway_required");
    assert!(peer.starts.lock().unwrap().is_empty());
    registry::grant(&app.db.lock().unwrap(), 1, &gateway, 2, true).unwrap();
    let (_, start) = request(
        &app,
        "member-token-2",
        "POST",
        "/api/v2/playback",
        body(source, "granted", "roku"),
    )
    .await;
    let id = start["id"].as_str().unwrap();
    assert_eq!(
        settled_for(&app, "member-token-2", id).await["status"],
        "ready"
    );
    registry::grant(&app.db.lock().unwrap(), 1, &gateway, 2, false).unwrap();
    let status = request(
        &app,
        "member-token-2",
        "POST",
        &format!("/api/v2/playback/{id}/heartbeat"),
        Value::Null,
    )
    .await
    .0;
    assert!(matches!(status, StatusCode::NOT_FOUND | StatusCode::GONE));
    tokio::time::timeout(Duration::from_secs(3), async {
        while peer.stops.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn native_direct_and_mandatory_gateway_policy_do_not_invoke_embedded_playback() {
    let app = fixture();
    let source = source(&app);
    for platform in ["roku", "vizio", "web"] {
        let (status, error) = request(
            &app,
            "member-token-1",
            "POST",
            "/api/v2/playback",
            body(&source, platform, platform),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(error["error_code"], "gateway_required");
    }
    let input = body(&source, "native", "android");
    let (status, start) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/playback",
        input.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let id = start["id"].as_str().unwrap();
    let ready = settled(&app, id).await;
    assert_eq!(ready["delivery"]["kind"], "direct");
    assert!(ready["delivery"]["url"]
        .as_str()
        .unwrap()
        .starts_with("http://"));
    let (_, retry) = request(&app, "member-token-1", "POST", "/api/v2/playback", input).await;
    assert_eq!(retry["id"], id);
    assert_eq!(
        request(
            &app,
            "member-token-2",
            "GET",
            &format!("/api/v2/playback/{id}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(app.playback.active_count().await, 0);
    request(
        &app,
        "member-token-1",
        "DELETE",
        &format!("/api/v2/playback/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            &format!("/api/v2/playback/{id}/heartbeat"),
            Value::Null
        )
        .await
        .0,
        StatusCode::GONE
    );
}
#[tokio::test]
async fn gateway_selection_affinity_renewal_and_grant_revocation_are_scoped() {
    let (app, peer) = setup().await;
    let source = source(&app);
    let first = gateway(&app, "first", 20);
    let (_, start) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/playback",
        body(&source, "one", "roku"),
    )
    .await;
    let id = start["id"].as_str().unwrap();
    let ready = settled(&app, id).await;
    assert_eq!(ready["delivery"]["kind"], "gateway");
    assert!(ready["delivery"]["url"]
        .as_str()
        .unwrap()
        .starts_with("https://first.gateway.invalid/base/media/"));
    gateway(&app, "second", 0);
    peer.mode.store(4, Ordering::SeqCst);
    let (_, start2) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/playback",
        body(&source, "two", "vizio"),
    )
    .await;
    let id2 = start2["id"].as_str().unwrap();
    settled(&app, id2).await;
    assert_eq!(
        *peer.starts.lock().unwrap(),
        vec!["first", "first"],
        "existing-job affinity must precede a new higher-priority gateway"
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            &format!("/api/v2/playback/{id}/heartbeat"),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    request(
        &app,
        "member-token-1",
        "DELETE",
        &format!("/api/v2/playback/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(settled(&app, id2).await["status"], "ready");
    registry::update(
        &app.db.lock().unwrap(),
        1,
        &first,
        serde_json::from_value(json!({"enabled":false})).unwrap(),
    )
    .unwrap();
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            &format!("/api/v2/playback/{id2}/heartbeat"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    tokio::time::timeout(Duration::from_secs(3), async {
        while peer.stops.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(app.playback.active_count().await, 0);
}
#[tokio::test]
async fn source_mutation_and_remote_failures_are_closed_and_actionable() {
    let (app, peer) = setup().await;
    let source_id = source(&app);
    gateway(&app, "first", 0);
    peer.mode.store(1, Ordering::SeqCst);
    let (_, start) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/playback",
        body(&source_id, "bad-url", "roku"),
    )
    .await;
    let failed = settled(&app, start["id"].as_str().unwrap()).await;
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["error_code"], "gateway_protocol_invalid");
    assert!(failed["delivery"].is_null());
    assert!(!failed.to_string().contains("foreign.invalid"));
    peer.mode.store(2, Ordering::SeqCst);
    let (_, start) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/playback",
        body(&source_id, "limit", "roku"),
    )
    .await;
    let failed = settled(&app, start["id"].as_str().unwrap()).await;
    assert_eq!(failed["error_code"], "provider_connection_limit");
    assert!(failed["error"]
        .as_str()
        .unwrap()
        .contains("connection limit"));
    assert!(!failed.to_string().contains("provider-private-credential"));
    app.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE addons SET manifest_url='https://changed.invalid/manifest.json' WHERE id=1",
            [],
        )
        .unwrap();
    let (status, error) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/playback",
        body(&source_id, "changed", "android"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error_code"], "source_configuration_changed");
}
#[tokio::test]
async fn stopping_during_gateway_start_releases_the_late_viewer() {
    let (app, peer) = setup().await;
    let source = source(&app);
    gateway(&app, "first", 0);
    peer.mode.store(3, Ordering::SeqCst);
    let (_, start) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/playback",
        body(&source, "cancel", "roku"),
    )
    .await;
    let id = start["id"].as_str().unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while peer.starts.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    request(
        &app,
        "member-token-1",
        "DELETE",
        &format!("/api/v2/playback/{id}"),
        Value::Null,
    )
    .await;
    peer.hold.notify_one();
    tokio::time::timeout(Duration::from_secs(3), async {
        while peer.stops.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(settled(&app, id).await["status"], "released");
}

#[tokio::test]
async fn fresh_playback_skips_full_gateways_and_unassigned_provider_sources_fail_closed() {
    let (app, peer) = setup().await;
    let id = source(&app);
    gateway(&app, "first", 0);
    gateway(&app, "second", 20);
    peer.mode.store(4, Ordering::SeqCst);
    let (_, start) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/playback",
        body(&id, "capacity", "roku"),
    )
    .await;
    let ready = settled(&app, start["id"].as_str().unwrap()).await;
    assert_eq!(ready["status"], "ready");
    assert_eq!(*peer.starts.lock().unwrap(), vec!["second"]);
    app.db.lock().unwrap().execute("INSERT INTO providers(id,name,url,username,password) VALUES(1,'Unassigned','http://provider.fixture.invalid','user','password')",[]).unwrap();
    let scoped = app.clone().with_lease(lease());
    let (items, _) = scoped.register(
        "iptv:1",
        vec![json!({"url":"http://provider.fixture.invalid/movie"})],
        "movie",
    );
    let (_, error) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/playback",
        body(items[0]["id"].as_str().unwrap(), "unassigned", "android"),
    )
    .await;
    assert_eq!(error["error_code"], "source_not_found");
}

#[cfg(target_os = "linux")]
fn isolated_public_fixture_address() {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    assert_eq!(
        std::env::var("VIPTV_TEST_ISOLATED_NETWORK").as_deref(),
        Ok("container")
    );
    assert!(
        std::path::Path::new("/.dockerenv").exists(),
        "never configure a host network for this fixture"
    );
    let interfaces = std::fs::read_dir("/sys/class/net")
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(
        interfaces,
        vec![std::ffi::OsString::from("lo")],
        "run with Docker --network none"
    );
    // A globally classified address exists only on this disposable container's
    // isolated loopback. Production egress validation remains unchanged.
    unsafe {
        let raw = libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0);
        assert!(raw >= 0);
        let socket = OwnedFd::from_raw_fd(raw);
        let mut request: libc::ifreq = std::mem::zeroed();
        for (slot, byte) in request.ifr_name.iter_mut().zip(b"lo:fixture\0") {
            *slot = *byte as libc::c_char;
        }
        for (operation, octets) in [
            (libc::SIOCSIFADDR, [11, 255, 255, 1]),
            (libc::SIOCSIFNETMASK, [255, 255, 255, 255]),
        ] {
            let address = libc::sockaddr_in {
                sin_family: libc::AF_INET as u16,
                sin_port: 0,
                sin_addr: libc::in_addr {
                    s_addr: u32::from_ne_bytes(octets),
                },
                sin_zero: [0; 8],
            };
            request.ifr_ifru.ifru_addr =
                std::ptr::read((&address as *const libc::sockaddr_in).cast::<libc::sockaddr>());
            assert_eq!(
                libc::ioctl(socket.as_raw_fd(), operation, &request),
                0,
                "isolated fixture needs CAP_NET_ADMIN"
            );
        }
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "run only in a disposable --network none container with CAP_NET_ADMIN and VIPTV_TEST_ISOLATED_NETWORK=container"]
async fn isolated_backend_gateway_real_media_lifecycle() {
    isolated_public_fixture_address();
    let binary = std::env::var("VIPTV_TEST_GATEWAY_BINARY").unwrap();
    let ffmpeg = std::env::var("VIPTV_TEST_FFMPEG").unwrap();
    let ffprobe = std::env::var("VIPTV_TEST_FFPROBE").unwrap();
    let root = tempfile::tempdir().unwrap();
    let media_file = root.path().join("fixture.mp4");
    let output = tokio::process::Command::new(&ffmpeg)
        .kill_on_drop(true)
        .args([
            "-v",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x180:rate=25",
            "-t",
            "3",
            "-an",
            "-c:v",
            "libx264",
            "-threads",
            "2",
            "-preset",
            "ultrafast",
            "-g",
            "25",
            "-movflags",
            "+faststart",
        ])
        .arg(&media_file)
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    let media = std::fs::read(media_file).unwrap();
    let listener = tokio::net::TcpListener::bind("11.255.255.1:0")
        .await
        .unwrap();
    let source_address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/movie.mp4",
                axum::routing::get(move || {
                    let media = media.clone();
                    async move { media }
                }),
            ),
        )
        .await
        .unwrap();
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let bootstrap = "isolated-playback-fixture-bootstrap-key";
    let mut child = tokio::process::Command::new(binary)
        .kill_on_drop(true)
        .env("API_KEY", bootstrap)
        .env("DATA_DIR", root.path().join("state"))
        .env("BIND_ADDRESS", address.to_string())
        .env("FFMPEG_PATH", ffmpeg)
        .env("FFPROBE_PATH", ffprobe)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            assert!(child.try_wait().unwrap().is_none());
            if tokio::net::TcpStream::connect(address).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let network = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let issued:Value=network.post(format!("http://{address}/v1/keys")).bearer_auth(bootstrap).json(&json!({"label":"Backend lifecycle fixture","namespaces":["first"],"scopes":["capabilities","create","read","renew","release"],"quotas":{"inputs":1,"outputs":1,"viewers":5},"expires_at":null})).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    let mut app = fixture();
    app.secret_vault = Some(Arc::new(
        crate::secret_store::Vault::from_json(
            &json!({"active":"test","keys":{"test":STANDARD.encode([7u8;32])}}).to_string(),
        )
        .unwrap(),
    ));
    app.gateway_client = client::Client::fixture(format!("http://{address}/").parse().unwrap());
    let registration=serde_json::from_value(json!({"name":"Real fixture","endpoint":"https://first.gateway.invalid/","namespace":"first","integration_key":issued["secret"]})).unwrap();
    registry::register(
        &app.db.lock().unwrap(),
        app.secret_vault.as_ref().unwrap(),
        1,
        registration,
    )
    .unwrap();
    source(&app);
    let (items, _) = app.clone().with_lease(lease()).register(
        "addon:1",
        vec![json!({"url":format!("http://{source_address}/movie.mp4")})],
        "movie",
    );
    let (status, start) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/playback",
        body(items[0]["id"].as_str().unwrap(), "real-media", "roku"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let id = start["id"].as_str().unwrap();
    let ready = tokio::time::timeout(Duration::from_secs(40), async {
        loop {
            let (_, value) = request(
                &app,
                "member-token-1",
                "GET",
                &format!("/api/v2/playback/{id}"),
                Value::Null,
            )
            .await;
            if value["status"] != "starting" {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(ready["status"], "ready");
    assert_eq!(ready["delivery"]["kind"], "gateway");
    let delivery = url::Url::parse(ready["delivery"]["url"].as_str().unwrap()).unwrap();
    assert_eq!(delivery.host_str(), Some("first.gateway.invalid"));
    let playlist = network
        .get(format!("http://{address}{}", delivery.path()))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(playlist.starts_with("#EXTM3U"));
    let segment = playlist
        .lines()
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap();
    let media_url = url::Url::parse(&format!("http://{address}{}", delivery.path()))
        .unwrap()
        .join(segment)
        .unwrap();
    assert!(!network
        .get(media_url)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .bytes()
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        app.playback.active_count().await,
        0,
        "backend embedded engine must remain idle"
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            &format!("/api/v2/playback/{id}/heartbeat"),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    request(
        &app,
        "member-token-1",
        "DELETE",
        &format!("/api/v2/playback/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(
        network
            .get(format!("http://{address}{}", delivery.path()))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let pid = i32::try_from(child.id().unwrap()).unwrap();
    assert!(pid > 0);
    // Signal only the child process created by this fixture.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    assert!(tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .unwrap()
        .unwrap()
        .success());
    server.abort();
    let _ = server.await;
}
