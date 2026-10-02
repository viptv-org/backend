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
    inputs: Arc<Mutex<Vec<Value>>>,
    outputs: Arc<Mutex<Vec<Value>>>,
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
    crate::test_support::encrypt_fixture_sources(app);
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
async fn live_source_fixture(app: &App) -> String {
    {
        let db = app.db.lock().unwrap();
        db.execute("INSERT INTO providers(id,name,url,username,password,enabled) VALUES(1,'Fixture','http://fixture.invalid','user','password',1)", []).unwrap();
        db.execute("INSERT INTO provider_ownership VALUES(1,1)", [])
            .unwrap();
        db.execute("INSERT INTO provider_live(id,provider_id,stream_id,name,logo) VALUES('iptv:1:7',1,'7','Selected channel','https://logo.fixture.invalid/7.png')", []).unwrap();
    }
    crate::test_support::encrypt_fixture_sources(app);
    let (status, response) = request(
        app,
        "member-token-1",
        "POST",
        "/api/v2/iptv/live/iptv:1:7/source",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    response["source"]["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn ready_live_admission_records_exact_profile_history_once_for_both_deliveries() {
    for platform in ["android", "roku"] {
        let (app, _peer) = setup().await;
        gateway(&app, "first", 1);
        let source = live_source_fixture(&app).await;
        let input = body(&source, "live-history", platform);
        let (status, start) = request(
            &app,
            "member-token-1",
            "POST",
            "/api/v2/playback",
            input.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{start}");
        let id = start["id"].as_str().unwrap();
        assert_eq!(settled(&app, id).await["status"], "ready");
        let saved = || {
            app.db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT profile_id,id,name,poster,position,duration,updated_at FROM progress",
                    [],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, f64>(4)?,
                            row.get::<_, f64>(5)?,
                            row.get::<_, i64>(6)?,
                        ))
                    },
                )
                .unwrap()
        };
        let first = saved();
        assert_eq!(
            (
                first.0,
                first.1.as_str(),
                first.2.as_str(),
                first.4,
                first.5
            ),
            (1, "iptv:1:7", "Selected channel", 0.0, 0.0)
        );
        assert!(first.3.ends_with("/7.png"));
        request(&app, "member-token-1", "POST", "/api/v2/playback", input).await;
        request(
            &app,
            "member-token-1",
            "POST",
            &format!("/api/v2/playback/{id}/heartbeat"),
            Value::Null,
        )
        .await;
        assert_eq!(saved(), first);
        let (status, recent) = request(
            &app,
            "member-token-1",
            "GET",
            "/api/v2/iptv/live/channels?collection=recent",
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{recent}");
        assert_eq!(recent["items"][0]["id"], "iptv:1:7");
        request(
            &app,
            "member-token-1",
            "DELETE",
            &format!("/api/v2/playback/{id}"),
            Value::Null,
        )
        .await;
        app.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE progress SET position=12,duration=45,context='{\"fixture\":true}'",
                [],
            )
            .unwrap();
        let (_, start) = request(
            &app,
            "member-token-1",
            "POST",
            "/api/v2/playback",
            body(&source, "second-live", platform),
        )
        .await;
        assert_eq!(
            settled(&app, start["id"].as_str().unwrap()).await["status"],
            "ready"
        );
        let second = saved();
        assert_eq!((second.4, second.5), (12.0, 45.0));
        assert!(second.6 > first.6);
        let db = app.db.lock().unwrap();
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM progress", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("SELECT context FROM progress", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "{\"fixture\":true}"
        );
    }
}

