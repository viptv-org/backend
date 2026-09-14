use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    routing::get,
    Router,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tower::ServiceExt;
use viptv_server::{
    playback::{Config, PlaybackManager},
    router, App,
};
const ACCOUNT_TOKEN: &str = "test-owner-account-session-token";
fn app() -> (Router, tempfile::TempDir) {
    let (state, dir) = app_state();
    (router(state, None), dir)
}
fn app_state() -> (App, tempfile::TempDir) {
    app_state_with_tools("missing-test-ffmpeg".into(), "missing-test-ffprobe".into())
}
fn app_state_with_tools(
    ffmpeg: std::path::PathBuf,
    ffprobe: std::path::PathBuf,
) -> (App, tempfile::TempDir) {
    app_state_with_tools_and_timeout(ffmpeg, ffprobe, Duration::from_secs(2))
}
fn app_state_with_tools_and_timeout(
    ffmpeg: std::path::PathBuf,
    ffprobe: std::path::PathBuf,
    request_timeout: Duration,
) -> (App, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let pm = PlaybackManager::new(Config {
        ffmpeg,
        ffprobe,
        root: dir.path().join("hls"),
        max_sessions: 2,
        ttl: Duration::from_secs(30),
    });
    let client = reqwest::Client::builder()
        .timeout(request_timeout)
        .build()
        .unwrap();
    let state = App::new(rusqlite::Connection::open_in_memory().unwrap(), client, pm).unwrap();
    {
        let db = state.db.lock().unwrap();
        let access_hash = format!("{:x}", Sha256::digest(ACCOUNT_TOKEN.as_bytes()));
        db.execute_batch("INSERT INTO auth_accounts(id,username,name,password_hash,role,recovery_hash,created_at) VALUES(1,'test-owner','Test Owner','unused','owner','unused',0);
            INSERT INTO profiles(id,name,avatar_style,avatar_seed,presentation_complete,created_at,updated_at) VALUES(1,'Test Owner','critters','api-fixture',1,0,0);
            INSERT INTO profile_owners(profile_id,account_id,created_at) VALUES(1,1,0);
            INSERT INTO auth_profiles(account_id,profile_id) VALUES(1,1);
            UPDATE addons SET account_id=1;")
            .unwrap();
        db.execute("INSERT INTO auth_sessions(id,account_id,access_hash,refresh_hash,csrf_hash,profile_id,kind,device_name,access_expires,refresh_expires,created_at) VALUES('api-session',1,?1,'unused-refresh','unused-csrf',1,'browser','integration',4102444800,4102444800,0)", [access_hash]).unwrap();
    }
    (state, dir)
}
async fn request(app: &Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    request_as(app, method, path, body, ACCOUNT_TOKEN).await
}
async fn request_as(
    app: &Router,
    method: &str,
    path: &str,
    body: Value,
    token: &str,
) -> (StatusCode, Value) {
    let r = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = r.status();
    let b = to_bytes(r.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&b).unwrap_or(Value::Null))
}

// One loopback upstream: base URL plus the task that must be aborted by the caller.
async fn serve_upstream(mock: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    (format!("http://{address}"), upstream)
}

// Protocol fixtures for the media-tool seam, not real decoder acceptance. Physical
// Roku and real FFmpeg checks separately validate the produced media.
#[cfg(unix)]
fn live_session_fixture(stall_probe: bool) -> (App, tempfile::TempDir, tempfile::TempDir, String) {
    use std::os::unix::fs::PermissionsExt;
    let tools = tempfile::tempdir().unwrap();
    let probe = tools.path().join("ffprobe");
    let engine = tools.path().join("ffmpeg");
    let probe_body = if stall_probe {
        "printf started > \"$0.started\"\nexec sleep 60"
    } else {
        r#"printf '%s' '{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":1280,"height":720,"pix_fmt":"yuv420p","level":31,"avg_frame_rate":"30/1"},{"index":1,"codec_type":"audio","codec_name":"aac","channels":2,"tags":{"language":"eng"}}],"format":{}}'"#
    };
    std::fs::write(&probe, format!("#!/bin/sh\n{probe_body}\n")).unwrap();
    std::fs::write(&engine, r#"#!/bin/sh
if [ "$1" = "-version" ]; then exit 0; fi
for output do :; done
directory=${output%/*}
printf fixture > "$directory/segment-000000000.ts"
printf '#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:1,\nsegment-000000000.ts\n' > "$output"
exec sleep 60
"#).unwrap();
    for file in [&probe, &engine] {
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let (state, media) = app_state_with_tools(engine, probe);
    // Seed an already imported channel; assertions below use authenticated routes.
    state.db.lock().unwrap().execute_batch(
        "INSERT INTO providers(id,name,url,username,password) VALUES(1,'Fixture','http://fixture.invalid','fixture','fixture');
         INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES('iptv:1:1',1,'1','Fixture East');"
    ).unwrap();
    (state, media, tools, "iptv:1:1".into())
}

#[cfg(unix)]
#[tokio::test]
async fn live_session_keeps_capacity_until_stop_and_invalidates_old_media() {
    let (state, _media, _tools, channel) = live_session_fixture(false);
    let app = router(state.clone(), None);
    let (status, listing) = request(&app, "GET", "/api/live", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listing["channels"][0]["id"], channel);
    let (status, first) = request(
        &app,
        "POST",
        "/api/playback",
        json!({"channel_id":channel,"force_transcode":true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["live"], true);
    assert_eq!(first["format"], "hls");
    let id = first["id"].as_str().unwrap();
    assert_eq!(
        request(&app, "POST", "/api/playback", json!({"channel_id":channel}))
            .await
            .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        request(
            &app,
            "POST",
            &format!("/api/playback/{id}/heartbeat"),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, "DELETE", &format!("/api/playback/{id}"), Value::Null)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, "GET", first["url"].as_str().unwrap(), Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let (status, second) =
        request(&app, "POST", "/api/playback", json!({"channel_id":channel})).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_ne!(first["id"], second["id"]);
    assert_eq!(
        request(
            &app,
            "DELETE",
            &format!("/api/playback/{}", second["id"].as_str().unwrap()),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, "GET", "/api/status", Value::Null).await.1["active_sessions"],
        0
    );
    state.playback.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn cancelled_live_startup_releases_capacity_without_publishing_a_session() {
    let (state, _media, tools, channel) = live_session_fixture(true);
    let app = router(state.clone(), None);
    let worker = {
        let app = app.clone();
        let channel = channel.clone();
        tokio::spawn(async move {
            request(&app, "POST", "/api/playback", json!({"channel_id":channel})).await
        })
    };
    tokio::time::timeout(Duration::from_secs(3), async {
        while !tools.path().join("ffprobe.started").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        request(
            &app,
            "POST",
            "/api/playback",
            json!({"channel_id":channel,"force_transcode":true})
        )
        .await
        .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let (status, _) = request(
                &app,
                "PATCH",
                "/api/providers/1",
                json!({"max_connections":2}),
            )
            .await;
            if status == StatusCode::OK {
                break;
            }
            assert_eq!(status, StatusCode::CONFLICT);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        request(&app, "GET", "/api/status", Value::Null).await.1["active_sessions"],
        0
    );
    state.playback.shutdown().await;
}
#[tokio::test]
async fn iptv_batches_survive_stalled_provider_and_keep_cursor() {
    use axum::extract::Query;
    use std::{
        collections::HashMap,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
    };
    // This verifies publication ordering, not a host-speed SLA. The mock cannot
    // finish its blocked candidate until the test releases it after both fast batches.
    const DEADLOCK_TIMEOUT: Duration = Duration::from_secs(10);
    let entered = Arc::new(tokio::sync::Notify::new());
    let completed = Arc::new(AtomicBool::new(false));
    let release = Arc::new(tokio::sync::Notify::new());
    let stalled = release.clone();
    let slow_entered = entered.clone();
    let slow_completed = completed.clone();
    let mock = Router::new().route(
        "/player_api.php",
        get(move |Query(q): Query<HashMap<String, String>>| {
            let stalled = stalled.clone();
            let slow_entered = slow_entered.clone();
            let slow_completed = slow_completed.clone();
            async move {
                if q["username"] == "slow" && q["series_id"] == "1" {
                    slow_entered.notify_one();
                    stalled.notified().await;
                    slow_completed.store(true, Ordering::SeqCst);
                    return (
                        StatusCode::BAD_GATEWAY,
                        axum::Json(json!({"error":"controlled stalled fixture failure"})),
                    );
                }
                (
                    StatusCode::OK,
                    axum::Json(json!({"episodes":{"1":[{"id":44,"episode_num":2}]}})),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let fixture = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    // Transport timeout must not release the blocked candidate during the ordering check.
    let (state, _dir) = app_state_with_tools_and_timeout(
        "missing-test-ffmpeg".into(),
        "missing-test-ffprobe".into(),
        Duration::from_secs(30),
    );
    state.addons.delete(1).unwrap();
    for (name, stream) in [("slow", "1"), ("fast", "2"), ("slow", "3")] {
        let p = if stream == "3" {
            1
        } else {
            state
                .providers
                .add(json!({"name":name,"url":url,"username":name,"password":"synthetic"}))
                .unwrap()["id"]
                .as_i64()
                .unwrap()
        };
        state.db.lock().unwrap().execute("INSERT INTO provider_vod(id,provider_id,stream_id,kind,name,normalized,year,imdb_id,extension) VALUES(?1,?2,?3,'series','Test','test',2024,'tt1234567','mp4')", rusqlite::params![format!("iptv:{p}:series:{stream}"), p, stream]).unwrap();
    }
    let a = router(state, None);
    let (_, j) = request(
        &a,
        "POST",
        "/api/streams",
        json!({"type":"series","id":"tt1234567:1:2","name":"Test","year":2024}),
    )
    .await;
    let path = format!("/api/streams/{}", j["id"].as_str().unwrap());
    tokio::time::timeout(DEADLOCK_TIMEOUT, entered.notified())
        .await
        .expect("stalled provider candidate must enter its fixture barrier");
    assert!(!completed.load(Ordering::SeqCst));
    let early = tokio::time::timeout(DEADLOCK_TIMEOUT, async {
        loop {
            let (_, v) = request(&a, "GET", &path, Value::Null).await;
            if v["events"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|e| !e["streams"].as_array().unwrap().is_empty())
                .count()
                == 2
            {
                break v;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("fast provider AND fast candidate of stalled provider must publish while slow candidate is held");
    assert!(
        !completed.load(Ordering::SeqCst),
        "slow fixture completed before explicit release"
    );
    assert_eq!(early["done"], false);
    assert!(early["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["source"] == "iptv:1"));
    assert!(early["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["source"] == "iptv:2"));
    let cursor = early["events"].as_array().unwrap().len();
    release.notify_one();
    let final_state = tokio::time::timeout(DEADLOCK_TIMEOUT, async {
        loop {
            let (_, v) = request(&a, "GET", &path, Value::Null).await;
            if v["done"] == true {
                break v;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("released stalled candidate must finish and append its late error");
    assert!(
        completed.load(Ordering::SeqCst),
        "late error must come from released fixture, not a transport timeout"
    );
    let all = final_state["events"].as_array().unwrap();
    assert_eq!(
        &all[..cursor],
        early["events"].as_array().unwrap().as_slice()
    );
    assert!(all
        .iter()
        .any(|e| e["source"] == "iptv:1" && e["error"].is_string()));
    let (_, tail) = request(&a, "GET", &format!("{path}?after={cursor}"), Value::Null).await;
    assert_eq!(tail["done"], true);
    assert_eq!(tail["events"], json!(&all[cursor..]));
    fixture.abort();
}

#[tokio::test]
async fn database_waiters_do_not_starve_runtime() {
    let (state, _dir) = app_state();
    let db = state.db.clone();
    let (held_tx, held_rx) = tokio::sync::oneshot::channel();
    let holder = tokio::task::spawn_blocking(move || {
        let _guard = db.lock().unwrap();
        held_tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(400));
    });
    held_rx.await.unwrap();
    let a = router(state, None);
    let mut tasks = Vec::new();
    for path in [
        "/api/profiles",
        "/api/providers",
        "/api/addons",
        "/api/catalogs",
        "/api/live",
        "/api/matches",
        "/api/profiles/1/progress",
        "/api/profiles/1/favorites",
    ] {
        let a = a.clone();
        tasks.push(tokio::spawn(async move {
            request(&a, "GET", path, Value::Null).await
        }));
    }
    let start = std::time::Instant::now();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(
        start.elapsed() < Duration::from_millis(250),
        "std mutex waiters blocked the single Tokio worker"
    );
    assert_eq!(
        request(&a, "GET", "/api/health", Value::Null).await.0,
        StatusCode::OK
    );
    holder.await.unwrap();
    for task in tasks {
        assert_eq!(task.await.unwrap().0, StatusCode::OK);
    }
}

#[tokio::test]
async fn playback_rejects_provider_over_capacity_before_probe() {
    let (state, _dir) = app_state();
    let p = state
        .providers
        .add(json!({"name":"Limited","url":"http://127.0.0.1:1","username":"u","password":"p"}))
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    let channel = format!("iptv:{p}:1");
    state
        .db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES(?1,?2,'1','Test')",
            rusqlite::params![channel, p],
        )
        .unwrap();
    let permit = state.providers.acquire_playback(p).await.unwrap();
    let a = router(state.clone(), None);
    assert_eq!(
        request(&a, "POST", "/api/playback", json!({"channel_id":channel}))
            .await
            .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        request(
            &a,
            "PATCH",
            &format!("/api/providers/{p}"),
            json!({"max_connections": 2}),
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    drop(permit);
    let (status, updated) = request(
        &a,
        "PATCH",
        &format!("/api/providers/{p}"),
        json!({"max_connections": 2}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["max_connections"], 2);
    // Missing test ffprobe fails, but capacity must still be released on every error.
    assert_eq!(
        request(&a, "POST", "/api/playback", json!({"channel_id":channel}))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert!(state.providers.acquire_playback(p).await.is_ok());
}

#[tokio::test]
async fn addon_patch_lists_disabled_and_filters_catalogs() {
    let (a, _dir) = app();
    let (status, patched) = request(&a, "PATCH", "/api/addons/1", json!({"enabled":false})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(patched["priority"].is_null());
    let (_, listed) = request(&a, "GET", "/api/addons", Value::Null).await;
    assert_eq!(listed[0]["enabled"], false);
    assert_eq!(
        request(&a, "GET", "/api/catalogs", Value::Null).await.1,
        json!([])
    );
    assert_eq!(
        request(&a, "PATCH", "/api/addons/1", json!({"enabled":"false"}))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn auth_and_profile_state() {
    let (a, _dir) = app();
    let health = a
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    let denied = a
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/profiles")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    let (s, p) = request(&a, "POST", "/api/profiles", json!({"name":"Family"})).await;
    assert_eq!(s, StatusCode::OK);
    let id = p["id"].as_str().unwrap();
    assert_eq!(
        request(&a, "POST", "/api/auth/profile", json!({"profile_id":id}))
            .await
            .0,
        StatusCode::OK
    );
    let path = format!("/api/profiles/{id}/favorites");
    assert_eq!(
        request(
            &a,
            "PUT",
            &path,
            json!({"id":"tt123","type":"movie","name":"Movie"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&a, "GET", &path, Value::Null).await.1[0]["id"],
        "tt123"
    );
    let path = format!("/api/profiles/{id}/progress");
    assert_eq!(
        request(
            &a,
            "PUT",
            &path,
            json!({"id":"tt123:1:2","type":"series","name":"Episode","position":44,"duration":100})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&a, "GET", &path, Value::Null).await.1[0]["position"],
        44.0
    );
    assert_eq!(
        request(
            &a,
            "PUT",
            &path,
            json!({"id":"tt123","type":"series","name":"Bad","position":-1,"duration":100})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}
#[tokio::test]
async fn provider_credentials_redacted_and_url_rejected() {
    let (a, _dir) = app();
    let(s,p)=request(&a,"POST","/api/providers",json!({"name":"Local","url":"http://127.0.0.1:23456","username":"viewer","password":"super-secret"})).await;
    assert_eq!(s, StatusCode::OK, "{p}");
    let (s, p) = request(&a, "GET", "/api/providers", Value::Null).await;
    assert_eq!(s, StatusCode::OK);
    assert!(!p.to_string().contains("super-secret"));
    assert!(p[0].get("password").is_none());
    assert_eq!(
        request(
            &a,
            "POST",
            "/api/providers",
            json!({"name":"Bad","url":"file:///etc/passwd","username":"u","password":"p"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            &a,
            "POST",
            "/api/playback",
            json!({"url":"http://arbitrary","position":0})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}
#[tokio::test]
async fn addon_mock_discovery_cursor_and_unsupported_sources() {
    let mock=Router::new().route("/manifest.json",get(||async{axum::Json(json!({"id":"test","name":"Mock","types":["movie"],"resources":["catalog","meta","stream"],"catalogs":[{"id":"top","type":"movie","name":"Movies","extra":[{"name":"search"},{"name":"skip"},{"name":"genre","options":["Family & Kids","Comedy"],"optionsLimit":1}]}]}))})).route("/catalog/movie/top/:extra",get(||async{axum::Json(json!({"metas":[{"id":"tt1","type":"movie","name":"Mock Movie"}]}))})).route("/meta/movie/:id",get(||async{axum::Json(json!({"meta":{"id":"tt1","name":"Mock Movie","releaseInfo":"2024"}}))})).route("/stream/movie/:id",get(||async{axum::Json(json!({"streams":[{"name":"HTTP","url":"https://example.com/movie.mp4"},{"name":"Torrent","infoHash":"0123456789"}]}))}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let (a, _dir) = app();
    let (s, added) = request(
        &a,
        "POST",
        "/api/addons",
        json!({"manifest_url":format!("http://{addr}/manifest.json")}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{added}");
    let (s, catalogs) = request(&a, "GET", "/api/catalogs", Value::Null).await;
    assert_eq!(s, StatusCode::OK);
    let catalog = catalogs
        .as_array()
        .unwrap()
        .iter()
        .find(|catalog| catalog["addon_id"] == added["id"])
        .unwrap();
    assert_eq!(catalog["supports_search"], true);
    assert_eq!(catalog["supports_skip"], true);
    assert_eq!(catalog["genres"], json!(["Family & Kids", "Comedy"]));
    assert_eq!(catalog["extra"][2]["name"], "genre");
    assert_eq!(catalog["extra"][2]["options_limit"], 1);
    let (s, d) = request(
        &a,
        "GET",
        &format!(
            "/api/discover?type=movie&addon_id={}&catalog=top&genre=Family%20%26%20Kids",
            added["id"]
        ),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{d}");
    assert_eq!(d["metas"][0]["id"], "tt1");
    let (invalid_status, invalid) = request(
        &a,
        "GET",
        &format!(
            "/api/discover?type=movie&addon_id={}&catalog=top&genre=Horror",
            added["id"]
        ),
        Value::Null,
    )
    .await;
    assert_eq!(invalid_status, StatusCode::BAD_REQUEST);
    assert!(invalid["error"]
        .as_str()
        .unwrap()
        .contains("advertised options"));
    let (s, j) = request(
        &a,
        "POST",
        "/api/streams",
        json!({"type":"movie","id":"tt1","name":"Mock Movie","year":2024}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let path = format!("/api/streams/{}", j["id"].as_str().unwrap());
    let events = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (_, v) = request(&a, "GET", &path, Value::Null).await;
            if v["done"] == true {
                break v;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let array = events["events"].as_array().unwrap();
    assert_eq!(array.len(), 2);
    let addon = array
        .iter()
        .find(|e| e["source"].as_str().unwrap().starts_with("addon:"))
        .unwrap();
    assert_eq!(addon["streams"].as_array().unwrap().len(), 1);
    assert!(addon["streams"][0].get("url").is_none());
    assert!(addon["error"].as_str().unwrap().contains("unsupported"));
    let (_, v) = request(&a, "GET", &format!("{path}?after=1"), Value::Null).await;
    assert_eq!(v["events"].as_array().unwrap().len(), 1);
    assert_eq!(v["events"][0]["seq"], 2);
    let response = a
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("{path}/events?after=1"))
                .header("authorization", format!("Bearer {ACCOUNT_TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let events = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(events.contains("event: streams"));
    assert!(events.contains("id: 2"));
    assert!(events.contains("event: done"));
    assert!(!events.contains("id: 1"));
    task.abort();
}

#[cfg(unix)]
#[tokio::test]
async fn family_channel_has_stable_identity_and_plays_manual_candidate() {
    let (state, _media, _tools, raw) = live_session_fixture(false);
    let app = router(state.clone(), None);
    let (status, channel) = request(
        &app,
        "POST",
        "/api/lineup",
        json!({
            "name":"Fixture East", "network":"Fixture", "feed":"east", "market":"",
            "category":"Kids", "number":1, "enabled":true,
            "candidates":[{"id":raw,"name":"Fixture East","verified":true}]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{channel}");
    let id = channel["id"].as_str().unwrap();
    assert!(id.starts_with("family:"));
    assert_eq!(
        request(
            &app,
            "PATCH",
            "/api/lineup/settings",
            json!({"enabled":true,"limit":100})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, live) = request(&app, "GET", "/api/live", Value::Null).await;
    assert_eq!(live["channels"][0]["id"], id);
    let (status, session) = request(&app, "POST", "/api/playback", json!({"channel_id":id})).await;
    assert_eq!(status, StatusCode::OK, "{session}");
    request(
        &app,
        "DELETE",
        &format!("/api/playback/{}", session["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    state
        .db
        .lock()
        .unwrap()
        .execute("UPDATE provider_live SET name='Fixture West'", [])
        .unwrap();
    assert_ne!(
        request(&app, "POST", "/api/playback", json!({"channel_id":id}))
            .await
            .0,
        StatusCode::OK
    );
    state
        .db
        .lock()
        .unwrap()
        .execute("DELETE FROM provider_live", [])
        .unwrap();
    let (_, live) = request(&app, "GET", "/api/live", Value::Null).await;
    assert_eq!(live["channels"][0]["id"], id);
    assert_eq!(live["channels"][0]["number"], 1);
    assert_ne!(
        request(&app, "POST", "/api/playback", json!({"channel_id":id}))
            .await
            .0,
        StatusCode::OK
    );
    state.playback.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn family_lineup_preserves_verified_legacy_references_and_rejects_wrong_feeds() {
    let (state, _media, _tools, raw) = live_session_fixture(false);
    let app = router(state.clone(), None);
    request(
        &app,
        "PUT",
        "/api/profiles/1/favorites",
        json!({"id":raw,"type":"live","name":"Old name"}),
    )
    .await;
    state.db.lock().unwrap().execute_batch("INSERT INTO progress(profile_id,id,type,name,position,duration,updated_at) VALUES(1,'iptv:1:1','live','Legacy East',0,0,1),(1,'uncertain:42','live','Unknown legacy channel',0,0,2)").unwrap();
    let body = json!({"name":"Fixture East","network":"Fixture","feed":"east","market":"","category":"Kids","number":1,"enabled":true,"candidates":[{"id":raw,"name":"Fixture East","verified":true}]});
    let mut wrong = body.clone();
    wrong["feed"] = json!("west");
    assert_eq!(
        request(&app, "POST", "/api/lineup", wrong).await.0,
        StatusCode::BAD_REQUEST
    );
    let (status, channel) = request(&app, "POST", "/api/lineup", body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{channel}");
    let (_, history) = request(&app, "GET", "/api/profiles/1/progress", Value::Null).await;
    assert_eq!(history[0]["id"], "uncertain:42");
    assert_eq!(history[1]["id"], channel["id"]);
    assert_eq!(
        state
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM progress WHERE id='iptv:1:1'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    let (_, favorites) = request(&app, "GET", "/api/profiles/1/favorites", Value::Null).await;
    assert_eq!(favorites[0]["id"], channel["id"]);
    assert_eq!(favorites[0]["name"], "Fixture East");
    request(
        &app,
        "DELETE",
        &format!(
            "/api/profiles/1/favorites/live/{}",
            channel["id"].as_str().unwrap()
        ),
        Value::Null,
    )
    .await;
    assert_eq!(
        request(&app, "GET", "/api/profiles/1/favorites", Value::Null)
            .await
            .1,
        json!([])
    );
    request(
        &app,
        "PATCH",
        "/api/lineup/settings",
        json!({"enabled":true,"limit":1}),
    )
    .await;
    let mut second = body.clone();
    second["number"] = json!(2);
    second["feed"] = json!("west");
    second["candidates"] = json!([]);
    assert_eq!(
        request(&app, "POST", "/api/lineup", second.clone()).await.0,
        StatusCode::BAD_REQUEST
    );
    second["enabled"] = json!(false);
    assert_eq!(
        request(&app, "POST", "/api/lineup", second).await.0,
        StatusCode::OK
    );
    state
        .db
        .lock()
        .unwrap()
        .execute("UPDATE auth_accounts SET role='member' WHERE id=1", [])
        .unwrap();
    assert_eq!(
        request(&app, "POST", "/api/lineup", body).await.0,
        StatusCode::FORBIDDEN
    );
    state.playback.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn family_verification_rejects_stale_catalog_and_inventory_mode_keeps_raw_playback() {
    let (state, _media, _tools, raw) = live_session_fixture(false);
    let app = router(state.clone(), None);
    let body = json!({"name":"Fixture East","network":"Fixture","feed":"east","market":"","category":"Kids","number":1,"enabled":false,"candidates":[{"id":raw,"name":"Fixture East","verified":true}]});
    let (status, channel) = request(&app, "POST", "/api/lineup", body.clone()).await;
    assert_eq!(status, StatusCode::OK);
    let (status, session) = request(&app, "POST", "/api/playback", json!({"channel_id":raw})).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "Raw channel remains playable when full inventory is active: {session}"
    );
    request(
        &app,
        "DELETE",
        &format!("/api/playback/{}", session["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    state
        .db
        .lock()
        .unwrap()
        .execute("UPDATE provider_live SET name='Other Network East'", [])
        .unwrap();
    let (status, error) = request(
        &app,
        "PATCH",
        &format!("/api/lineup/{}", channel["id"].as_str().unwrap()),
        body,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    state.playback.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn family_startup_tries_verified_backup_and_reports_sanitized_attempts() {
    let (state, _media, tools_dir, raw) = live_session_fixture(false);
    let probe = tools_dir.path().join("ffprobe");
    let script = std::fs::read_to_string(&probe).unwrap();
    std::fs::write(&probe,script.replacen("#!/bin/sh\n","#!/bin/sh\nfor input do :; done\ncase \"$input\" in */1.ts) printf 'bad input' >&2; exit 1;; esac\n",1)).unwrap();
    state.db.lock().unwrap().execute("INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES('iptv:1:2',1,'2','Fixture East')",[]).unwrap();
    let app = router(state.clone(), None);
    let (status,channel)=request(&app,"POST","/api/lineup",json!({"name":"Fixture East","network":"Fixture","feed":"east","market":"","category":"Kids","number":1,"enabled":true,"candidates":[{"id":raw,"name":"Fixture East","verified":true},{"id":"iptv:1:2","name":"Fixture East","verified":true}]})).await;
    assert_eq!(status, StatusCode::OK, "{channel}");
    let (status, session) = request(
        &app,
        "POST",
        "/api/playback",
        json!({"channel_id":channel["id"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{session}");
    let (_, lineup) = request(&app, "GET", "/api/lineup", Value::Null).await;
    assert_eq!(
        lineup["channels"][0]["last_startup"]["attempts"][0]["reason"],
        "startup_failed"
    );
    assert_eq!(
        lineup["channels"][0]["last_startup"]["attempts"][1]["reason"],
        "selected"
    );
    let (busy_status, busy) = request(
        &app,
        "POST",
        "/api/playback",
        json!({"channel_id":channel["id"],"force_transcode":true}),
    )
    .await;
    assert_eq!(busy_status, StatusCode::TOO_MANY_REQUESTS, "{busy}");
    assert!(busy["error"]
        .as_str()
        .unwrap()
        .contains("connections are busy"));
    request(
        &app,
        "DELETE",
        &format!("/api/playback/{}", session["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    std::fs::write(tools_dir.path().join("ffmpeg"), "#!/bin/sh\nexit 1\n").unwrap();
    let (failed_status, failed) = request(
        &app,
        "POST",
        "/api/playback",
        json!({"channel_id":channel["id"]}),
    )
    .await;
    assert_eq!(failed_status, StatusCode::BAD_GATEWAY, "{failed}");
    assert!(failed["error"]
        .as_str()
        .unwrap()
        .contains("No working stream"));
    assert_eq!(
        request(&app, "GET", "/api/status", Value::Null).await.1["active_sessions"],
        0
    );
    state.playback.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn family_startup_cancellation_and_revocation_prevent_later_candidates() {
    for mode in 0..3 {
        let (state, _media, tools_dir, raw) = live_session_fixture(true);
        let probe = tools_dir.path().join("ffprobe");
        std::fs::write(&probe,"#!/bin/sh\nfor input do :; done\ncase \"$input\" in */2.ts) printf attempted > \"$0.backup\";; esac\nprintf started > \"$0.started\"\nexec sleep 60\n").unwrap();
        state.db.lock().unwrap().execute("INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES('iptv:1:2',1,'2','Fixture East')",[]).unwrap();
        let app = router(state.clone(), None);
        let (_,channel)=request(&app,"POST","/api/lineup",json!({"name":"Fixture East","network":"Fixture","feed":"east","market":"","category":"Kids","number":1,"enabled":true,"candidates":[{"id":raw,"name":"Fixture East","verified":true},{"id":"iptv:1:2","name":"Fixture East","verified":true}]})).await;
        let worker_app = app.clone();
        let worker = tokio::spawn(async move {
            request(
                &worker_app,
                "POST",
                "/api/playback",
                json!({"channel_id":channel["id"],"startup_id":"cancel-fixture"}),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while !tools_dir.path().join("ffprobe.started").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        if mode == 1 {
            state
                .db
                .lock()
                .unwrap()
                .execute("DELETE FROM auth_sessions WHERE id='api-session'", [])
                .unwrap();
            let (status, _) = tokio::time::timeout(Duration::from_secs(2), worker)
                .await
                .unwrap()
                .unwrap();
            assert!(status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN);
        } else if mode == 2 {
            assert_eq!(
                request(
                    &app,
                    "DELETE",
                    "/api/playback/startups/cancel-fixture",
                    Value::Null
                )
                .await
                .0,
                StatusCode::OK
            );
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(2), worker)
                    .await
                    .unwrap()
                    .unwrap()
                    .0,
                StatusCode::CONFLICT
            );
        } else {
            worker.abort();
            assert!(worker.await.unwrap_err().is_cancelled());
        }
        tokio::time::timeout(
            Duration::from_secs(2),
            state.playback.settle_cancelled_inputs(),
        )
        .await
        .unwrap();
        assert!(!tools_dir.path().join("ffprobe.backup").exists());
        assert_eq!(state.playback.active_count().await, 0);
        let permit = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(permit) = state.providers.acquire_playback_for_kind(1, "live").await {
                    break permit;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Cancelled audience tears down and releases its input");
        drop(permit);
        state.playback.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn owned_startup_cancel_arriving_before_playback_prevents_any_probe() {
    let (state, _media, tools_dir, raw) = live_session_fixture(true);
    let app = router(state.clone(), None);
    assert_eq!(
        request(
            &app,
            "DELETE",
            "/api/playback/startups/fixture-start",
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    let (status, _) = request(
        &app,
        "POST",
        "/api/playback",
        json!({"channel_id":raw,"startup_id":"fixture-start"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(!tools_dir.path().join("ffprobe.started").exists());
    state.playback.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires VIPTV_TEST_FFMPEG and VIPTV_TEST_FFPROBE; run in isolated media image"]
async fn real_family_fallback_observes_upstream_connection_allowance() {
    use std::future::IntoFuture;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    #[derive(Clone)]
    struct Upstream {
        bytes: Arc<Vec<u8>>,
        active: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
        attempts: Arc<std::sync::Mutex<Vec<String>>>,
    }
    struct Connected(Arc<AtomicUsize>);
    impl Drop for Connected {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    async fn stream(
        axum::extract::State(up): axum::extract::State<Upstream>,
        axum::extract::Path(file): axum::extract::Path<String>,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;
        let active = up.active.fetch_add(1, Ordering::SeqCst) + 1;
        up.peak.fetch_max(active, Ordering::SeqCst);
        let guard = Connected(up.active.clone());
        up.attempts.lock().unwrap().push(file.clone());
        if file == "1.ts" {
            drop(guard);
            return StatusCode::UNAUTHORIZED.into_response();
        }
        let body = async_stream::stream! {
            let _guard=guard;
            loop {
                for chunk in up.bytes.chunks(188*7) {
                    tokio::time::sleep(Duration::from_micros((188*7*8_000_000 / up.bytes.len()) as u64)).await;
                    yield Ok::<_,std::io::Error>(axum::body::Bytes::copy_from_slice(chunk));
                }
            }
        };
        ([("content-type", "video/mp2t")], Body::from_stream(body)).into_response()
    }
    let ffmpeg = std::path::PathBuf::from(
        std::env::var("VIPTV_TEST_FFMPEG").expect("Explicit real-media tool required"),
    );
    let ffprobe = std::path::PathBuf::from(
        std::env::var("VIPTV_TEST_FFPROBE").expect("Explicit real-media tool required"),
    );
    let fixture = tempfile::tempdir().unwrap();
    let media = fixture.path().join("fixture.ts");
    let generated = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new(&ffmpeg)
            .kill_on_drop(true)
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=640x360:rate=30",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000",
                "-t",
                "8",
                "-c:v",
                "libx264",
                "-threads",
                "1",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-g",
                "30",
                "-c:a",
                "aac",
                "-f",
                "mpegts",
            ])
            .arg(&media)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        generated.status.success(),
        "Controlled test media generation failed"
    );
    let up = Upstream {
        bytes: Arc::new(std::fs::read(media).unwrap()),
        active: Arc::new(AtomicUsize::new(0)),
        peak: Arc::new(AtomicUsize::new(0)),
        attempts: Arc::new(std::sync::Mutex::new(Vec::new())),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream = tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route("/live/fixture/fixture/:file", get(stream))
                .with_state(up.clone()),
        )
        .into_future(),
    );
    let (state, _root) = app_state_with_tools(ffmpeg, ffprobe);
    state.db.lock().unwrap().execute("INSERT INTO providers(id,name,url,username,password,max_connections) VALUES(1,'Controlled fixture',?1,'fixture','fixture',1)",[format!("http://{address}")]).unwrap();
    state.db.lock().unwrap().execute_batch("INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES('iptv:1:1',1,'1','Fixture East'),('iptv:1:2',1,'2','Fixture East')").unwrap();
    let app = router(state.clone(), None);
    let (_,channel)=request(&app,"POST","/api/lineup",json!({"name":"Fixture East","network":"Fixture","feed":"east","market":"","category":"Kids","number":1,"enabled":true,"candidates":[{"id":"iptv:1:1","name":"Fixture East","verified":true},{"id":"iptv:1:2","name":"Fixture East","verified":true}]})).await;
    let (status, session) = request(
        &app,
        "POST",
        "/api/playback",
        json!({"channel_id":channel["id"],"startup_id":"real-fixture"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{session}");
    assert_eq!(up.peak.load(Ordering::SeqCst),1,"Observed upstream concurrency must respect the single account slot across probe and playback");
    assert_eq!(up.active.load(Ordering::SeqCst), 1);
    assert!(up.attempts.lock().unwrap().iter().any(|s| s == "1.ts"));
    assert!(up.attempts.lock().unwrap().iter().any(|s| s == "2.ts"));
    assert_eq!(
        request(
            &app,
            "DELETE",
            "/api/playback/startups/real-fixture",
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    tokio::time::timeout(Duration::from_secs(3), async {
        while up.active.load(Ordering::SeqCst) != 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(state.playback.active_count().await, 0);
    state.playback.shutdown().await;
    upstream.abort();
    let _ = upstream.await;
    println!("REAL_FAMILY_FALLBACK_OK peak_connections=1 active_after_stop=0");
}

#[cfg(unix)]
#[tokio::test]
async fn startup_cancellation_is_scoped_to_the_requesting_auth_session() {
    let (state, _media, tools_dir, raw) = live_session_fixture(true);
    let other_token = "other-session-fixture-token";
    let hash = format!("{:x}", Sha256::digest(other_token.as_bytes()));
    state.db.lock().unwrap().execute("INSERT INTO auth_sessions(id,account_id,access_hash,refresh_hash,csrf_hash,profile_id,kind,device_name,access_expires,refresh_expires,created_at) VALUES('other-session',1,?1,'other-refresh','other-csrf',1,'browser','fixture',4102444800,4102444800,0)",[hash]).unwrap();
    let app = router(state.clone(), None);
    let worker_app = app.clone();
    let worker = tokio::spawn(async move {
        request(
            &worker_app,
            "POST",
            "/api/playback",
            json!({"channel_id":raw,"startup_id":"shared-name"}),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !tools_dir.path().join("ffprobe.started").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        request_as(
            &app,
            "DELETE",
            "/api/playback/startups/shared-name",
            Value::Null,
            other_token
        )
        .await
        .0,
        StatusCode::OK
    );
    tokio::time::sleep(Duration::from_millis(75)).await;
    assert!(
        !worker.is_finished(),
        "Another authenticated session cannot cancel this startup"
    );
    request(
        &app,
        "DELETE",
        "/api/playback/startups/shared-name",
        Value::Null,
    )
    .await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), worker)
            .await
            .unwrap()
            .unwrap()
            .0,
        StatusCode::CONFLICT
    );
    state.playback.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn interrupted_family_playback_recovers_under_the_same_owned_session() {
    let (state, _media, tools_dir, raw) = live_session_fixture(false);
    let engine = tools_dir.path().join("ffmpeg");
    let script = std::fs::read_to_string(&engine).unwrap();
    let script=script.replace("for output do :; done","input=''\nprevious=''\nfor argument do\n if [ \"$previous\" = '-i' ]; then input=$argument; fi\n previous=$argument\n output=$argument\ndone").replace("exec sleep 60","case \"$input\" in */1.ts) while [ ! -f \"$0.disconnect\" ]; do sleep 0.05; done; exit 0;; esac\nexec sleep 60");
    std::fs::write(&engine, script).unwrap();
    state.db.lock().unwrap().execute("INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES('iptv:1:2',1,'2','Fixture East')",[]).unwrap();
    let probe = tools_dir.path().join("ffprobe");
    let script = std::fs::read_to_string(&probe).unwrap().replacen(
        "#!/bin/sh\n",
        "#!/bin/sh\nfor input do :; done\ncase \"$input\" in */2.ts) sleep 0.5;; esac\n",
        1,
    );
    std::fs::write(probe, script).unwrap();
    let app = router(state.clone(), None);
    let (_,channel)=request(&app,"POST","/api/lineup",json!({"name":"Fixture East","network":"Fixture","feed":"east","market":"","category":"Kids","number":1,"enabled":true,"candidates":[{"id":raw,"name":"Fixture East","verified":true},{"id":"iptv:1:2","name":"Fixture East","verified":true}]})).await;
    let (status, first) = request(
        &app,
        "POST",
        "/api/playback",
        json!({"channel_id":channel["id"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["managed_live"], true);
    let (status, second) = request(
        &app,
        "POST",
        "/api/playback",
        json!({"channel_id":channel["id"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    let logical = first["id"].as_str().unwrap();
    let mut reported_during_recovery = false;
    std::fs::write(tools_dir.path().join("ffmpeg.disconnect"), "end").unwrap();
    // Report EOF immediately, before the supervisor necessarily sees it.
    let (status, early) = request(
        &app,
        "POST",
        &format!("/api/playback/{logical}/recover"),
        json!({"generation":1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{early}");
    assert_ne!(early["state"], "failed");
    let next = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (status, heartbeat) = request(
                &app,
                "POST",
                &format!("/api/playback/{logical}/heartbeat"),
                json!({}),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{heartbeat}");
            if heartbeat["state"] == "recovering" && !reported_during_recovery {
                let (status, response) = request(
                    &app,
                    "POST",
                    &format!("/api/playback/{logical}/recover"),
                    json!({"generation":1}),
                )
                .await;
                assert_eq!(status, StatusCode::OK, "{response}");
                assert_ne!(
                    response["state"], "failed",
                    "A decoder EOF during shared recovery must keep its viewer attached"
                );
                reported_during_recovery = true;
            }
            if heartbeat["generation"] == 2 && heartbeat["state"] == "playing" {
                break heartbeat["playback"].clone();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert!(reported_during_recovery);
    assert_eq!(next["id"], logical);
    assert_eq!(next["channel_id"], channel["id"]);
    assert_ne!(next["url"], first["url"]);
    assert_eq!(
        request(&app, "GET", first["url"].as_str().unwrap(), Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    request(
        &app,
        "POST",
        &format!("/api/playback/{logical}/recover"),
        json!({"generation":1}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert_eq!(
        request(
            &app,
            "POST",
            &format!("/api/playback/{logical}/heartbeat"),
            json!({})
        )
        .await
        .1["generation"],
        2,
        "A delayed fault from the old generation must not interrupt its healthy replacement"
    );
    assert_eq!(state.playback.active_count().await, 1);
    assert_eq!(
        request(
            &app,
            "DELETE",
            &format!("/api/playback/{logical}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(state.playback.active_count().await, 1);
    request(
        &app,
        "DELETE",
        &format!("/api/playback/{}", second["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(state.playback.active_count().await, 0);
    state.playback.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn stopping_recovery_reaps_replacement_before_releasing_the_account() {
    let (state, _media, tools_dir, raw) = live_session_fixture(false);
    let probe = tools_dir.path().join("ffprobe");
    let script=std::fs::read_to_string(&probe).unwrap().replacen("#!/bin/sh\n","#!/bin/sh\nfor input do :; done\ncase \"$input\" in */2.ts) printf started > \"$0.replacement\"; exec sleep 60;; esac\n",1);
    std::fs::write(&probe, script).unwrap();
    state.db.lock().unwrap().execute("INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES('iptv:1:2',1,'2','Fixture East')",[]).unwrap();
    let app = router(state.clone(), None);
    let (_,channel)=request(&app,"POST","/api/lineup",json!({"name":"Fixture East","network":"Fixture","feed":"east","market":"","category":"Kids","number":1,"enabled":true,"candidates":[{"id":raw,"name":"Fixture East","verified":true},{"id":"iptv:1:2","name":"Fixture East","verified":true}]})).await;
    let (status, first) = request(
        &app,
        "POST",
        "/api/playback",
        json!({"channel_id":channel["id"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let logical = first["id"].as_str().unwrap();
    assert_eq!(
        request(
            &app,
            "POST",
            &format!("/api/playback/{logical}/recover"),
            json!({"generation":1})
        )
        .await
        .0,
        StatusCode::OK
    );
    tokio::time::timeout(Duration::from_secs(3), async {
        while !tools_dir.path().join("ffprobe.replacement").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(2),
            request(
                &app,
                "DELETE",
                &format!("/api/playback/{logical}"),
                Value::Null
            )
        )
        .await
        .unwrap()
        .0,
        StatusCode::OK
    );
    assert_eq!(state.playback.active_count().await, 0);
    let permit = state
        .providers
        .acquire_playback_for_kind(1, "live")
        .await
        .expect("Replacement child reaped before stop completes");
    drop(permit);
    state.playback.shutdown().await;
}

#[tokio::test]
async fn family_recovery_controls_are_validated_and_persisted() {
    let (state, _media, _tools, _) = live_session_fixture(false);
    let app = router(state.clone(), None);
    let (_, initial) = request(&app, "GET", "/api/lineup", Value::Null).await;
    assert_eq!(initial["settings"]["recovery"]["stall_seconds"], 20);
    let policy =
        json!({"stall_seconds":30,"attempt_seconds":15,"deadline_seconds":40,"max_recoveries":3});
    let (status, saved) = request(
        &app,
        "PATCH",
        "/api/lineup/settings",
        json!({"enabled":false,"limit":100,"recovery":policy}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["recovery"], policy);
    for invalid in [
        json!({"stall_seconds":0,"attempt_seconds":15,"deadline_seconds":40,"max_recoveries":3}),
        json!({"stall_seconds":30,"attempt_seconds":41,"deadline_seconds":40,"max_recoveries":3}),
        json!({"stall_seconds":30,"attempt_seconds":15,"deadline_seconds":40,"max_recoveries":50}),
    ] {
        let (status, _) = request(
            &app,
            "PATCH",
            "/api/lineup/settings",
            json!({"enabled":true,"limit":100,"recovery":invalid}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let (_, current) = request(&app, "GET", "/api/lineup", Value::Null).await;
    assert_eq!(current["settings"]["recovery"], policy);
    assert_eq!(current["settings"]["enabled"], false);
    state.playback.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn frozen_family_output_recovers_and_exhausts_the_shared_budget() {
    let (state, _media, _tools, raw) = live_session_fixture(false);
    state.db.lock().unwrap().execute("INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES('iptv:1:2',1,'2','Fixture East')",[]).unwrap();
    let app = router(state.clone(), None);
    let (status,_) = request(&app,"PATCH","/api/lineup/settings",json!({"enabled":false,"limit":100,"recovery":{"stall_seconds":5,"attempt_seconds":5,"deadline_seconds":10,"max_recoveries":1}})).await;
    assert_eq!(status, StatusCode::OK);
    let (_,channel)=request(&app,"POST","/api/lineup",json!({"name":"Fixture East","network":"Fixture","feed":"east","market":"","category":"Kids","number":1,"enabled":true,"candidates":[{"id":raw,"name":"Fixture East","verified":true},{"id":"iptv:1:2","name":"Fixture East","verified":true}]})).await;
    let (status, first) = request(
        &app,
        "POST",
        "/api/playback",
        json!({"channel_id":channel["id"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let logical = first["id"].as_str().unwrap();
    let mut generations = std::collections::HashSet::new();
    let terminal = tokio::time::timeout(Duration::from_secs(14), async {
        loop {
            let (status, h) = request(
                &app,
                "POST",
                &format!("/api/playback/{logical}/heartbeat"),
                json!({}),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{h}");
            generations.insert(h["generation"].as_u64().unwrap());
            if h["state"] == "failed" {
                break h;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("Frozen media must exhaust its configured budget even while the client heartbeats");
    assert_eq!(terminal["generation"], 2);
    assert_eq!(terminal["reason"], "recovery_budget_exhausted");
    assert!(generations.contains(&1) && generations.contains(&2));
    assert_eq!(state.playback.active_count().await, 0);
    request(
        &app,
        "DELETE",
        &format!("/api/playback/{logical}"),
        Value::Null,
    )
    .await;
    state.playback.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn recovery_never_retries_an_input_that_failed_during_initial_startup() {
    let (state, _media, tools, raw) = live_session_fixture(false);
    let probe = tools.path().join("ffprobe");
    let script=std::fs::read_to_string(&probe).unwrap().replacen("#!/bin/sh\n","#!/bin/sh\nfor input do :; done\nprintf '%s\\n' \"$input\" >> \"$0.inputs\"\ncase \"$input\" in */1.ts) exit 1;; esac\n",1);
    std::fs::write(&probe, script).unwrap();
    state.db.lock().unwrap().execute_batch("INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES('iptv:1:2',1,'2','Fixture East'),('iptv:1:3',1,'3','Fixture East');").unwrap();
    let app = router(state.clone(), None);
    let (_,channel)=request(&app,"POST","/api/lineup",json!({"name":"Fixture East","network":"Fixture","feed":"east","market":"","category":"Kids","number":1,"enabled":true,"candidates":[{"id":raw,"name":"Fixture East","verified":true},{"id":"iptv:1:2","name":"Fixture East","verified":true},{"id":"iptv:1:3","name":"Fixture East","verified":true}]})).await;
    let (status, first) = request(
        &app,
        "POST",
        "/api/playback",
        json!({"channel_id":channel["id"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let logical = first["id"].as_str().unwrap();
    request(
        &app,
        "POST",
        &format!("/api/playback/{logical}/recover"),
        json!({"generation":1}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let (_, h) = request(
                &app,
                "POST",
                &format!("/api/playback/{logical}/heartbeat"),
                json!({}),
            )
            .await;
            if h["generation"] == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let inputs = std::fs::read_to_string(tools.path().join("ffprobe.inputs")).unwrap();
    assert_eq!(
        inputs.lines().filter(|s| s.ends_with("/1.ts")).count(),
        1,
        "Failed primary must not be retried in another generation"
    );
    assert!(inputs.lines().any(|s| s.ends_with("/3.ts")));
    request(
        &app,
        "DELETE",
        &format!("/api/playback/{logical}"),
        Value::Null,
    )
    .await;
    assert_eq!(state.playback.active_count().await, 0);
    state.playback.shutdown().await;
}

#[tokio::test]
async fn bulk_xtream_import_validates_twenty_accounts_and_keeps_partial_results_private() {
    async fn login(
        axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
    ) -> axum::Json<Value> {
        axum::Json(
            json!({"user_info":{"auth":if q.get("password").is_some_and(|p|p=="bad-secret") {0}else{1},"status":"Active","max_connections":"2"}}),
        )
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream = tokio::spawn(async move {
        axum::serve(listener, Router::new().route("/player_api.php", get(login)))
            .await
            .unwrap()
    });
    let (app, _dir) = app();
    let entries:Vec<_>=(0..20).map(|i|json!({"name":format!("Account {i}"),"url":format!("http://{address}"),"username":format!("private-login-{i}"),"password":"private-password"})).collect();
    let (status, result) = request(
        &app,
        "POST",
        "/api/providers/import",
        json!({"entries":entries}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["results"].as_array().unwrap().len(), 20);
    assert!(result["results"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["status"] == "imported"));
    let (_, list) = request(&app, "GET", "/api/providers", Value::Null).await;
    assert_eq!(list.as_array().unwrap().len(), 20);
    assert!(list
        .as_array()
        .unwrap()
        .iter()
        .all(|p| p["enable_live"] == true
            && p["enable_movies"] == false
            && p["enable_series"] == false));
    assert!(!format!("{result}{list}").contains("private-"));
    let urls=format!("http://{address}/get.php?username=private-extra&password=private-password&type=m3u\nhttp://{address}/player_api.php?username=private-invalid&password=bad-secret\nhttp://{address}/get.php?username=private-login-0&password=private-password");
    let (status, partial) = request(
        &app,
        "POST",
        "/api/providers/import",
        json!({"entries":urls}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{partial}");
    assert_eq!(partial["results"][0]["status"], "imported");
    assert_eq!(partial["results"][1]["status"], "validation_failed");
    assert_eq!(partial["results"][2]["status"], "duplicate");
    assert!(!partial.to_string().contains("private-"));
    assert!(!partial.to_string().contains("bad-secret"));
    assert_eq!(
        request(&app, "GET", "/api/providers", Value::Null)
            .await
            .1
            .as_array()
            .unwrap()
            .len(),
        21
    );
    upstream.abort();
    let _ = upstream.await;
}

#[tokio::test]
async fn credential_renewal_preserves_provider_identity_scopes_and_imported_channels() {
    async fn login(
        axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
    ) -> axum::Json<Value> {
        axum::Json(
            json!({"user_info":{"auth":if q.get("password").is_some_and(|p|p=="renewed-secret") {1}else{0},"status":"Active"}}),
        )
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream = tokio::spawn(async move {
        axum::serve(listener, Router::new().route("/player_api.php", get(login)))
            .await
            .unwrap()
    });
    let (state, _dir) = app_state();
    let app = router(state.clone(), None);
    let (_,provider)=request(&app,"POST","/api/providers",json!({"name":"Existing","url":format!("http://{address}"),"username":"private-owner-login","password":"expired-secret","enable_live":true,"enable_movies":false,"enable_series":true})).await;
    let id = provider["id"].as_i64().unwrap();
    let raw = format!("iptv:{id}:1");
    state.db.lock().unwrap().execute("INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES(?1,?2,'1','Fixture East')",rusqlite::params![raw,id]).unwrap();
    let (_,channel)=request(&app,"POST","/api/lineup",json!({"name":"Fixture East","network":"Fixture","feed":"east","market":"","category":"Kids","number":1,"enabled":true,"candidates":[{"id":raw,"name":"Fixture East","verified":true}]})).await;
    let (status, renewed) = request(
        &app,
        "POST",
        &format!("/api/providers/{id}/credentials"),
        json!({"password":"renewed-secret"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{renewed}");
    assert_eq!(renewed["id"], id);
    assert_eq!(renewed["enable_movies"], false);
    assert_eq!(renewed["enable_series"], true);
    assert!(!renewed.to_string().contains("private-owner-login"));
    assert!(!renewed.to_string().contains("secret"));
    let (_, saved) = request(&app, "GET", "/api/lineup", Value::Null).await;
    assert_eq!(saved["channels"][0]["id"], channel["id"]);
    assert_eq!(saved["channels"][0]["candidates"][0]["id"], raw);
    let path = format!("/api/providers/{id}");
    request(&app, "PATCH", &path, json!({"enabled":false})).await;
    let (status, _) = request(
        &app,
        "POST",
        &format!("{path}/credentials"),
        json!({"password":"renewed-secret"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        request(&app, "GET", "/api/providers", Value::Null).await.1[0]["enabled"],
        false
    );
    request(&app, "PATCH", &path, json!({"enabled":true})).await;
    assert_eq!(
        request(&app, "GET", "/api/live?limit=1", Value::Null)
            .await
            .1["channels"][0]["id"],
        raw
    );
    assert_eq!(request(&app,"POST","/api/providers",json!({"name":"Duplicate","url":format!("http://{address}/player_api.php"),"username":"private-owner-login","password":"renewed-secret"})).await.0,StatusCode::BAD_REQUEST);
    upstream.abort();
    let _ = upstream.await;
    state.playback.shutdown().await;
}

#[tokio::test]
async fn bulk_accounts_bound_workers_and_metadata_and_enforce_owner_access() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let hits = Arc::new(AtomicUsize::new(0));
    let counters = (active.clone(), peak.clone(), hits.clone());
    let upstream_router = Router::new().route(
        "/player_api.php",
        get(
            move |axum::extract::Query(q): axum::extract::Query<
                std::collections::HashMap<String, String>,
            >| {
                let (active, peak, hits) = counters.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    let data = match q.get("action").map(String::as_str) {
                        Some("get_live_streams") => json!((1..=5)
                            .map(|i| json!({"stream_id":i,"name":format!("Fixture {i}")}))
                            .collect::<Vec<_>>()),
                        Some(_) => json!([]),
                        None if q.get("username").is_some_and(|u| u == "oversized") => {
                            json!({"user_info":{"auth":1},"padding":"s".repeat(256*1024)})
                        }
                        None => json!({"user_info":{"auth":1,"status":"Active"}}),
                    };
                    axum::Json(data)
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream =
        tokio::spawn(async move { axum::serve(listener, upstream_router).await.unwrap() });
    let (state, _dir) = app_state();
    let app = router(state.clone(), None);
    let entry = json!({"url":format!("http://{address}"),"username":"one","password":"private"});
    assert_eq!(
        request(
            &app,
            "POST",
            "/api/providers/import",
            json!({"entries":vec![entry.clone();21]})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request_as(
            &app,
            "POST",
            "/api/providers/import",
            json!({"entries":[entry.clone()]}),
            "invalid-session"
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    state
        .db
        .lock()
        .unwrap()
        .execute("UPDATE auth_accounts SET role='member' WHERE id=1", [])
        .unwrap();
    assert_eq!(
        request(
            &app,
            "POST",
            "/api/providers/import",
            json!({"entries":[entry.clone()]})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    state
        .db
        .lock()
        .unwrap()
        .execute("UPDATE auth_accounts SET role='owner' WHERE id=1", [])
        .unwrap();
    let mut entries: Vec<_> = (0..6)
        .map(|i| {
            let mut v = entry.clone();
            v["username"] = json!(format!("login-{i}"));
            v
        })
        .collect();
    let mut large = entry;
    large["username"] = json!("oversized");
    entries.push(large);
    let (status, result) = request(
        &app,
        "POST",
        "/api/providers/import",
        json!({"entries":entries}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["results"][6]["status"], "validation_failed");
    assert!((2..=4).contains(&peak.load(Ordering::SeqCst)));
    let id = result["results"][0]["provider_id"].as_i64().unwrap();
    assert_eq!(
        request(
            &app,
            "POST",
            &format!("/api/providers/{id}/sync"),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, catalog) = request(&app, "GET", "/api/live?limit=2", Value::Null).await;
    assert_eq!(catalog["channels"].as_array().unwrap().len(), 2);
    assert_eq!(catalog["total"], 5);
    upstream.abort();
    let _ = upstream.await;
    state.playback.shutdown().await;
}

#[tokio::test]
async fn renewed_credentials_discard_obsolete_catalog_and_guide_fetches() {
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };
    let entered = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(AtomicBool::new(false));
    let signals = (entered.clone(), release.clone());
    let mock=Router::new().route("/player_api.php",get(move |axum::extract::Query(q):axum::extract::Query<std::collections::HashMap<String,String>>| {
        let(entered,release)=signals.clone();async move {
            let old=q.get("password").is_some_and(|p|p=="old-password");
            let action=q.get("action").map(String::as_str).unwrap_or("");
            if old && (action=="get_live_streams" || action=="get_short_epg") {
                entered.fetch_add(1,Ordering::SeqCst);
                while !release.load(Ordering::SeqCst) {tokio::time::sleep(Duration::from_millis(5)).await;}
            }
            axum::Json(match action {
                ""=>json!({"user_info":{"auth":1,"status":"Active"}}),
                "get_live_streams"=>json!([{"stream_id":1,"name":if old {"Obsolete channel"}else{"Renewed channel"}}]),
                "get_short_epg"=>json!({"epg_listings":[{"id":"fixture","title":if old {"T2xkIEd1aWRl"}else{"TmV3IEd1aWRl"},"start_timestamp":"4102440000","stop_timestamp":"4102444800"}]}),
                _=>json!([]),
            })
        }
    }));
    let (address, upstream) = serve_upstream(mock).await;
    let (state, _dir) = app_state();
    let app = router(state.clone(), None);
    let(_,provider)=request(&app,"POST","/api/providers",json!({"name":"Existing","url":address,"username":"login","password":"old-password","enable_movies":false,"enable_series":false})).await;
    let id = provider["id"].as_i64().unwrap();
    let raw = format!("iptv:{id}:1");
    state.db.lock().unwrap().execute("INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES(?1,?2,'1','Original channel')",rusqlite::params![raw,id]).unwrap();
    let old_app = app.clone();
    let sync_path = format!("/api/providers/{id}/sync");
    let old_path = sync_path.clone();
    let old_sync =
        tokio::spawn(async move { request(&old_app, "POST", &old_path, Value::Null).await });
    let old_app = app.clone();
    let guide_path = format!("/api/guide/{raw}");
    let old_path = guide_path.clone();
    let old_guide =
        tokio::spawn(async move { request(&old_app, "GET", &old_path, Value::Null).await });
    tokio::time::timeout(Duration::from_secs(1), async {
        while entered.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        request(
            &app,
            "POST",
            &format!("/api/providers/{id}/credentials"),
            json!({"password":"new-password"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, "POST", &sync_path, Value::Null).await.0,
        StatusCode::OK
    );
    release.store(true, Ordering::SeqCst);
    assert_eq!(
        old_sync.await.unwrap().0,
        StatusCode::BAD_REQUEST,
        "Old login must not overwrite the refreshed catalog"
    );
    assert_eq!(
        old_guide.await.unwrap().0,
        StatusCode::BAD_REQUEST,
        "Old login must not repopulate the guide cache"
    );
    assert_eq!(
        request(&app, "GET", "/api/live", Value::Null).await.1["channels"][0]["name"],
        "Renewed channel"
    );
    assert!(request(&app, "GET", &guide_path, Value::Null)
        .await
        .1
        .to_string()
        .contains("New Guide"));
    upstream.abort();
    let _ = upstream.await;
    state.playback.shutdown().await;
}

#[tokio::test]
async fn shared_account_pools_reconcile_duplicates_and_reserve_one_final_slot() {
    let (state, _dir) = app_state();
    state.db.lock().unwrap().execute_batch("INSERT INTO providers(id,name,url,username,password,max_connections) VALUES(1,'Primary','http://same.example','same','password',3),(2,'Legacy duplicate','http://same.example/player_api.php','same','password',2),(3,'Alias','http://alias.example','alias','password',2);").unwrap();
    let app = router(state.clone(), None);
    let (status, pools) = request(&app, "GET", "/api/account-pools", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{pools}");
    let group = pools["pools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["provider_ids"].as_array().unwrap().contains(&json!(1)))
        .unwrap();
    assert_eq!(group["provider_ids"], json!([1, 2]));
    assert_eq!(group["configured_limit"], 2);
    let pool = group["id"].as_i64().unwrap();
    let first = state
        .providers
        .acquire_playback_for_kind(1, "live")
        .await
        .unwrap();
    let second = state
        .providers
        .acquire_playback_for_kind(2, "series")
        .await
        .unwrap();
    assert!(state
        .providers
        .acquire_playback_for_kind(1, "movie")
        .await
        .is_err());
    assert_eq!(
        request(
            &app,
            "PATCH",
            "/api/providers/3/pool",
            json!({"pool_id":pool})
        )
        .await
        .0,
        StatusCode::CONFLICT,
        "Regrouping must preserve active reservations"
    );
    drop((first, second));
    assert_eq!(
        request(
            &app,
            "PATCH",
            "/api/providers/3/pool",
            json!({"pool_id":pool})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "PATCH",
            &format!("/api/account-pools/{pool}"),
            json!({"configured_limit":2,"external_reserve":1})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (one, two) = tokio::join!(
        state.providers.acquire_playback_for_kind(1, "live"),
        state.providers.acquire_playback_for_kind(3, "movie")
    );
    assert_eq!(
        usize::from(one.is_ok()) + usize::from(two.is_ok()),
        1,
        "Only one contender gets the final shared slot"
    );
    let (_, current) = request(&app, "GET", "/api/account-pools", Value::Null).await;
    let group = current["pools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == pool)
        .unwrap();
    assert_eq!(group["local_reservations"], 1);
    assert_eq!(group["estimated_free"], 0);
    drop((one, two));
    let (_, current) = request(&app, "GET", "/api/account-pools", Value::Null).await;
    let group = current["pools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == pool)
        .unwrap();
    assert_eq!(group["local_reservations"], 0);
    assert_eq!(group["estimated_free"], 1);
    assert_eq!(
        request(&app, "GET", "/api/providers", Value::Null)
            .await
            .1
            .as_array()
            .unwrap()
            .len(),
        3
    );
    state.playback.shutdown().await;
}

#[tokio::test]
async fn account_reports_lower_allowances_without_double_counting_local_use() {
    use std::sync::{Arc, Mutex};
    let report = Arc::new(Mutex::new(
        json!({"auth":1,"status":"Active","max_connections":"3","active_cons":"1"}),
    ));
    let source = report.clone();
    let mock = Router::new().route(
        "/player_api.php",
        get(move || {
            let source = source.clone();
            async move { axum::Json(json!({"user_info":source.lock().unwrap().clone()})) }
        }),
    );
    let (address, upstream) = serve_upstream(mock).await;
    let (state, _dir) = app_state();
    let app = router(state.clone(), None);
    let(_,p)=request(&app,"POST","/api/providers",json!({"name":"Account","url":address,"username":"private","password":"secret","max_connections":4})).await;
    let id = p["id"].as_i64().unwrap();
    let first = state
        .providers
        .acquire_playback_for_kind(id, "movie")
        .await
        .unwrap();
    let (status, observed) = request(
        &app,
        "POST",
        &format!("/api/providers/{id}/status"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{observed}");
    assert_eq!(observed["effective_limit"], 3);
    assert_eq!(observed["local_reservations"], 1);
    assert_eq!(observed["reported_usage"], 1);
    assert_eq!(
        observed["estimated_free"], 2,
        "Reported usage includes this existing local viewer"
    );
    let second = state
        .providers
        .acquire_playback_for_kind(id, "live")
        .await
        .unwrap();
    let third = state
        .providers
        .acquire_playback_for_kind(id, "series")
        .await
        .unwrap();
    assert!(state.providers.acquire_playback(id).await.is_err());
    *report.lock().unwrap() =
        json!({"auth":1,"status":"Active","max_connections":"2","active_cons":"3"});
    let (_, lower) = request(
        &app,
        "POST",
        &format!("/api/providers/{id}/status"),
        Value::Null,
    )
    .await;
    assert_eq!(lower["effective_limit"], 2);
    assert_eq!(lower["local_reservations"], 3);
    assert_eq!(lower["estimated_free"], 0);
    drop((first, second, third));
    *report.lock().unwrap() =
        json!({"auth":1,"status":"Active","max_connections":"0","active_cons":"0"});
    let (_, invalid_limit) = request(
        &app,
        "POST",
        &format!("/api/providers/{id}/status"),
        Value::Null,
    )
    .await;
    assert_eq!(
        invalid_limit["effective_limit"], 2,
        "Zero does not erase the last valid allowance or mean unlimited"
    );
    assert_eq!(invalid_limit["estimated_free"], 2);
    assert!(!invalid_limit.to_string().contains("secret"));
    upstream.abort();
    let _ = upstream.await;
    state.playback.shutdown().await;
}

#[tokio::test]
async fn stale_account_reports_never_raise_estimated_headroom() {
    let (state, _dir) = app_state();
    state.db.lock().unwrap().execute_batch("INSERT INTO providers(id,name,url,username,password,max_connections) VALUES(1,'Account','http://fixture.example','private','secret',4);
        INSERT INTO account_pools(id,name,configured_limit,external_reserve) VALUES(1,'Account',4,0);
        INSERT INTO provider_pools(provider_id,pool_id) VALUES(1,1);
        INSERT INTO account_observations(pool_id,reported_limit,limit_at,reported_usage,usage_at,external_estimate) VALUES(1,2,1,2,1,2);").unwrap();
    let app = router(state.clone(), None);
    let (status, pools) = request(&app, "GET", "/api/account-pools", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{pools}");
    let pool = &pools["pools"][0];
    assert_eq!(pool["confidence"], "stale");
    assert_eq!(pool["effective_limit"], 2);
    assert_eq!(pool["estimated_free"], 0);
    assert!(pool["usage_age_seconds"].as_i64().unwrap() > 60);
    assert!(state
        .providers
        .acquire_playback_for_kind(1, "live")
        .await
        .is_err());
    state.playback.shutdown().await;
}

#[tokio::test]
async fn external_reserve_is_not_counted_twice_when_already_used_outside_viptv() {
    let (state, _dir) = app_state();
    state.db.lock().unwrap().execute_batch("INSERT INTO providers(id,name,url,username,password,max_connections) VALUES(1,'Account','http://fixture.example','private','secret',3);
        INSERT INTO account_pools(id,name,configured_limit,external_reserve) VALUES(1,'Account',3,1);
        INSERT INTO provider_pools(provider_id,pool_id) VALUES(1,1);
        INSERT INTO account_observations(pool_id,reported_limit,limit_at,reported_usage,usage_at,external_estimate) VALUES(1,3,strftime('%s','now'),1,strftime('%s','now'),1);").unwrap();
    let app = router(state.clone(), None);
    let (_, pools) = request(&app, "GET", "/api/account-pools", Value::Null).await;
    assert_eq!(
        pools["pools"][0]["estimated_free"], 2,
        "One externally used slot already satisfies the one-slot external reserve"
    );
    let first = state.providers.acquire_playback(1).await.unwrap();
    let second = state.providers.acquire_playback(1).await.unwrap();
    assert!(state.providers.acquire_playback(1).await.is_err());
    drop((first, second));
    state.playback.shutdown().await;
}

#[tokio::test]
async fn late_account_report_cannot_replace_a_newer_full_usage_observation() {
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };
    let entered = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(AtomicBool::new(false));
    let signals = (entered.clone(), release.clone());
    let mock=Router::new().route("/player_api.php",get(move ||{let(entered,release)=signals.clone();async move {
        let first=entered.fetch_add(1,Ordering::SeqCst)==0;
        if first {while !release.load(Ordering::SeqCst) {tokio::time::sleep(Duration::from_millis(5)).await;}}
        axum::Json(json!({"user_info":{"auth":1,"status":"Active","max_connections":"1","active_cons":if first {"0"}else{"1"}}}))
    }}));
    let (address, upstream) = serve_upstream(mock).await;
    let (state, _dir) = app_state();
    let app = router(state.clone(), None);
    let (_, provider) = request(
        &app,
        "POST",
        "/api/providers",
        json!({"name":"Account","url":address,"username":"private","password":"secret"}),
    )
    .await;
    let id = provider["id"].as_i64().unwrap();
    let path = format!("/api/providers/{id}/status");
    let old_app = app.clone();
    let old_path = path.clone();
    let old = tokio::spawn(async move { request(&old_app, "POST", &old_path, Value::Null).await });
    tokio::time::timeout(Duration::from_secs(1), async {
        while entered.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        request(&app, "POST", &path, Value::Null).await.1["estimated_free"],
        0
    );
    release.store(true, Ordering::SeqCst);
    assert_eq!(old.await.unwrap().0, StatusCode::BAD_REQUEST);
    assert!(
        state.providers.acquire_playback(id).await.is_err(),
        "Old zero-usage reply must not reopen the final slot"
    );
    upstream.abort();
    let _ = upstream.await;
    state.playback.shutdown().await;
}

#[tokio::test]
async fn regrouping_accounts_preserves_known_reported_limits_and_external_usage() {
    let (state, _dir) = app_state();
    state.db.lock().unwrap().execute_batch("INSERT INTO providers(id,name,url,username,password,max_connections) VALUES(1,'One','http://one.example','private','secret',4),(2,'Alias','http://alias.example','alias','secret',4);
        INSERT INTO account_pools(id,name,configured_limit,external_reserve) VALUES(1,'One',4,1),(2,'Alias',4,0);
        INSERT INTO provider_pools(provider_id,pool_id) VALUES(1,1),(2,2);
        INSERT INTO account_observations(pool_id,reported_limit,limit_at,reported_usage,usage_at,external_estimate) VALUES(1,1,1,1,1,1);").unwrap();
    let app = router(state.clone(), None);
    let (status, single) = request(
        &app,
        "PATCH",
        "/api/providers/1/pool",
        json!({"pool_id":null}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{single}");
    assert_eq!(
        single["id"], 1,
        "Already separate subscription keeps its identity"
    );
    assert_eq!(single["estimated_free"], 0);
    let (status, merged) =
        request(&app, "PATCH", "/api/providers/1/pool", json!({"pool_id":2})).await;
    assert_eq!(status, StatusCode::OK, "{merged}");
    assert_eq!(merged["effective_limit"], 1);
    assert_eq!(merged["external_reserve"], 1);
    assert_eq!(merged["reported_usage"], 1);
    assert_eq!(merged["estimated_free"], 0);
    assert!(state.providers.acquire_playback(1).await.is_err());
    assert!(state.providers.acquire_playback(2).await.is_err());
    let (status, split) = request(
        &app,
        "PATCH",
        "/api/providers/1/pool",
        json!({"pool_id":null}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{split}");
    assert_eq!(split["effective_limit"], 1);
    assert_eq!(split["estimated_free"], 0);
    assert!(state.providers.acquire_playback(1).await.is_err());
    state.db.lock().unwrap().execute("UPDATE account_observations SET reported_usage=NULL,usage_at=NULL,external_estimate=NULL", []).unwrap();
    let (status, unknown) =
        request(&app, "PATCH", "/api/providers/1/pool", json!({"pool_id":2})).await;
    assert_eq!(status, StatusCode::OK, "{unknown}");
    assert!(
        unknown["reported_usage"].is_null(),
        "Regrouping must not invent a zero-usage report"
    );
    assert_eq!(unknown["confidence"], "unknown");
    state.playback.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn family_selection_uses_absolute_free_slots_and_keeps_healthy_viewers() {
    let (state, _media, _tools, raw) = live_session_fixture(false);
    state
        .providers
        .update(1, json!({"max_connections":4}))
        .unwrap();
    state.db.lock().unwrap().execute_batch("INSERT INTO providers(id,name,url,username,password,max_connections) VALUES(2,'More free','http://second.invalid','fixture','fixture',3);
        INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES('iptv:2:1',2,'1','Fixture East');").unwrap();
    let mut occupied = Vec::new();
    for _ in 0..3 {
        occupied.push(
            state
                .providers
                .acquire_playback_for_kind(1, "movie")
                .await
                .unwrap(),
        );
    }
    let app = router(state.clone(), None);
    let (_,channel)=request(&app,"POST","/api/lineup",json!({"name":"Fixture East","network":"Fixture","feed":"east","market":"","category":"Kids","number":1,"enabled":true,"candidates":[{"id":raw,"name":"Fixture East","verified":true},{"id":"iptv:2:1","name":"Fixture East","verified":true}]})).await;
    let (status, first) = request(
        &app,
        "POST",
        "/api/playback",
        json!({"channel_id":channel["id"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let (_, lineup) = request(&app, "GET", "/api/lineup", Value::Null).await;
    let attempts = &lineup["channels"][0]["last_startup"]["attempts"];
    let selected = attempts
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["reason"] == "selected")
        .unwrap();
    assert_eq!(
        selected["candidate_id"], "iptv:2:1",
        "Three free slots must beat one free slot on the larger subscription"
    );
    assert_eq!(selected["estimated_free_before"], 3);
    assert!(!selected.to_string().contains("http"));
    drop(occupied);
    let logical = first["id"].as_str().unwrap();
    let (_, heartbeat) = request(
        &app,
        "POST",
        &format!("/api/playback/{logical}/heartbeat"),
        json!({}),
    )
    .await;
    assert_eq!(
        heartbeat["generation"], 1,
        "Changed ranking must not replace a playing viewer"
    );
    request(
        &app,
        "POST",
        &format!("/api/playback/{logical}/recover"),
        json!({"generation":1}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (_, heartbeat) = request(
                &app,
                "POST",
                &format!("/api/playback/{logical}/heartbeat"),
                json!({}),
            )
            .await;
            if heartbeat["generation"] == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    let (_, lineup) = request(&app, "GET", "/api/lineup", Value::Null).await;
    let selected = lineup["channels"][0]["last_startup"]["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["reason"] == "selected")
        .unwrap();
    assert_eq!(selected["candidate_id"], raw);
    assert_eq!(selected["estimated_free_before"], 4);
    request(
        &app,
        "DELETE",
        &format!("/api/playback/{logical}"),
        Value::Null,
    )
    .await;
    state.playback.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn concurrent_family_selection_reserves_each_final_slot_once() {
    let (state, _media, _tools, raw) = live_session_fixture(false);
    state.db.lock().unwrap().execute_batch("INSERT INTO providers(id,name,url,username,password,max_connections) VALUES(2,'Second','http://second.invalid','fixture','fixture',1);
        INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES('iptv:2:1',2,'1','Fixture East');").unwrap();
    let app = router(state.clone(), None);
    let (_,channel)=request(&app,"POST","/api/lineup",json!({"name":"Fixture East","network":"Fixture","feed":"east","market":"","category":"Kids","number":1,"enabled":true,"candidates":[{"id":raw,"name":"Fixture East","verified":true},{"id":"iptv:2:1","name":"Fixture East","verified":true}]})).await;
    let (first, second) = tokio::join!(
        request(
            &app,
            "POST",
            "/api/playback",
            json!({"channel_id":channel["id"]})
        ),
        request(
            &app,
            "POST",
            "/api/playback",
            json!({"channel_id":channel["id"],"force_transcode":true})
        )
    );
    assert_eq!(first.0, StatusCode::OK, "{}", first.1);
    assert_eq!(second.0, StatusCode::OK, "{}", second.1);
    let (_, pools) = request(&app, "GET", "/api/account-pools", Value::Null).await;
    for pool in pools["pools"].as_array().unwrap() {
        assert_eq!(pool["local_reservations"], 1);
        assert_eq!(pool["estimated_free"], 0);
    }
    assert_eq!(
        request(
            &app,
            "POST",
            "/api/playback",
            json!({"channel_id":channel["id"],"audio_track_index":1})
        )
        .await
        .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    for session in [first.1, second.1] {
        request(
            &app,
            "DELETE",
            &format!("/api/playback/{}", session["id"].as_str().unwrap()),
            Value::Null,
        )
        .await;
    }
    let (_, pools) = request(&app, "GET", "/api/account-pools", Value::Null).await;
    assert!(pools["pools"]
        .as_array()
        .unwrap()
        .iter()
        .all(|p| p["local_reservations"] == 0));
    state.playback.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn family_selection_reconsiders_a_busy_backup_after_failed_preparation() {
    let (state, _media, tools_dir, raw) = live_session_fixture(false);
    state.db.lock().unwrap().execute_batch("INSERT INTO providers(id,name,url,username,password,max_connections) VALUES(2,'Second','http://second.invalid','fixture','fixture',1);
        INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES('iptv:2:2',2,'2','Fixture East');").unwrap();
    let probe = tools_dir.path().join("ffprobe");
    let script = std::fs::read_to_string(&probe).unwrap();
    std::fs::write(&probe,script.replacen("#!/bin/sh\n","#!/bin/sh\nfor input do :; done\ncase \"$input\" in */1.ts) touch \"$0.started\"; while [ ! -f \"$0.release\" ]; do sleep 0.02; done; exit 1;; esac\n",1)).unwrap();
    let occupied = state
        .providers
        .acquire_playback_for_kind(2, "movie")
        .await
        .unwrap();
    let app = router(state.clone(), None);
    let (_,channel)=request(&app,"POST","/api/lineup",json!({"name":"Fixture East","network":"Fixture","feed":"east","market":"","category":"Kids","number":1,"enabled":true,"candidates":[{"id":raw,"name":"Fixture East","verified":true},{"id":"iptv:2:2","name":"Fixture East","verified":true}]})).await;
    let pending_app = app.clone();
    let channel_id = channel["id"].clone();
    let pending = tokio::spawn(async move {
        request(
            &pending_app,
            "POST",
            "/api/playback",
            json!({"channel_id":channel_id}),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !tools_dir.path().join("ffprobe.started").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    drop(occupied);
    std::fs::write(tools_dir.path().join("ffprobe.release"), "release").unwrap();
    let (status, session) = pending.await.unwrap();
    assert_eq!(
        status,
        StatusCode::OK,
        "Newly free backup must be reconsidered: {session}"
    );
    let (_, lineup) = request(&app, "GET", "/api/lineup", Value::Null).await;
    let selected = lineup["channels"][0]["last_startup"]["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["reason"] == "selected")
        .unwrap();
    assert_eq!(selected["candidate_id"], "iptv:2:2");
    request(
        &app,
        "DELETE",
        &format!("/api/playback/{}", session["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    state.playback.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn family_selection_ties_use_recent_success_speed_and_owner_quality_order() {
    let (state, _media, _tools, raw) = live_session_fixture(false);
    state.db.lock().unwrap().execute_batch("INSERT INTO providers(id,name,url,username,password,max_connections) VALUES(2,'Second','http://second.invalid','fixture','fixture',1);
        INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES('iptv:2:1',2,'1','Fixture East');").unwrap();
    let app = router(state.clone(), None);
    let (_,channel)=request(&app,"POST","/api/lineup",json!({"name":"Fixture East","network":"Fixture","feed":"east","market":"","category":"Kids","number":1,"enabled":true,"candidates":[{"id":raw,"name":"Fixture East","verified":true},{"id":"iptv:2:1","name":"Fixture East","verified":true}]})).await;
    // Observe the second candidate through real preparation at the media-tool seam.
    let occupied = state.providers.acquire_playback(1).await.unwrap();
    let (status, session) = request(
        &app,
        "POST",
        "/api/playback",
        json!({"channel_id":channel["id"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{session}");
    request(
        &app,
        "DELETE",
        &format!("/api/playback/{}", session["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    drop(occupied);
    let (status, session) = request(
        &app,
        "POST",
        "/api/playback",
        json!({"channel_id":channel["id"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{session}");
    let (_, lineup) = request(&app, "GET", "/api/lineup", Value::Null).await;
    assert_eq!(
        lineup["channels"][0]["last_startup"]["attempts"][0]["candidate_id"], "iptv:2:1",
        "Recent success wins an otherwise equal untested owner preference"
    );
    request(
        &app,
        "DELETE",
        &format!("/api/playback/{}", session["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    let occupied = state.providers.acquire_playback(2).await.unwrap();
    let (status, session) = request(
        &app,
        "POST",
        "/api/playback",
        json!({"channel_id":channel["id"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{session}");
    request(
        &app,
        "DELETE",
        &format!("/api/playback/{}", session["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    drop(occupied);
    for (sql,expected) in [
        ("UPDATE family_input_observations SET healthy=1,startup_ms=CASE live_id WHEN 'iptv:1:1' THEN 200 ELSE 100 END", "iptv:2:1"),
        ("UPDATE family_input_observations SET healthy=1,startup_ms=100", "iptv:1:1"),
        ("UPDATE family_input_observations SET healthy=CASE live_id WHEN 'iptv:1:1' THEN 0 ELSE 1 END", "iptv:2:1"),
    ] {
        state.db.lock().unwrap().execute(sql,[]).unwrap();
        let (status,session)=request(&app,"POST","/api/playback",json!({"channel_id":channel["id"]})).await;
        assert_eq!(status,StatusCode::OK,"{session}");
        let (_,lineup)=request(&app,"GET","/api/lineup",Value::Null).await;
        let selected=lineup["channels"][0]["last_startup"]["attempts"].as_array().unwrap().iter().find(|a|a["reason"]=="selected").unwrap();
        assert_eq!(selected["candidate_id"],expected);
        request(&app,"DELETE",&format!("/api/playback/{}",session["id"].as_str().unwrap()),Value::Null).await;
    }
    state.playback.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn automatic_family_matching_preserves_siblings_coasts_and_local_markets() {
    let (state, _media, _tools, _raw) = live_session_fixture(false);
    state
        .db
        .lock()
        .unwrap()
        .execute("DELETE FROM provider_live", [])
        .unwrap();
    let app = router(state.clone(), None);
    let identities = [
        ("HBO", "east", ""),
        ("HBO", "west", ""),
        ("HBO2", "east", ""),
        ("FX", "east", ""),
        ("FXX", "east", ""),
        ("FS1", "national", ""),
        ("FS2", "national", ""),
        ("WABC", "local", "New York"),
    ];
    let mut channels = Vec::new();
    for (index, (network, feed, market)) in identities.iter().enumerate() {
        let (status,channel)=request(&app,"POST","/api/lineup",json!({"name":format!("{network} {feed}"),"network":network,"feed":feed,"market":market,"category":"Family","number":index+1,"enabled":true,"candidates":[]})).await;
        assert_eq!(status, StatusCode::OK, "{channel}");
        channels.push(channel);
    }
    let names = [
        "US EN HBO EAST UHD",
        "US EN HBO WEST HD",
        "US EN HBO2 EAST HD",
        "US EN FX EAST HD",
        "US EN FXX EAST HD",
        "US EN FS1 HD",
        "US EN FS2 HD",
        "US EN WABC New York HD",
        "US EN HBO HD",
        "CA EN HBO EAST HD",
        "US ES HBO EAST HD",
        "US EN WABC Los Angeles HD",
    ];
    for (index, name) in names.iter().enumerate() {
        state
            .db
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES(?1,1,?2,?3)",
                rusqlite::params![
                    format!("iptv:1:{}", index + 1),
                    (index + 1).to_string(),
                    name
                ],
            )
            .unwrap();
    }
    state.db.lock().unwrap().execute_batch("INSERT INTO provider_live(id,provider_id,stream_id,name,category) VALUES('iptv:1:13',1,'13','US EN WABC','New York'),('iptv:1:14',1,'14','US EN New York WABC HD','');").unwrap();
    let (status, result) = request(&app, "POST", "/api/lineup/matching/run", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let (_, listing) = request(&app, "GET", "/api/lineup", Value::Null).await;
    assert_eq!(
        listing["channels"].as_array().unwrap().len(),
        8,
        "Inventory cannot expand the allowlist"
    );
    for (index, channel) in listing["channels"].as_array().unwrap().iter().enumerate() {
        assert_eq!(
            channel["candidates"].as_array().unwrap().len(),
            if index == 7 { 3 } else { 1 },
            "{channel}"
        );
        if index == 7 {
            let ids = channel["candidates"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["id"].as_str().unwrap())
                .collect::<std::collections::HashSet<_>>();
            assert_eq!(
                ids,
                std::collections::HashSet::from(["iptv:1:8", "iptv:1:13", "iptv:1:14"])
            );
        } else {
            assert_eq!(
                channel["candidates"][0]["id"],
                format!("iptv:1:{}", index + 1)
            );
        }
    }
    let (_, matches) = request(
        &app,
        "GET",
        &format!(
            "/api/lineup/matching?channel_id={}",
            channels[0]["id"].as_str().unwrap()
        ),
        Value::Null,
    )
    .await;
    assert!(matches["matches"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["candidate_id"] == "iptv:1:9" && m["status"] == "review"));
    let (status, session) = request(
        &app,
        "POST",
        "/api/playback",
        json!({"channel_id":channels[0]["id"]}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "Automatically associated media must play: {session}"
    );
    request(
        &app,
        "DELETE",
        &format!("/api/playback/{}", session["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    state.playback.shutdown().await;
}

#[tokio::test]
async fn family_matching_source_ids_are_scoped_and_cannot_override_sibling_identity() {
    let (state, _dir) = app_state();
    state.db.lock().unwrap().execute_batch("INSERT INTO providers(id,name,url,username,password) VALUES(1,'One','http://one.invalid','one','secret'),(2,'Two','http://two.invalid','two','secret');
        INSERT INTO provider_live(id,provider_id,stream_id,name,epg_channel_id) VALUES('iptv:1:1',1,'1','HBO HD','hbo.us'),('iptv:1:2',1,'2','Unlabeled feed','hbo.us'),('iptv:2:1',2,'1','Unlabeled feed','hbo.us'),('iptv:1:3',1,'3','US EN HBO 2 EAST HD','hbo.us');").unwrap();
    let app = router(state.clone(), None);
    let mut channels = Vec::new();
    for (number, network) in [(1, "HBO"), (2, "HBO2")] {
        let (status,c)=request(&app,"POST","/api/lineup",json!({"name":format!("{network} East"),"network":network,"feed":"east","market":"","category":"Movies","number":number,"enabled":true,"candidates":[]})).await;
        assert_eq!(status, StatusCode::OK, "{c}");
        channels.push(c);
    }
    let (status,result)=request(&app,"PATCH",&format!("/api/lineup/{}/matching",channels[0]["id"].as_str().unwrap()),json!({"candidate_id":"iptv:1:1","decision":"pin","observed_name":"HBO HD","verified":true})).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let (_, listing) = request(&app, "GET", "/api/lineup", Value::Null).await;
    let hbo = &listing["channels"][0]["candidates"];
    assert!(
        hbo.as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == "iptv:1:2"),
        "Verified source-scoped ID resolves the same provider's unmarked feed"
    );
    assert!(
        !hbo.as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == "iptv:2:1"),
        "Another provider's identical EPG ID is untrusted"
    );
    assert!(
        !hbo.as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == "iptv:1:3"),
        "Even a verified ID must preserve HBO versus HBO2"
    );
    assert_eq!(listing["channels"][1]["candidates"][0]["id"], "iptv:1:3");
    state
        .db
        .lock()
        .unwrap()
        .execute(
            "UPDATE providers SET url='http://replacement.invalid' WHERE id=1",
            [],
        )
        .unwrap();
    request(&app, "POST", "/api/lineup/matching/run", Value::Null).await;
    let (_, changed) = request(&app, "GET", "/api/lineup", Value::Null).await;
    assert!(
        !changed["channels"][0]["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == "iptv:1:2"),
        "Changed endpoint invalidates source-scoped ID trust"
    );
    state.playback.shutdown().await;
}

#[tokio::test]
async fn family_matching_keeps_diverse_reserves_and_durable_corrections() {
    let (state, _dir) = app_state();
    let app = router(state.clone(), None);
    for id in 1..=5 {
        state.db.lock().unwrap().execute("INSERT INTO providers(id,name,url,username,password) VALUES(?1,?2,'http://fixture.invalid',?2,'secret')",rusqlite::params![id,format!("Account {id}")]).unwrap();
        state.db.lock().unwrap().execute("INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES(?1,?2,'1','US EN Cartoon Network EAST HD')",rusqlite::params![format!("iptv:{id}:1"),id]).unwrap();
    }
    state.db.lock().unwrap().execute("INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES('iptv:1:2',1,'2','US EN Cartoon Network EAST UHD')",[]).unwrap();
    let (_,channel)=request(&app,"POST","/api/lineup",json!({"name":"Cartoon East","network":"Cartoon Network","feed":"east","market":"","category":"Kids","number":1,"enabled":true,"candidates":[]})).await;
    let id = channel["id"].as_str().unwrap();
    for provider in [1, 2] {
        assert_eq!(
            request(
                &app,
                "PATCH",
                &format!("/api/lineup/matching/groups/{provider}"),
                json!({"upstream_group":"Shared infrastructure"})
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    assert_eq!(
        request(&app, "POST", "/api/lineup/matching/run", Value::Null)
            .await
            .0,
        StatusCode::OK
    );
    let path = format!("/api/lineup/matching?channel_id={id}");
    let corrections = format!("/api/lineup/{id}/matching");
    let (_, result) = request(&app, "GET", &path, Value::Null).await;
    assert_eq!(result["settings"]["active_candidates"], 4);
    assert_eq!(result["channels"][0]["active"], 4);
    assert_eq!(result["channels"][0]["reserves"], 2);
    let active = result["matches"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["status"] == "active")
        .map(|m| m["provider_id"].as_i64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        active,
        vec![1, 3, 4, 5],
        "Prefer distinct accounts and known upstream groups"
    );
    let (status, _) = request(
        &app,
        "PATCH",
        &corrections,
        json!({"candidate_id":"iptv:1:1","decision":"reject"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status,_)=request(&app,"PATCH",&corrections,json!({"candidate_id":"iptv:3:1","decision":"pin","observed_name":"US EN Cartoon Network EAST HD","verified":true})).await;
    assert_eq!(status, StatusCode::OK);
    state
        .db
        .lock()
        .unwrap()
        .execute(
            "DELETE FROM provider_live WHERE id IN ('iptv:1:1','iptv:3:1')",
            [],
        )
        .unwrap();
    request(&app, "POST", "/api/lineup/matching/run", Value::Null).await;
    for provider in [1, 3] {
        state.db.lock().unwrap().execute("INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES(?1,?2,'1','US EN Cartoon Network EAST HD')",rusqlite::params![format!("iptv:{provider}:1"),provider]).unwrap();
    }
    request(&app, "POST", "/api/lineup/matching/run", Value::Null).await;
    let (_, result) = request(&app, "GET", &path, Value::Null).await;
    let rows = result["matches"].as_array().unwrap();
    assert!(rows.iter().any(|m| m["candidate_id"] == "iptv:1:1"
        && m["reason"] == "owner_rejection"
        && m["status"] == "rejected"));
    assert!(rows.iter().any(|m| m["candidate_id"] == "iptv:3:1"
        && m["pinned"] == true
        && m["status"] == "active"));
    assert_eq!(request(&app,"PATCH","/api/lineup/matching",json!({"confidence":95,"ambiguity_margin":10,"ambiguity_policy":"review","active_candidates":6})).await.0,StatusCode::OK);
    request(&app, "POST", "/api/lineup/matching/run", Value::Null).await;
    let (_, result) = request(&app, "GET", &path, Value::Null).await;
    assert_eq!(result["channels"][0]["shortage"], 1);
    assert!(!result.to_string().contains("secret"));
    assert!(!result.to_string().contains("http://"));
    state.playback.shutdown().await;
}

#[tokio::test]
async fn automatic_matches_remain_correctable_without_rewriting_canonical_history() {
    let (state, _dir) = app_state();
    let app = router(state.clone(), None);
    state.db.lock().unwrap().execute_batch("INSERT INTO providers(id,name,url,username,password) VALUES(1,'One','http://fixture.invalid','fixture','secret'); INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES('iptv:1:1',1,'1','US EN Cartoon Network EAST HD');").unwrap();
    let mut channels = Vec::new();
    for (number, network) in [(1, "Cartoon Network"), (2, "Boomerang")] {
        let(_,c)=request(&app,"POST","/api/lineup",json!({"name":network,"network":network,"feed":"east","market":"","category":"Kids","number":number,"enabled":true,"candidates":[]})).await;
        channels.push(c);
    }
    request(&app, "POST", "/api/lineup/matching/run", Value::Null).await;
    state
        .db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO favorites(profile_id,id,type,name) VALUES(1,?1,'live','Original channel')",
            [channels[0]["id"].as_str().unwrap()],
        )
        .unwrap();
    assert_eq!(
        request(
            &app,
            "PATCH",
            &format!(
                "/api/lineup/{}/matching",
                channels[0]["id"].as_str().unwrap()
            ),
            json!({"candidate_id":"iptv:1:1","decision":"reject"})
        )
        .await
        .0,
        StatusCode::OK
    );
    let(status,result)=request(&app,"PATCH",&format!("/api/lineup/{}/matching",channels[1]["id"].as_str().unwrap()),json!({"candidate_id":"iptv:1:1","decision":"pin","observed_name":"US EN Cartoon Network EAST HD","verified":true})).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "The owner can correct an automatic mistake: {result}"
    );
    let (_, listing) = request(&app, "GET", "/api/lineup", Value::Null).await;
    assert!(listing["channels"][0]["candidates"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(listing["channels"][1]["candidates"][0]["id"], "iptv:1:1");
    let favorite: String = state
        .db
        .lock()
        .unwrap()
        .query_row("SELECT id FROM favorites WHERE profile_id=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(favorite, channels[0]["id"].as_str().unwrap());
    state.playback.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn matching_preserves_manual_quality_order_after_disappearance() {
    let (state, _media, _tools, raw) = live_session_fixture(false);
    let app = router(state.clone(), None);
    state.db.lock().unwrap().execute("INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES('iptv:1:2',1,'2','Fixture East')",[]).unwrap();
    let(_,channel)=request(&app,"POST","/api/lineup",json!({"name":"Fixture East","network":"Fixture","feed":"east","market":"","category":"Kids","number":1,"enabled":true,"candidates":[{"id":"iptv:1:2","name":"Fixture East","verified":true},{"id":raw,"name":"Fixture East","verified":true}]})).await;
    request(&app, "POST", "/api/lineup/matching/run", Value::Null).await;
    let (_, listing) = request(&app, "GET", "/api/lineup", Value::Null).await;
    assert_eq!(listing["channels"][0]["candidates"][0]["id"], "iptv:1:2");
    state
        .db
        .lock()
        .unwrap()
        .execute("DELETE FROM provider_live WHERE id='iptv:1:2'", [])
        .unwrap();
    request(&app, "POST", "/api/lineup/matching/run", Value::Null).await;
    state.db.lock().unwrap().execute("INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES('iptv:1:2',1,'2','Fixture East')",[]).unwrap();
    request(&app, "POST", "/api/lineup/matching/run", Value::Null).await;
    let (_, listing) = request(&app, "GET", "/api/lineup", Value::Null).await;
    assert_eq!(listing["channels"][0]["id"], channel["id"]);
    assert_eq!(listing["channels"][0]["candidates"][0]["id"], "iptv:1:2");
    state.playback.shutdown().await;
}

#[tokio::test]
async fn matching_aliases_and_fuzzy_evidence_require_confidence_and_clear_margin() {
    let (state, _dir) = app_state();
    let app = router(state.clone(), None);
    state.db.lock().unwrap().execute_batch("INSERT INTO providers(id,name,url,username,password) VALUES(1,'One','http://fixture.invalid','fixture','secret'); INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES('iptv:1:1',1,'1','US EN Cartoon Networkk EAST HD');").unwrap();
    let(_,first)=request(&app,"POST","/api/lineup",json!({"name":"Cartoon East","network":"Cartoon Network","feed":"east","market":"","category":"Kids","number":1,"enabled":true,"candidates":[]})).await;
    request(&app, "POST", "/api/lineup/matching/run", Value::Null).await;
    let (_, review) = request(&app, "GET", "/api/lineup/matching", Value::Null).await;
    assert_eq!(review["matches"][0]["status"], "review");
    assert_eq!(review["matches"][0]["score"], 90);
    let policy = json!({"confidence":90,"ambiguity_margin":10,"ambiguity_policy":"review","active_candidates":4});
    assert_eq!(
        request(&app, "PATCH", "/api/lineup/matching", policy.clone())
            .await
            .0,
        StatusCode::OK
    );
    request(&app, "POST", "/api/lineup/matching/run", Value::Null).await;
    assert_eq!(
        request(&app, "GET", "/api/lineup", Value::Null).await.1["channels"][0]["candidates"][0]
            ["id"],
        "iptv:1:1"
    );
    let(_,second)=request(&app,"POST","/api/lineup",json!({"name":"Competing East","network":"Cartoon Networkz","feed":"east","market":"","category":"Kids","number":2,"enabled":true,"candidates":[]})).await;
    let path = format!("/api/lineup/{}/matching", second["id"].as_str().unwrap());
    assert_eq!(
        request(
            &app,
            "PATCH",
            &path,
            json!({"aliases":["Cartoon Networkk"]})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, review) = request(&app, "GET", "/api/lineup/matching", Value::Null).await;
    assert!(review["matches"]
        .as_array()
        .unwrap()
        .iter()
        .all(|m| m["status"] == "review"));
    assert!(review["matches"][0]["competing"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["channel_id"] == first["id"]));
    let mut lower = policy;
    lower["ambiguity_margin"] = json!(5);
    request(&app, "PATCH", "/api/lineup/matching", lower).await;
    request(&app, "POST", "/api/lineup/matching/run", Value::Null).await;
    let (_, listing) = request(&app, "GET", "/api/lineup", Value::Null).await;
    assert_eq!(listing["channels"][1]["candidates"][0]["id"], "iptv:1:1");
    state
        .db
        .lock()
        .unwrap()
        .execute("UPDATE auth_accounts SET role='member' WHERE id=1", [])
        .unwrap();
    assert_eq!(
        request(&app, "POST", "/api/lineup/matching/run", Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    state.playback.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn large_matching_snapshot_keeps_controls_responsive_and_rejects_stale_publication() {
    let (state, _media, _tools, raw) = live_session_fixture(false);
    let app = router(state.clone(), None);
    let(_,channel)=request(&app,"POST","/api/lineup",json!({"name":"Fixture East","network":"Fixture","feed":"east","market":"","category":"Kids","number":1,"enabled":true,"candidates":[{"id":raw,"name":"Fixture East","verified":true}]})).await;
    state.db.lock().unwrap().execute_batch("WITH RECURSIVE n(x) AS (SELECT 10 UNION ALL SELECT x+1 FROM n WHERE x<200009) INSERT INTO provider_live(id,provider_id,stream_id,name) SELECT 'iptv:1:'||x,1,CAST(x AS TEXT),'US EN Unrelated channel '||x||' EAST HD' FROM n;").unwrap();
    let job_app = app.clone();
    let started = std::time::Instant::now();
    let job = tokio::spawn(async move {
        request(&job_app, "POST", "/api/lineup/matching/run", Value::Null).await
    });
    let mut edits = 0;
    let mut longest = Duration::ZERO;
    while !job.is_finished() {
        let at = std::time::Instant::now();
        let (status, result) = request(
            &app,
            "PATCH",
            "/api/lineup/matching/groups/1",
            json!({"upstream_group":format!("Group {}",edits%2)}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{result}");
        longest = longest.max(at.elapsed());
        edits += 1;
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "Bounded matching fixture exceeded deadline"
        );
        if edits == 2 {
            assert_eq!(
                request(&app, "POST", "/api/lineup/matching/run", Value::Null)
                    .await
                    .0,
                StatusCode::TOO_MANY_REQUESTS,
                "Only one catalog matching snapshot may run at once"
            );
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let (status, result) = job.await.unwrap();
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "Changed inputs must invalidate staged results: {result}"
    );
    assert!(edits > 1);
    assert!(
        longest < Duration::from_secs(2),
        "Control request blocked for {longest:?}"
    );
    let (_, listing) = request(&app, "GET", "/api/lineup", Value::Null).await;
    assert_eq!(listing["channels"][0]["id"], channel["id"]);
    assert_eq!(listing["channels"][0]["candidates"][0]["id"], raw);
    eprintln!(
        "MATCHING_SCALE_OK entries=200001 duration_ms={} max_control_ms={} edits={edits}",
        started.elapsed().as_millis(),
        longest.as_millis()
    );
    state.playback.shutdown().await;
}

#[tokio::test]
async fn scheduled_catalog_job_refreshes_twenty_accounts_with_partial_failure_and_matching() {
    use axum::extract::Query;
    use std::collections::HashMap;
    let mock=Router::new().route("/player_api.php",get(|Query(q):Query<HashMap<String,String>>|async move {
        let user=q.get("username").map(String::as_str).unwrap_or("");
        if user=="account20" {return (StatusCode::TOO_MANY_REQUESTS,axum::Json(json!({"error":"slow down"})));}
        if !q.contains_key("action") {return (StatusCode::OK,axum::Json(json!({"user_info":{"auth":if user=="account19" {0}else{1},"status":"Active"}})));}
        match q["action"].as_str() {
            "get_live_categories"=>(StatusCode::OK,axum::Json(json!([{ "category_id":"1","category_name":"US EN"}]))),
            "get_live_streams"=>(StatusCode::OK,axum::Json(json!((1..=100).map(|id|json!({"stream_id":id,"name":if id==1{"US EN Cartoon Network EAST HD".to_owned()}else{format!("US EN Fixture{id} EAST HD")},"category_id":"1"})).collect::<Vec<_>>()))),
            _=>(StatusCode::BAD_REQUEST,axum::Json(json!({"unexpected_scope":true})))
        }
    }));
    let (address, upstream) = serve_upstream(mock).await;
    let (state, _dir) = app_state();
    let app = router(state.clone(), None);
    for id in 1..=20 {
        state.providers.add(json!({"name":format!("Account {id}"),"url":address,"username":format!("account{id}"),"password":"synthetic-secret","enable_live":true,"enable_movies":false,"enable_series":false})).unwrap();
    }
    state.db.lock().unwrap().execute("INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES('iptv:19:old',19,'old','Previous valid channel')",[]).unwrap();
    let(_,channel)=request(&app,"POST","/api/lineup",json!({"name":"Cartoon East","network":"Cartoon Network","feed":"east","market":"","category":"Kids","number":1,"enabled":true,"candidates":[]})).await;
    for number in 2..=100 {
        let (status,value)=request(&app,"POST","/api/lineup",json!({"name":format!("Fixture{number} East"),"network":format!("Fixture{number}"),"feed":"east","market":"","category":"Pilot","number":number,"enabled":true,"candidates":[]})).await;
        assert_eq!(status, StatusCode::OK, "{value}");
    }
    let(status,settings)=request(&app,"PATCH","/api/automation/catalog",json!({"enabled":false,"interval_minutes":360,"concurrency":4,"retries":1,"provider_ids":(1..=20).collect::<Vec<_>>()})).await;
    assert_eq!(status, StatusCode::OK, "{settings}");
    let (status, job) = request(&app, "POST", "/api/automation/catalog/run", Value::Null).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{job}");
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let (_, status) = request(&app, "GET", "/api/automation/catalog", Value::Null).await;
            if status["last_run"]["state"] == "completed" {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let (_, lineup) = request(&app, "GET", "/api/lineup", Value::Null).await;
    assert_eq!(lineup["channels"].as_array().unwrap().len(), 100);
    assert!(
        lineup["channels"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["candidates"].as_array().unwrap().len() == 4),
        "Every selected channel gets four bounded backups"
    );
    let accounts = result["last_run"]["accounts"].as_array().unwrap();
    assert_eq!(
        accounts
            .iter()
            .filter(|a| a["status"] == "completed")
            .count(),
        18
    );
    assert!(accounts
        .iter()
        .any(|a| a["provider_id"] == 19 && a["reason"] == "authentication_failed"));
    assert!(accounts
        .iter()
        .any(|a| a["provider_id"] == 20 && a["reason"] == "rate_limited"));
    assert_eq!(
        state
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM provider_live WHERE id='iptv:19:old'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    let (_, lineup) = request(&app, "GET", "/api/lineup", Value::Null).await;
    assert_eq!(lineup["channels"][0]["id"], channel["id"]);
    assert_eq!(
        lineup["channels"][0]["candidates"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert!(!result.to_string().contains("synthetic-secret"));
    assert!(!result.to_string().contains("http://"));
    upstream.abort();
    let _ = upstream.await;
    state.playback.shutdown().await;
}

#[tokio::test]
async fn cancelled_catalog_job_does_not_publish_late_metadata() {
    use axum::extract::Query;
    use std::{
        collections::HashMap,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
    };
    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let signals = (entered.clone(), release.clone());
    let mock = Router::new().route(
        "/player_api.php",
        get(move |Query(q): Query<HashMap<String, String>>| {
            let (entered, release) = signals.clone();
            async move {
                if !q.contains_key("action") {
                    return axum::Json(json!({"user_info":{"auth":1,"status":"Active"}}));
                }
                if q["action"] == "get_live_categories" {
                    return axum::Json(json!([]));
                }
                entered.store(true, Ordering::Release);
                while !release.load(Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                axum::Json(json!([{"stream_id":2,"name":"US EN Cartoon Network EAST HD"}]))
            }
        }),
    );
    let (address, upstream) = serve_upstream(mock).await;
    let (state, _dir) = app_state();
    state.providers.add(json!({"name":"One","url":address,"username":"fixture","password":"secret","enable_live":true,"enable_movies":false,"enable_series":false})).unwrap();
    state.db.lock().unwrap().execute("INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES('iptv:1:old',1,'old','Last good channel')",[]).unwrap();
    let app = router(state.clone(), None);
    request(
        &app,
        "PATCH",
        "/api/automation/catalog",
        json!({"enabled":false,"provider_ids":[1]}),
    )
    .await;
    assert_eq!(
        request(&app, "POST", "/api/automation/catalog/run", Value::Null)
            .await
            .0,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        while !entered.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        request(&app, "POST", "/api/automation/catalog/run", Value::Null)
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(&app, "POST", "/api/automation/catalog/cancel", Value::Null)
            .await
            .0,
        StatusCode::OK
    );
    release.store(true, Ordering::Release);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let (_, v) = request(&app, "GET", "/api/automation/catalog", Value::Null).await;
            if v["last_run"]["state"] == "cancelled" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let rows: Vec<String> = state
        .db
        .lock()
        .unwrap()
        .prepare("SELECT id FROM provider_live")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(rows, vec!["iptv:1:old"]);
    upstream.abort();
    let _ = upstream.await;
    state.playback.shutdown().await;
}

#[tokio::test]
async fn empty_automated_catalog_retains_last_good_scope() {
    use axum::extract::Query;
    use std::collections::HashMap;
    let mock = Router::new().route(
        "/player_api.php",
        get(|Query(q): Query<HashMap<String, String>>| async move {
            axum::Json(if q.contains_key("action") {
                json!([])
            } else {
                json!({"user_info":{"auth":1,"status":"Active"}})
            })
        }),
    );
    let (address, upstream) = serve_upstream(mock).await;
    let (state, _dir) = app_state();
    state.providers.add(json!({"name":"One","url":address,"username":"fixture","password":"secret","enable_live":true,"enable_movies":false,"enable_series":false})).unwrap();
    state.db.lock().unwrap().execute("INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES('iptv:1:old',1,'old','Last good channel')",[]).unwrap();
    let app = router(state.clone(), None);
    request(
        &app,
        "PATCH",
        "/api/automation/catalog",
        json!({"enabled":false,"provider_ids":[1]}),
    )
    .await;
    request(&app, "POST", "/api/automation/catalog/run", Value::Null).await;
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let (_, v) = request(&app, "GET", "/api/automation/catalog", Value::Null).await;
            if v["last_run"]["state"] == "completed" {
                break v;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(result["last_run"]["accounts"][0]["reason"], "empty_catalog");
    assert_eq!(result["last_run"]["accounts"][0]["attempts"], 1);
    assert_eq!(
        state
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM provider_live WHERE id='iptv:1:old'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    upstream.abort();
    let _ = upstream.await;
    state.playback.shutdown().await;
}

#[tokio::test]
async fn broken_category_catalog_retains_last_good_identity_evidence() {
    use axum::extract::Query;
    use std::collections::HashMap;
    let mock = Router::new().route(
        "/player_api.php",
        get(|Query(q): Query<HashMap<String, String>>| async move {
            axum::Json(if q.contains_key("action") {
                if q["action"] == "get_live_categories" {
                    json!([{"category_id":"1"}])
                } else {
                    json!([{"stream_id":2,"name":"Cartoon Network EAST","category_id":"1"}])
                }
            } else {
                json!({"user_info":{"auth":1,"status":"Active"}})
            })
        }),
    );
    let (address, upstream) = serve_upstream(mock).await;
    let (state, _dir) = app_state();
    state.providers.add(json!({"name":"One","url":address,"username":"fixture","password":"secret","enable_live":true,"enable_movies":false,"enable_series":false})).unwrap();
    state.db.lock().unwrap().execute("INSERT INTO provider_live(id,provider_id,stream_id,name,category_id,category) VALUES('iptv:1:old',1,'old','Last good channel','1','US EN')",[]).unwrap();
    let app = router(state.clone(), None);
    request(
        &app,
        "PATCH",
        "/api/automation/catalog",
        json!({"enabled":false,"provider_ids":[1]}),
    )
    .await;
    request(&app, "POST", "/api/automation/catalog/run", Value::Null).await;
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let (_, v) = request(&app, "GET", "/api/automation/catalog", Value::Null).await;
            if v["last_run"]["state"] == "completed" {
                break v;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        result["last_run"]["accounts"][0]["reason"],
        "invalid_metadata"
    );
    assert_eq!(result["last_run"]["accounts"][0]["attempts"], 1);
    assert_eq!(
        state
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM provider_live WHERE id='iptv:1:old'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    upstream.abort();
    let _ = upstream.await;
    state.playback.shutdown().await;
}

#[tokio::test]
async fn catalog_restart_resumes_pending_accounts_and_honors_persisted_due_time() {
    use axum::extract::Query;
    use std::{
        collections::HashMap,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };
    let first_calls = Arc::new(AtomicUsize::new(0));
    let calls = first_calls.clone();
    let mock = Router::new().route(
        "/player_api.php",
        get(move |Query(q): Query<HashMap<String, String>>| {
            let calls = calls.clone();
            async move {
                if q["username"] == "one" {
                    calls.fetch_add(1, Ordering::SeqCst);
                }
                axum::Json(match q.get("action").map(String::as_str) {
                    None => json!({"user_info":{"auth":1,"status":"Active"}}),
                    Some("get_live_categories") => json!([]),
                    _ => json!([{"stream_id":1,"name":"US EN Cartoon Network EAST HD"}]),
                })
            }
        }),
    );
    let (address, upstream) = serve_upstream(mock).await;
    let (state, _media) = app_state();
    for name in ["one", "two"] {
        state.providers.add(json!({"name":name,"url":address,"username":name,"password":"secret","enable_live":true,"enable_movies":false,"enable_series":false})).unwrap();
    }
    let app = router(state.clone(), None);
    request(
        &app,
        "PATCH",
        "/api/automation/catalog",
        json!({"enabled":false,"provider_ids":[1,2]}),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("restart.sqlite");
    state
        .db
        .lock()
        .unwrap()
        .execute("VACUUM INTO ?1", [path.to_str().unwrap()])
        .unwrap();
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute("INSERT INTO catalog_runs(id,state,owner_id,policy,generation,created_at) SELECT 'resume-run','running',1,data,4,100 FROM catalog_schedule WHERE id=1",[]).unwrap();
        db.execute_batch("INSERT INTO catalog_results(run_id,provider_id,status,attempts) VALUES('resume-run',1,'completed',1),('resume-run',2,'running',1);
            INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES('iptv:1:1',1,'1','US EN Cartoon Network EAST HD');").unwrap();
    }
    let (runtime, _resumed_media) = app_state();
    let resumed = App::new(
        rusqlite::Connection::open(&path).unwrap(),
        runtime.providers.client.clone(),
        runtime.playback.clone(),
    )
    .unwrap();
    let resumed_app = router(resumed.clone(), None);
    let result = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let (_, v) = request(&resumed_app, "GET", "/api/automation/catalog", Value::Null).await;
            if v["last_run"]["state"] == "completed" {
                break v;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(result["last_run"]["id"], "resume-run");
    assert_eq!(
        first_calls.load(Ordering::SeqCst),
        0,
        "Restart must not refetch a checkpointed success"
    );
    assert_eq!(result["last_run"]["accounts"][0]["attempts"], 1);
    assert_eq!(result["last_run"]["accounts"][1]["attempts"], 2);
    assert_eq!(
        resumed
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT generation FROM catalog_runs WHERE id='resume-run'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        5
    );
    resumed.db.lock().unwrap().execute("UPDATE catalog_schedule SET data=json_set(data,'$.enabled',json('true')),next_run=?1 WHERE id=1",[viptv_server::util::now()+3600]).unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(
        first_calls.load(Ordering::SeqCst),
        0,
        "Future schedule must not fire early"
    );
    resumed
        .db
        .lock()
        .unwrap()
        .execute(
            "UPDATE catalog_schedule SET next_run=?1 WHERE id=1",
            [viptv_server::util::now() - 1],
        )
        .unwrap();
    let next = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let (_, v) = request(&resumed_app, "GET", "/api/automation/catalog", Value::Null).await;
            if v["last_run"]["id"] != "resume-run" && v["last_run"]["state"] == "completed" {
                break v;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(first_calls.load(Ordering::SeqCst) > 0);
    assert!(next["next_run"].as_i64().unwrap() > viptv_server::util::now());
    upstream.abort();
    let _ = upstream.await;
    state.playback.shutdown().await;
    resumed.playback.shutdown().await;
}

#[tokio::test]
async fn timed_out_catalog_transaction_rolls_back_before_publication() {
    use axum::extract::Query;
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };
    let mock = Router::new().route(
        "/player_api.php",
        get(|Query(q): Query<HashMap<String, String>>| async move {
            axum::Json(match q.get("action").map(String::as_str) {
                None => json!({"user_info":{"auth":1,"status":"Active"}}),
                Some("get_live_categories") => json!([]),
                _ => json!([{"stream_id":2,"name":"US EN Cartoon Network EAST HD"}]),
            })
        }),
    );
    let (address, upstream) = serve_upstream(mock).await;
    let (state, _dir) = app_state();
    state.providers.add(json!({"name":"One","url":address,"username":"fixture","password":"secret","enable_live":true,"enable_movies":false,"enable_series":false})).unwrap();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let entered = Arc::new(Mutex::new(Some(entered_tx)));
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    {
        let db = state.db.lock().unwrap();
        db.execute("INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES('iptv:1:old',1,'old','Last good channel')",[]).unwrap();
        db.create_scalar_function(
            "fixture_hold_publication",
            0,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8,
            move |_| {
                if let Some(signal) = entered.lock().unwrap().take() {
                    let _ = signal.send(());
                }
                release_rx
                    .recv_timeout(Duration::from_secs(12))
                    .expect("test must release blocked publication");
                Ok(1)
            },
        )
        .unwrap();
        db.execute_batch("CREATE TRIGGER fixture_publication BEFORE INSERT ON provider_live BEGIN SELECT fixture_hold_publication(); END;").unwrap();
    }
    let app = router(state.clone(), None);
    request(
        &app,
        "PATCH",
        "/api/automation/catalog",
        json!({"enabled":false,"provider_ids":[1],"retries":0,"request_timeout_seconds":5}),
    )
    .await;
    assert_eq!(
        request(&app, "POST", "/api/automation/catalog/run", Value::Null)
            .await
            .0,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(Duration::from_secs(3), entered_rx)
        .await
        .unwrap()
        .unwrap();
    // The worker now holds an open publication transaction. Its caller times out
    // independently; releasing it must roll back, not install the late snapshot.
    tokio::time::sleep(Duration::from_millis(5500)).await;
    release_tx.send(()).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let (_, v) = request(&app, "GET", "/api/automation/catalog", Value::Null).await;
            if v["last_run"]["state"] == "completed" {
                break v;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        result["last_run"]["accounts"][0]["reason"],
        "request_failed"
    );
    let ids = state
        .db
        .lock()
        .unwrap()
        .prepare("SELECT id FROM provider_live")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(ids, vec!["iptv:1:old"]);
    // The next manual run honors the persisted account backoff, without opening
    // a second request/publication transaction (the fixture receiver is closed).
    request(&app, "POST", "/api/automation/catalog/run", Value::Null).await;
    let skipped = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let (_, v) = request(&app, "GET", "/api/automation/catalog", Value::Null).await;
            if v["last_run"]["id"] != result["last_run"]["id"]
                && v["last_run"]["state"] == "completed"
            {
                break v;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(skipped["last_run"]["accounts"][0]["status"], "backoff");
    assert_eq!(skipped["last_run"]["accounts"][0]["attempts"], 0);
    upstream.abort();
    let _ = upstream.await;
    state.playback.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn shared_family_viewers_use_one_input_and_keep_independent_capabilities() {
    let (state, _media, _tools, raw) = live_session_fixture(false);
    let app = router(state.clone(), None);
    let (_,channel)=request(&app,"POST","/api/lineup",json!({"name":"Fixture East","network":"Fixture","feed":"east","market":"","category":"Kids","number":1,"enabled":true,"candidates":[{"id":raw,"name":"Fixture East","verified":true}]})).await;
    let body = json!({"channel_id":channel["id"]});
    let ((sa, a), (sb, b)) = tokio::join!(
        request(&app, "POST", "/api/playback", body.clone()),
        request(&app, "POST", "/api/playback", body.clone())
    );
    assert_eq!(sa, StatusCode::OK, "{a}");
    assert_eq!(sb, StatusCode::OK, "{b}");
    assert_ne!(a["id"], b["id"]);
    assert_ne!(a["url"], b["url"]);
    assert!(a.get("_source_key").is_none());
    let (status, activity) = request(&app, "GET", "/api/activity", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{activity}");
    assert_eq!(activity["sharing"]["workers"], 1);
    assert_eq!(activity["sharing"]["viewers"], 2);
    assert_eq!(activity["pools"][0]["local_reservations"], 1);
    assert_eq!(
        request(&app, "POST", "/api/activity/pause", json!({"paused":true}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "DELETE",
            &format!("/api/playback/{}", a["id"].as_str().unwrap()),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, "GET", a["url"].as_str().unwrap(), Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(
            &app,
            "POST",
            &format!("/api/playback/{}/heartbeat", b["id"].as_str().unwrap()),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    // An unrelated setting change must not split an existing audience.
    state
        .db
        .lock()
        .unwrap()
        .execute("UPDATE family_matching_revision SET value=value+1", [])
        .unwrap();
    let (status, c) = request(&app, "POST", "/api/playback", body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{c}");
    assert_eq!(
        request(&app, "GET", "/api/activity", Value::Null).await.1["sharing"]["workers"],
        1
    );
    state
        .db
        .lock()
        .unwrap()
        .execute("UPDATE providers SET enabled=0 WHERE id=1", [])
        .unwrap();
    assert_ne!(
        request(&app, "POST", "/api/playback", body).await.0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "POST",
            &format!("/api/playback/{}/heartbeat", b["id"].as_str().unwrap()),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    for viewer in [&b, &c] {
        assert_eq!(
            request(
                &app,
                "DELETE",
                &format!("/api/playback/{}", viewer["id"].as_str().unwrap()),
                Value::Null
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    assert_eq!(
        request(&app, "GET", "/api/status", Value::Null).await.1["active_sessions"],
        0
    );
    state.playback.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn health_exclusion_and_activity_undo_preserve_manual_intent() {
    let (state, _media, _tools, raw) = live_session_fixture(false);
    let app = router(state.clone(), None);
    let path = format!("/api/stream-health/{raw}");
    let (status, health) = request(&app, "GET", "/api/stream-health", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{health}");
    assert_eq!(health["settings"]["retry_minutes"], json!([5, 15, 60, 360]));
    assert_eq!(
        request(
            &app,
            "PATCH",
            "/api/stream-health",
            json!({"budget_seconds":15,"sample_seconds":15})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            &app,
            "PATCH",
            &path,
            json!({"disabled":true,"exclude_minutes":0})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, activity) = request(&app, "GET", "/api/activity", Value::Null).await;
    let change = activity["changes"][0]["id"].as_i64().unwrap();
    assert_eq!(
        request(
            &app,
            "POST",
            &format!("/api/activity/undo/{change}"),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(!state
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT disabled FROM candidate_health WHERE live_id=?1",
            [&raw],
            |r| r.get::<_, bool>(0)
        )
        .unwrap());
    request(
        &app,
        "PATCH",
        &path,
        json!({"disabled":true,"exclude_minutes":0}),
    )
    .await;
    let (_, activity) = request(&app, "GET", "/api/activity", Value::Null).await;
    let change = activity["changes"][0]["id"].as_i64().unwrap();
    request(
        &app,
        "PATCH",
        &path,
        json!({"disabled":true,"exclude_minutes":5}),
    )
    .await;
    assert_eq!(
        request(
            &app,
            "POST",
            &format!("/api/activity/undo/{change}"),
            json!({})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    state
        .db
        .lock()
        .unwrap()
        .execute("UPDATE auth_accounts SET role='member' WHERE id=1", [])
        .unwrap();
    for path in ["/api/stream-health", "/api/guides", "/api/activity"] {
        let (status, denied) = request(&app, "GET", path, Value::Null).await;
        assert!(
            matches!(status, StatusCode::BAD_REQUEST | StatusCode::FORBIDDEN),
            "{path}: {denied}"
        );
        assert!(denied.get("error").is_some());
    }
    state.playback.shutdown().await;
}

#[tokio::test]
async fn guide_refresh_persists_last_valid_and_owner_repair_uses_exact_feed() {
    let (state, _dir) = app_state();
    let now = chrono::DateTime::<chrono::Utc>::from_timestamp(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64,
        0,
    )
    .unwrap();
    let begin = now - chrono::Duration::minutes(5);
    let end = now + chrono::Duration::hours(2);
    let xml=format!("<tv><channel id=\"cn\"><display-name>Cartoon Network East</display-name></channel><programme channel=\"cn\" start=\"{}\" stop=\"{}\"><title>Family programme</title><desc>A retained description</desc></programme></tv>",begin.format("%Y%m%d%H%M%S %z"),end.format("%Y%m%d%H%M%S %z"));
    let content = std::sync::Arc::new(std::sync::Mutex::new(xml));
    let serving = content.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/guide",
                get(move || {
                    let body = serving.lock().unwrap().clone();
                    async move { body }
                }),
            ),
        )
        .await
        .unwrap()
    });
    let app = router(state.clone(), None);
    let (_,family)=request(&app,"POST","/api/lineup",json!({"name":"Cartoon Network East","network":"Cartoon Network","feed":"east","market":"","category":"Kids","number":1,"enabled":true,"candidates":[]})).await;
    let id = family["id"].as_str().unwrap();
    let (status, source) = request(
        &app,
        "POST",
        "/api/guides/sources",
        json!({"name":"Local fixture","url":format!("http://{address}/guide")}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{source}");
    async fn refresh(app: &Router) {
        assert_eq!(
            request(app, "POST", "/api/guides/run", json!({})).await.0,
            StatusCode::ACCEPTED
        );
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let (_, v) = request(app, "GET", "/api/guides", Value::Null).await;
                if ["completed", "failed"].contains(&v["last_run"]["state"].as_str().unwrap_or(""))
                {
                    assert_eq!(v["last_run"]["state"], "completed", "{v}");
                    assert_ne!(v["last_run"]["reason"], "guide_mapping_failed", "{v}");
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
    }
    refresh(&app).await;
    let (status,mapped)=request(&app,"PATCH",&format!("/api/guides/channels/{id}"),json!({"source_id":1,"guide_id":"cn","observed_name":"Cartoon Network East","verified":true,"priority":0})).await;
    assert_eq!(status, StatusCode::OK, "{mapped}");
    let (status, guide) = request(&app, "GET", &format!("/api/guide/{id}"), Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{guide}");
    assert_eq!(guide["programs"][0]["title"], "Family programme");
    assert!(guide["programs"][0]["display_time"]
        .as_str()
        .unwrap()
        .contains("EDT"));
    assert_eq!(guide["timeline"].as_array().unwrap().len(), 54);
    let ticks = guide["timeline"].as_array().unwrap();
    assert_eq!(
        ticks[1]["start"].as_i64().unwrap() - ticks[0]["start"].as_i64().unwrap(),
        1800
    );
    assert!(ticks[0]["display_time"].as_str().unwrap().contains("EDT"));
    *content.lock().unwrap() = "<tv>broken".into();
    refresh(&app).await;
    assert_eq!(
        request(&app, "GET", &format!("/api/guide/{id}"), Value::Null)
            .await
            .1["programs"],
        guide["programs"]
    );
    let (_,west)=request(&app,"POST","/api/lineup",json!({"name":"Cartoon Network West","network":"Cartoon Network","feed":"west","market":"","category":"Kids","number":2,"enabled":true,"candidates":[]})).await;
    assert_eq!(request(&app,"PATCH",&format!("/api/guides/channels/{}",west["id"].as_str().unwrap()),json!({"source_id":1,"guide_id":"cn","observed_name":"Cartoon Network East","verified":true,"priority":0})).await.0,StatusCode::BAD_REQUEST);
    state.playback.shutdown().await;
    upstream.abort();
}

#[cfg(unix)]
#[tokio::test]
async fn health_jobs_publish_media_observations_and_scheduler_recovers_a_failure() {
    let (state, _media, tools, raw) = live_session_fixture(false);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let upstream = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/live/fixture/fixture/1.ts",
                get(|| async { "bounded fixture bytes" }),
            ),
        )
        .await
        .unwrap()
    });
    state
        .db
        .lock()
        .unwrap()
        .execute("UPDATE providers SET url=?1", [format!("http://{addr}")])
        .unwrap();
    let app = router(state.clone(), None);
    let (_,family)=request(&app,"POST","/api/lineup",json!({"name":"Fixture East","network":"Fixture","feed":"east","market":"","category":"Kids","number":1,"enabled":true,"candidates":[{"id":raw,"name":"Fixture East","verified":true}]})).await;
    std::fs::write(
        tools.path().join("ffprobe"),
        "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{\"frames\":[],\"streams\":[]}'\n",
    )
    .unwrap();
    assert_eq!(
        request(
            &app,
            "POST",
            &format!("/api/stream-health/{raw}/check"),
            json!({})
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    async fn wait_health(app: &Router, state: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let (status, v) = request(app, "GET", "/api/stream-health", Value::Null).await;
                assert_eq!(status, StatusCode::OK, "{v}");
                if v["checking"].as_array().unwrap().is_empty()
                    && v["candidates"][0]["state"] == state
                {
                    break v;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap()
    }
    let failed = wait_health(&app, "cooling_down").await;
    assert!(
        failed["candidates"][0]["next_check"].as_i64().unwrap()
            > failed["candidates"][0]["at"].as_i64().unwrap()
    );
    let sample = json!({"frames":(0..90).map(|n|json!({"media_type":"video","best_effort_timestamp_time":format!("{:.2}",n as f64/10.0)})).collect::<Vec<_>>(),"streams":[{"codec_type":"video","codec_name":"h264","width":1280,"height":720},{"codec_type":"audio","codec_name":"aac","channels":2}]});
    std::fs::write(
        tools.path().join("ffprobe"),
        format!("#!/bin/sh\ncat >/dev/null\nprintf '%s' '{}'\n", sample),
    )
    .unwrap();
    let (status, v) = request(&app, "PATCH", "/api/stream-health", json!({"enabled":true})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    state
        .db
        .lock()
        .unwrap()
        .execute("UPDATE candidate_health SET next_check=0", [])
        .unwrap();
    let healthy = wait_health(&app, "healthy").await;
    assert_eq!(healthy["candidates"][0]["sample"]["video_codec"], "h264");
    // Manual disable survives later observations and excludes future viewing.
    request(
        &app,
        "PATCH",
        &format!("/api/stream-health/{raw}"),
        json!({"disabled":true,"exclude_minutes":0}),
    )
    .await;
    assert_ne!(
        request(
            &app,
            "POST",
            "/api/playback",
            json!({"channel_id":family["id"]})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, "GET", "/api/stream-health", Value::Null)
            .await
            .1["candidates"][0]["state"],
        "disabled"
    );
    state.playback.shutdown().await;
    upstream.abort();
}

#[tokio::test]
#[ignore = "requires VIPTV_TEST_FFMPEG and VIPTV_TEST_FFPROBE; run in isolated media image"]
async fn real_health_decodes_media_and_rejects_http_success_with_invalid_content() {
    let ffmpeg = std::env::var("VIPTV_TEST_FFMPEG").unwrap();
    let ffprobe = std::env::var("VIPTV_TEST_FFPROBE").unwrap();
    let (state, dir) = app_state_with_tools(ffmpeg.clone().into(), ffprobe.into());
    let media = dir.path().join("sample.ts");
    let output = tokio::process::Command::new(ffmpeg)
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x180:rate=25",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "10",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-c:a",
            "aac",
            "-f",
            "mpegts",
        ])
        .arg(&media)
        .output()
        .await
        .unwrap();
    assert!(output.status.success(), "Fixture media generation failed");
    let bytes = std::sync::Arc::new(std::sync::Mutex::new(std::fs::read(media).unwrap()));
    let served = bytes.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route(
                    "/live/fixture/fixture/1.ts",
                    get(|| async { axum::response::Redirect::temporary("/sample.ts") }),
                )
                .route(
                    "/sample.ts",
                    get(move || {
                        let data = served.lock().unwrap().clone();
                        async move { data }
                    }),
                ),
        )
        .await
        .unwrap()
    });
    {
        let db = state.db.lock().unwrap();
        db.execute("INSERT INTO providers(id,name,url,username,password,max_connections) VALUES(1,'Real decoder fixture',?1,'fixture','fixture',1)",[format!("http://{address}")]).unwrap();
        db.execute("INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES('iptv:1:1',1,'1','Fixture East')",[]).unwrap();
    }
    let app = router(state.clone(), None);
    request(&app,"POST","/api/lineup",json!({"name":"Fixture East","network":"Fixture","feed":"east","market":"","category":"Kids","number":1,"enabled":true,"candidates":[{"id":"iptv:1:1","name":"Fixture East","verified":true}]})).await;
    for expected in ["healthy", "cooling_down"] {
        assert_eq!(
            request(&app, "POST", "/api/stream-health/iptv:1:1/check", json!({}))
                .await
                .0,
            StatusCode::ACCEPTED
        );
        let result = tokio::time::timeout(Duration::from_secs(40), async {
            loop {
                let (_, v) = request(&app, "GET", "/api/stream-health", Value::Null).await;
                if v["checking"].as_array().unwrap().is_empty() {
                    break v;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(result["candidates"][0]["state"], expected, "{result}");
        if expected == "healthy" {
            assert_eq!(result["candidates"][0]["sample"]["video_codec"], "h264");
            assert!(
                result["candidates"][0]["sample"]["sample_seconds"]
                    .as_f64()
                    .unwrap()
                    > 5.0
            );
        }
        assert_eq!(
            request(&app, "GET", "/api/account-pools", Value::Null)
                .await
                .1["pools"][0]["local_reservations"],
            0
        );
        *bytes.lock().unwrap() = b"HTTP succeeded but this is not video".to_vec();
    }
    state.playback.shutdown().await;
    upstream.abort();
}

#[tokio::test]
async fn oversized_provider_guide_uses_selected_feeds_and_retains_last_good() {
    use base64::Engine;
    use std::sync::{Arc, Mutex};
    let (state, _dir) = app_state();
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let fail = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let xml_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = calls.clone();
    let failure = fail.clone();
    let xml_requests = xml_calls.clone();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let upstream = tokio::spawn(async move {
        axum::serve(listener, Router::new()
            .route("/xmltv.php", get(move || {xml_requests.fetch_add(1,std::sync::atomic::Ordering::SeqCst);async {"x".repeat(33*1024*1024)}}))
            .route("/player_api.php", get(move |axum::extract::Query(query):axum::extract::Query<std::collections::HashMap<String,String>>| {
                let stream=query["stream_id"].clone();requests.lock().unwrap().push(stream.clone());
                assert_eq!(query["action"],"get_short_epg"); assert_eq!(query["limit"],"1000");
                let failing=failure.load(std::sync::atomic::Ordering::SeqCst);
                async move {if failing {(StatusCode::SERVICE_UNAVAILABLE,axum::Json(json!({})))} else {(StatusCode::OK,axum::Json(json!({"epg_listings":[{"title":base64::engine::general_purpose::STANDARD.encode(format!("Feed {stream} & family")),"description":"RGVzY3JpcHRpb24=","start_timestamp":(now-60).to_string(),"stop_timestamp":(now+7200).to_string()}]})))}}
            }))).await.unwrap()
    });
    {
        let db = state.db.lock().unwrap();
        db.execute("INSERT INTO providers(id,name,url,username,password) VALUES(1,'Fixture',?1,'fixture','fixture')",[format!("http://{address}")]).unwrap();
        db.execute_batch("INSERT INTO provider_live(id,provider_id,stream_id,name,epg_channel_id) VALUES('iptv:1:1',1,'1','Cartoon Network East','shared-bad-id'),('iptv:1:2',1,'2','Cartoon Network West','shared-bad-id'),('iptv:1:3',1,'3','Unselected','other');").unwrap();
    }
    let app = router(state.clone(), None);
    let mut channels = Vec::new();
    for (stream, feed) in [(1, "east"), (2, "west")] {
        let name = if stream == 1 {
            "Cartoon Network East"
        } else {
            "Cartoon Network West"
        };
        let (status,family)=request(&app,"POST","/api/lineup",json!({"name":name,"network":"Cartoon Network","feed":feed,"market":"","category":"Kids","number":stream,"enabled":true,"candidates":[{"id":format!("iptv:1:{stream}"),"name":name,"verified":true}]})).await;
        assert_eq!(status, StatusCode::OK, "{family}");
        channels.push(family["id"].as_str().unwrap().to_owned());
    }
    assert_eq!(
        request(
            &app,
            "POST",
            "/api/guides/sources",
            json!({"name":"Provider","provider_id":1})
        )
        .await
        .0,
        StatusCode::OK
    );
    for cycle in 0..3 {
        if cycle == 2 {
            fail.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        assert_eq!(
            request(&app, "POST", "/api/guides/run", json!({})).await.0,
            StatusCode::ACCEPTED
        );
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let (_, summary) = request(&app, "GET", "/api/guides", Value::Null).await;
                if summary["last_run"]["state"] == "completed" {
                    assert_eq!(
                        summary["sources"][0]["mode"], "selected_channels",
                        "{summary}"
                    );
                    assert_ne!(
                        summary["last_run"]["reason"], "guide_mapping_failed",
                        "{summary}"
                    );
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap();
        for (index, id) in channels.iter().enumerate() {
            let (status, guide) =
                request(&app, "GET", &format!("/api/guide/{id}"), Value::Null).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(
                guide["programs"][0]["title"],
                format!("Feed {} & family", index + 1),
                "{guide}"
            );
            assert_eq!(guide["programs"][0]["description"], "Description");
        }
    }
    assert_eq!(xml_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let mut actual = calls.lock().unwrap().clone();
    actual.sort();
    assert_eq!(actual, vec!["1", "1", "1", "2", "2", "2"]);
    upstream.abort();
    state.playback.shutdown().await;
}

#[tokio::test]
async fn us_live_view_filters_sections_and_searches_only_current_programmes() {
    let (state, _dir) = app_state();
    let now = viptv_server::util::now();
    {
        let db = state.db.lock().unwrap();
        db.execute("INSERT INTO providers(id,name,url,username,password) VALUES(1,'Fixture','http://fixture.invalid','fixture','fixture')",[]).unwrap();
        for (id, name, category) in [
            (1, "US CNN HD", "US News"),
            (2, "Cartoon Network East", "US Kids"),
            (3, "Cartoon Network West", "US Kids"),
            (4, "UK Cartoon Network", "UK Kids"),
            (5, "HBO", "Adult XXX"),
            (6, "Unknown", "US"),
            (7, "HGTV", "US Entertainment"),
        ] {
            db.execute("INSERT INTO provider_live(id,provider_id,stream_id,name,category) VALUES(?1,1,?2,?3,?4)",rusqlite::params![format!("iptv:1:{id}"),id.to_string(),name,category]).unwrap();
        }
        use base64::Engine;
        let title = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);
        let payload = json!({"epg_listings":[{"title":title("Gumball"),"start_timestamp":now-60,"stop_timestamp":now+60},{"title":title("Future Adventure"),"start_timestamp":now+60,"stop_timestamp":now+3600}]});
        db.execute(
            "INSERT INTO provider_cache VALUES(1,'get_short_epg:2',?1,?2)",
            rusqlite::params![now + 3600, payload.to_string()],
        )
        .unwrap();
    }
    let app = router(state.clone(), None);
    let (status, all) = request(&app, "GET", "/api/live?view=us", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{all}");
    assert_eq!(all["total"], 4);
    assert_eq!(all["channels"][0]["id"], "iptv:1:1");
    let (_, categories) = request(&app, "GET", "/api/live/categories?view=us", Value::Null).await;
    assert_eq!(
        categories["categories"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["News", "Kids", "Home & Food"]
    );
    for (query, count) in [
        ("gumball", 1),
        ("future", 0),
        ("kids", 2),
        ("cn", 2),
        ("west", 1),
        ("hgtv", 1),
        ("xxx", 0),
    ] {
        let (_, result) = request(
            &app,
            "GET",
            &format!("/api/live?view=us&search={query}"),
            Value::Null,
        )
        .await;
        assert_eq!(result["total"], count, "query {query}: {result}");
    }
    let (_, result) = request(
        &app,
        "GET",
        "/api/live?view=us&category=section%3AKids&offset=1&limit=1",
        Value::Null,
    )
    .await;
    assert_eq!(result["total"], 2);
    assert_eq!(result["channels"][0]["id"], "iptv:1:3");
    state
        .db
        .lock()
        .unwrap()
        .execute("UPDATE provider_cache SET expires_at=0", [])
        .unwrap();
    let (_, result) = request(&app, "GET", "/api/live?view=us&search=gumball", Value::Null).await;
    assert_eq!(
        result["total"], 0,
        "stale guide must not claim a current airing"
    );
    {
        let db = state.db.lock().unwrap();
        for id in ["iptv:1:2", "iptv:1:4", "iptv:1:5"] {
            db.execute(
                "INSERT INTO favorites(profile_id,id,type,name) VALUES(1,?1,'live','Saved')",
                [id],
            )
            .unwrap();
        }
    }
    let (_, saved) = request(
        &app,
        "GET",
        "/api/live?view=us&collection=favorites",
        Value::Null,
    )
    .await;
    assert_eq!(
        saved["total"], 1,
        "saved references cannot reintroduce foreign or adult channels"
    );
    assert_eq!(saved["channels"][0]["id"], "iptv:1:2");
    state.playback.shutdown().await;
}