#[tokio::test]
async fn cancelled_or_removed_live_admission_does_not_record_history() {
    let (app, peer) = setup().await;
    gateway(&app, "first", 1);
    let source = live_source_fixture(&app).await;
    peer.mode.store(3, Ordering::SeqCst);
    let (_, start) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/playback",
        body(&source, "cancel-live", "roku"),
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
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM progress", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    app.db
        .lock()
        .unwrap()
        .execute("DELETE FROM provider_live", [])
        .unwrap();
    let (status, error) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/playback",
        body(&source, "removed-live", "android"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{error}");
    assert_eq!(error["error_code"], "source_not_found");
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM progress", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn source_revoked_during_live_gateway_start_never_records_recent() {
    let (app, peer) = setup().await;
    gateway(&app, "first", 1);
    let source = live_source_fixture(&app).await;
    peer.mode.store(3, Ordering::SeqCst);
    let (_, start) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/playback",
        body(&source, "revoke-live", "roku"),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(3), async {
        while peer.starts.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    app.db
        .lock()
        .unwrap()
        .execute("DELETE FROM provider_ownership WHERE provider_id=1", [])
        .unwrap();
    peer.hold.notify_one();
    tokio::time::timeout(Duration::from_secs(3), async {
        while peer.stops.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let failed = settled(&app, start["id"].as_str().unwrap()).await;
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["error_code"], "source_not_found");
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM progress", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
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
    app.providers.vault = app.secret_vault.clone();
    app.addons.vault = app.secret_vault.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    app.gateway_client = client::Client::fixture(
        format!("http://{}/", listener.local_addr().unwrap())
            .parse()
            .unwrap(),
    );
    let starts = Arc::new(Mutex::new(Vec::new()));
    let outputs = Arc::new(Mutex::new(Vec::new()));
    let inputs = Arc::new(Mutex::new(Vec::new()));
    let received_inputs = inputs.clone();
    let received_outputs = outputs.clone();
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
        let scopes=if mode.load(Ordering::SeqCst)==6 {json!(["capabilities","create"])}else{json!(["capabilities","create","read","renew","release"])};
        Json(json!({"version":1,"ready":true,"torrent":mode.load(Ordering::SeqCst)!=5,"protocols":["hls"],"namespaces":["first","second"],"scopes":scopes,"available":{"inputs":capacity,"outputs":capacity,"viewers":5}}))
    }}))
        .route("/v1/sessions",axum::routing::post(move|Json(value):Json<Value>|{let created=created.clone();let state=state.clone();let waiting=waiting.clone();let outputs=received_outputs.clone();let inputs=received_inputs.clone();async move{
            inputs.lock().unwrap().push(value["input"].clone());
            outputs.lock().unwrap().push(value["output"].clone());
            let id={let mut created=created.lock().unwrap();created.push(value["namespace"].as_str().unwrap().into());format!("viewer_{}",created.len())};
            if state.load(Ordering::SeqCst)==2 {return (StatusCode::TOO_MANY_REQUESTS,Json(json!({"error":{"code":"source_connection_limit","message":"never expose provider-private-credential"}})));}
            if state.load(Ordering::SeqCst)==7 {return (StatusCode::TOO_MANY_REQUESTS,Json(json!({"error":{"code":"torrent_cache_capacity","message":"never expose private torrent state"}})));}
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
            inputs,
            outputs,
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

#[tokio::test]
async fn addon_torrent_playback_forwards_file_selection_and_rechecks_gateway_affinity() {
    let (mut app, peer) = setup().await;
    gateway(&app, "first", 1);
    app.addons.allow_test_loopback = true;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let addon = tokio::spawn(async move {
        axum::serve(listener, Router::new().route("/stream/movie/:id", axum::routing::get(|| async {
            Json(json!({"streams":[{"infoHash":"1".repeat(40),"fileIdx":2,"name":"Fixture"},{"infoHash":"1".repeat(40),"fileIdx":3,"name":"Fixture"}]}))
        }))).await.unwrap();
    });
    app.db.lock().unwrap().execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id) VALUES(1,'Fixture',?1,?2,1)",rusqlite::params![format!("{endpoint}/manifest.json"),json!({"id":"fixture","name":"Fixture","resources":["stream"],"types":["movie"]}).to_string()]).unwrap();
    crate::test_support::encrypt_fixture_sources(&app);
    let (_, start) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/streams",
        json!({"type":"movie","id":"fixture","only_addons":true}),
    )
    .await;
    let discovery = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (_, result) = request(
                &app,
                "member-token-1",
                "GET",
                &format!("/api/v2/streams/{}", start["id"].as_str().unwrap()),
                Value::Null,
            )
            .await;
            if result["done"] == true {
                break result;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let first = discovery["events"][0]["streams"][0]["id"].as_str().unwrap();
    let second = discovery["events"][0]["streams"][1]["id"].as_str().unwrap();
    assert_ne!(
        discovery["events"][0]["streams"][0]["source_fingerprint"],
        discovery["events"][0]["streams"][1]["source_fingerprint"]
    );
    for platform in ["android", "desktop", "web"] {
        let (status, start) = request(
            &app,
            "member-token-1",
            "POST",
            "/api/v2/playback",
            body(first, &format!("torrent-{platform}"), platform),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let ready = settled(&app, start["id"].as_str().unwrap()).await;
        assert_eq!(ready["status"], "ready");
        assert_eq!(ready["delivery"]["kind"], "gateway");
        assert_eq!(peer.inputs.lock().unwrap().last().unwrap()["file_index"], 2);
    }
    // Removing capability/scope support cannot be bypassed by a ready source's affinity.
    for (mode, expected) in [(5, "delivery_unsupported"), (6, "gateway_scope_missing")] {
        peer.mode.store(mode, Ordering::SeqCst);
        let before = peer.inputs.lock().unwrap().len();
        let (_, start) = request(
            &app,
            "member-token-1",
            "POST",
            "/api/v2/playback",
            body(first, &format!("torrent-refused-{mode}"), "android"),
        )
        .await;
        let refused = settled(&app, start["id"].as_str().unwrap()).await;
        assert_eq!(refused["error_code"], expected);
        assert_eq!(peer.inputs.lock().unwrap().len(), before);
    }
    // A distinct selected file is a distinct affinity identity.
    peer.mode.store(4, Ordering::SeqCst);
    gateway(&app, "second", 2);
    let (_, start) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/playback",
        body(second, "torrent-other-file", "android"),
    )
    .await;
    assert_eq!(
        settled(&app, start["id"].as_str().unwrap()).await["status"],
        "ready"
    );
    assert_eq!(peer.starts.lock().unwrap().last().unwrap(), "second");
    assert_eq!(peer.inputs.lock().unwrap().last().unwrap()["file_index"], 3);
    peer.mode.store(7, Ordering::SeqCst);
    let (_, start) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/playback",
        body(first, "torrent-cache-full", "android"),
    )
    .await;
    let refused = settled(&app, start["id"].as_str().unwrap()).await;
    assert_eq!(refused["error_code"], "gateway_capacity");
    assert!(!refused.to_string().contains("private torrent"));
    addon.abort();
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
async fn reflected_private_source_headers_are_hidden_in_cards_and_preserved_for_both_lease_deliveries(
) {
    for platform in ["android", "roku"] {
        let (app, peer) = setup().await;
        source(&app);
        gateway(&app, "first", 0);
        let scoped = app.clone().with_lease(lease());
        let cookie = "session=fixture-private-cookie";
        let key = "fixture-private-api-key";
        let (cards,error)=scoped.register("addon:1",vec![json!({
            "url":"http://source.fixture.invalid/private-input","name":"French Player",
            "title":format!("French en eng {cookie} fixture-private-cookie {key}"),
            "behaviorHints":{"proxyHeaders":{"request":{"Cookie":cookie,"X-API-Key":key,"Accept-Language":"en","User-Agent":"Player"}}}
        })],"movie");
        assert!(error.is_none());
        assert_eq!(cards.len(), 1);
        let public = serde_json::to_string(&cards).unwrap();
        for secret in [cookie, "fixture-private-cookie", key, "private-input"] {
            assert!(!public.contains(secret));
        }
        let id = cards[0]["id"].as_str().unwrap();
        let (status, foreign) = request(
            &app,
            "member-token-2",
            "POST",
            "/api/v2/playback",
            body(id, "foreign", platform),
        )
        .await;
        assert!(!status.is_success());
        assert!(!foreign.to_string().contains(key));
        assert!(!foreign.to_string().contains(cookie));
        let (status, started) = request(
            &app,
            "member-token-1",
            "POST",
            "/api/v2/playback",
            body(id, "private-headers", platform),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let playback = started["id"].as_str().unwrap();
        let ready = settled(&app, playback).await;
        assert_eq!(ready["status"], "ready");
        let headers = if platform == "android" {
            ready["delivery"]["headers"].clone()
        } else {
            peer.inputs.lock().unwrap()[0]["headers"].clone()
        };
        assert_eq!(headers["cookie"], cookie);
        assert_eq!(headers["x-api-key"], key);
        assert_eq!(headers["accept-language"], "en");
        assert_eq!(headers["user-agent"], "Player");
        if platform == "roku" {
            assert!(!ready.to_string().contains(key));
            assert!(!ready.to_string().contains(cookie));
        }
        request(
            &app,
            "member-token-1",
            "DELETE",
            &format!("/api/v2/playback/{playback}"),
            Value::Null,
        )
        .await;
    }
}

#[tokio::test]
async fn playback_never_falls_back_to_another_accounts_gateway_without_a_grant() {
    let (app, peer) = setup().await;
    let gateway = gateway(&app, "first", 0);
    app.db.lock().unwrap().execute_batch("INSERT INTO profiles(id,name,avatar_seed,presentation_complete) VALUES(2,'Second','two',1); INSERT INTO profile_owners VALUES(2,2,0); INSERT INTO auth_profiles VALUES(2,2); UPDATE auth_sessions SET profile_id=2 WHERE account_id=2; INSERT INTO addons(id,name,manifest_url,enabled,manifest,account_id) VALUES(2,'Second','https://second.fixture.invalid/manifest.json',1,'{}',2);").unwrap();
    crate::test_support::encrypt_fixture_sources(&app);
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
    for platform in ["roku", "vizio", "web", "webos"] {
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
async fn conversion_and_track_choices_require_and_reach_the_authorized_gateway() {
    let (app, peer) = setup().await;
    let source = source(&app);
    let mut input = body(&source, "selected_tracks", "android");
    input["conversion"] = json!("audio");
    input["audio_track"] = json!(2);
    input["preferred_audio_language"] = json!("pt-BR");
    input["subtitles_off"] = json!(true);
    let (status, refused) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/playback",
        input.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(refused["error_code"], "gateway_required");
    gateway(&app, "first", 0);
    let (status, start) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/playback",
        input.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let ready = settled(&app, start["id"].as_str().unwrap()).await;
    assert_eq!(ready["delivery"]["kind"], "gateway");
    assert_eq!(peer.outputs.lock().unwrap()[0]["conversion"], "audio");
    assert_eq!(peer.outputs.lock().unwrap()[0]["audio_track"], 2);
    assert_eq!(
        peer.outputs.lock().unwrap()[0]["preferred_audio_language"],
        "pt-BR"
    );
    assert_eq!(peer.outputs.lock().unwrap()[0]["subtitles_off"], true);
    assert_eq!(peer.outputs.lock().unwrap()[0]["max_height"], 2160);
    input["conversion"] = json!("video");
    assert_eq!(
        request(&app, "member-token-1", "POST", "/api/v2/playback", input)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let mut bad = body(&source, "bad_tracks", "android");
    bad["subtitles_off"] = json!(true);
    bad["subtitle_track"] = json!(3);
    assert_eq!(
        request(&app, "member-token-1", "POST", "/api/v2/playback", bad)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn v2_profile_preferences_are_scoped_snapshots_without_quality_clamping() {
    let (app, peer) = setup().await;
    let source = source(&app);
    let save = |audio: &str, subtitle: &str| {
        app.db.lock().unwrap().execute("INSERT INTO playback_preferences(profile_id,value) VALUES(1,?1) ON CONFLICT(profile_id) DO UPDATE SET value=excluded.value", [json!({"audio_language":audio,"subtitle_language":subtitle,"subtitles_enabled":true,"quality":"480p"}).to_string()]).unwrap();
    };
    save("ja", "fr");
    let direct = body(&source, "profile_snapshot", "android");
    let (status, admission) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/playback",
        direct.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let id = admission["id"].as_str().unwrap();
    let ready = settled(&app, id).await;
    assert_eq!(ready["delivery"]["kind"], "direct");
    assert_eq!(ready["delivery"]["preferences"]["audio_language"], "ja");
    assert_eq!(ready["delivery"]["preferences"]["subtitle_language"], "fr");
    assert!(ready["delivery"]["preferences"].get("quality").is_none());
    save("de", "es");
    let (status, repeated) =
        request(&app, "member-token-1", "POST", "/api/v2/playback", direct).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(repeated["id"], id);
    assert_eq!(repeated["delivery"]["preferences"]["audio_language"], "ja");
    gateway(&app, "first", 0);
    let managed = body(&source, "profile_new", "roku");
    let (status, admission) =
        request(&app, "member-token-1", "POST", "/api/v2/playback", managed).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(
        settled(&app, admission["id"].as_str().unwrap()).await["status"],
        "ready"
    );
    let output = peer.outputs.lock().unwrap()[0].clone();
    assert_eq!(output["preferred_audio_language"], "de");
    assert_eq!(output["preferred_subtitle_language"], "es");
    assert_eq!(output["max_height"], 2160);
    let mut off = body(&source, "profile_off", "roku");
    off["subtitles_off"] = json!(true);
    off["preferred_audio_language"] = json!("it");
    let (_, admission) = request(&app, "member-token-1", "POST", "/api/v2/playback", off).await;
    assert_eq!(
        settled(&app, admission["id"].as_str().unwrap()).await["status"],
        "ready"
    );
    let output = peer.outputs.lock().unwrap()[1].clone();
    assert_eq!(output["preferred_audio_language"], "it");
    assert!(output["preferred_subtitle_language"].is_null());
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
    {
        let db = app.db.lock().unwrap();
        let secret = app.secret_vault.as_ref().unwrap().seal(1,"xtream","1",json!({"url":"http://provider.fixture.invalid","username":"user","password":"password"}).to_string().as_bytes()).unwrap();
        db.execute(
            "INSERT INTO provider_credentials_v2 VALUES(1,1,?1)",
            [secret],
        )
        .unwrap();
        db.execute(
            "UPDATE providers SET url='',username='',password='',credentials_version=1 WHERE id=1",
            [],
        )
        .unwrap();
        // Deliberately no ownership: admission must still reject this fixture.
    }
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
