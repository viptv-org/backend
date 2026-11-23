use super::*;
use crate::{auth, auth_integration_tests::fixture, test_support::request};
use tracing::instrument::WithSubscriber;

fn lease() -> ResourceLease {
    ResourceLease {
        policy_revision: 0,
        principal: auth::Principal::Account {
            account_id: 1,
            role: "owner".into(),
            profile_id: Some(1),
            session_id: Some("s1".into()),
        },
        session_id: Some("s1".into()),
    }
}
fn register(app: &App, input: Value, kind: &str) -> String {
    app.db.lock().unwrap().execute("INSERT OR IGNORE INTO addons(id,name,manifest_url,enabled,manifest,account_id) VALUES(1,'Fixture','https://addon.fixture.invalid/manifest.json',1,'{}',1)",[]).unwrap();
    crate::test_support::encrypt_fixture_sources(app);
    let (cards, error) = app.clone().with_lease(lease()).register("addon:1", vec![input], kind);
    assert!(error.is_none());
    cards[0]["id"].as_str().unwrap().to_owned()
}
fn direct_source(app: &App) -> String {
    register(app, json!({"url":"http://source.fixture.invalid/episode.mkv","name":"Fixture"}), "movie")
}
fn torrent_source(app: &App) -> String {
    let id = register(app, json!({"infoHash":"0".repeat(40),"fileIdx":0}), "series");
    app.streams.lock().unwrap().get_mut(&id).unwrap().exact_vod = Some(crate::app_state::ExactVod {
        title: "episode_fixture".into(),
        series: Some("series_fixture".into()),
        season: Some(1),
        episode: Some(2),
    });
    id
}
fn download(source: &str, id: &str, platform: &str) -> Value {
    json!({"purpose":"download","request_id":id,"stream_id":source,"client":{"platform":platform,"can_play_direct":true,"max_width":3840,"max_height":2160,"video_codecs":["h264"],"audio_codecs":["aac"]}})
}
fn runtime_download(source: &str, id: &str) -> Value {
    let mut body = download(source, id, "android");
    body["client"]["can_play_direct"] = json!(false);
    body["client"]["native_torrent"] = json!({"version":2,"network_policy":"public_discovery_verified_v2"});
    body
}
fn side_effects(app: &App) -> (usize, i64, i64) {
    let db = app.db.lock().unwrap();
    let count = |sql: &str| db.query_row(sql, [], |row| row.get::<_, i64>(0)).unwrap();
    (
        app.gateway_playbacks.entries.lock().unwrap().len(),
        count("SELECT count(*) FROM playback_request_authority"),
        count("SELECT count(*) FROM progress"),
    )
}
async fn admit(app: &App, token: &str, body: Value) -> (StatusCode, Value) {
    request(app, token, "POST", "/api/playback", body).await
}

#[tokio::test]
async fn direct_download_returns_delivery_without_lease_viewing_or_url_in_logs() {
    let app = fixture();
    let source = direct_source(&app);
    let (subscriber, logs) = crate::test_support::capture_logs(tracing::Level::INFO);
    let (status, value) = admit(&app, "member-token-1", download(&source, "download_direct", "android"))
        .with_subscriber(subscriber)
        .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(
        value,
        json!({"purpose":"download","status":"ready","delivery":{"kind":"direct","url":"http://source.fixture.invalid/episode.mkv","headers":{},"format":"original","size":null,"expires_at":null}})
    );
    assert_eq!(side_effects(&app), (0, 0, 0));
    let logs = logs.text();
    let lines = logs
        .lines()
        .filter(|line| line.contains("Download admission accepted"))
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 1, "{logs}");
    for fact in [
        format!("session_tag={}", crate::gateway::diagnostics::tag("s1")),
        format!("profile_tag={}", crate::gateway::diagnostics::tag("1")),
        "delivery=\"direct\"".into(),
    ] {
        assert!(lines[0].contains(&fact), "{fact}: {}", lines[0]);
    }
    assert!(!logs.contains("source.fixture.invalid"), "{logs}");
    assert!(!logs.contains("Playback admission accepted"), "{logs}");
}

#[tokio::test]
async fn sources_that_need_the_gateway_are_not_downloadable() {
    let app = fixture();
    let direct = direct_source(&app);
    let torrent = torrent_source(&app);
    let mut forced = download(&direct, "download_forced", "android");
    forced["force_gateway"] = json!(true);
    let mut converted = download(&direct, "download_converted", "android");
    converted["conversion"] = json!("audio");
    let mut undeclared = download(&direct, "download_undeclared", "android");
    undeclared["client"]["can_play_direct"] = json!(false);
    for body in [
        download(&direct, "download_roku", "roku"),
        download(&direct, "download_vizio", "vizio"),
        forced,
        converted,
        undeclared,
        download(&torrent, "download_torrent", "android"),
        runtime_download(&torrent, "download_unnegotiated"),
    ] {
        let (status, value) = admit(&app, "member-token-1", body).await;
        assert_eq!(status, StatusCode::NOT_ACCEPTABLE, "{value}");
        assert_eq!(value["error_code"], "download_unsupported");
    }
    assert_eq!(side_effects(&app), (0, 0, 0));
}

#[tokio::test]
async fn negotiated_runtime_download_returns_the_torrent_descriptor_without_a_lease() {
    let app = fixture();
    let (status, _) = request(&app, "member-token-1", "GET", "/api/torrent-runtime-protocol", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    let source = torrent_source(&app);
    let (status, value) = admit(&app, "member-token-1", runtime_download(&source, "download_runtime")).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["purpose"], "download");
    assert_eq!(value["status"], "ready");
    let delivery = &value["delivery"];
    assert_eq!(delivery["kind"], "native_torrent");
    assert_eq!(delivery["format"], "original");
    assert!(delivery["expires_at"].is_null());
    assert!(delivery["size"].is_null());
    assert!(delivery.get("url").is_none());
    let grant = &delivery["grant"];
    assert_eq!(grant["version"], 2);
    assert_eq!(grant["network_policy"], "public_discovery_verified_v2");
    assert_eq!(grant["info_hash"], "0".repeat(40));
    assert_eq!(grant["file_index"], 0);
    assert_eq!(grant["input"], json!({"kind":"magnet","uri":format!("magnet:?xt=urn:btih:{}", "0".repeat(40))}));
    assert!(value.get("id").is_none() && value.get("renew_after_seconds").is_none());
    assert_eq!(side_effects(&app), (0, 0, 0));
}

#[tokio::test]
async fn foreign_profiles_and_accounts_cannot_download_a_source() {
    let app = fixture();
    let source = direct_source(&app);
    let (status, value) = admit(&app, "member-token-2", download(&source, "download_foreign_account", "android")).await;
    assert!(status.is_client_error(), "{value}");
    assert!(value.get("delivery").is_none(), "{value}");
    {
        let db = app.db.lock().unwrap();
        db.execute("INSERT INTO profiles(id,name,avatar_seed,presentation_complete) VALUES(2,'Other','fixture-two',1)", []).unwrap();
        db.execute("INSERT INTO profile_owners(profile_id,account_id,created_at) VALUES(2,1,0)", []).unwrap();
        db.execute("INSERT INTO auth_profiles VALUES(1,2)", []).unwrap();
        db.execute("UPDATE auth_sessions SET profile_id=2 WHERE id='s1'", []).unwrap();
    }
    let (status, value) = admit(&app, "member-token-1", download(&source, "download_foreign_profile", "android")).await;
    assert!(status.is_client_error(), "{value}");
    assert!(value.get("delivery").is_none(), "{value}");
    assert_eq!(side_effects(&app), (0, 0, 0));
}

#[tokio::test]
async fn download_admissions_are_rate_limited_per_profile() {
    let app = fixture();
    let source = direct_source(&app);
    for index in 0..60 {
        let (status, value) = admit(&app, "member-token-1", download(&source, &format!("download_{index}"), "android")).await;
        assert_eq!(status, StatusCode::OK, "{index}: {value}");
    }
    let (status, value) = admit(&app, "member-token-1", download(&source, "download_over", "android")).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{value}");
    assert_eq!(value["error_code"], "download_rate_limited");
    let limiter = downloads::limiter();
    let now = Instant::now();
    assert!((0..60).all(|_| limiter.admit("1:1", now)));
    assert!(!limiter.admit("1:1", now));
    assert!(limiter.admit("1:2", now));
    assert!(limiter.admit("1:1", now + Duration::from_secs(3600)));
}

#[tokio::test]
async fn download_rejects_playback_only_options_and_unknown_purposes() {
    let app = fixture();
    let source = direct_source(&app);
    for (field, value) in [("prepare", json!(true)), ("resume", json!(true)), ("position", json!(30))] {
        let mut body = download(&source, field, "android");
        body[field] = value;
        let (status, error) = admit(&app, "member-token-1", body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{field}: {error}");
        assert_eq!(error["error_code"], "invalid_playback_request");
    }
    let mut decoder = download(&source, "download_decoder", "android");
    decoder["purpose"] = json!("download");
    let (status, _) = request(&app, "member-token-1", "POST", "/api/playback-decoder-start", decoder).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let mut unknown = download(&source, "download_unknown", "android");
    unknown["purpose"] = json!("offline");
    let (status, error) = admit(&app, "member-token-1", unknown).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error_code"], "invalid_playback_request");
    assert_eq!(side_effects(&app), (0, 0, 0));
}

#[tokio::test]
async fn default_and_explicit_playback_purposes_keep_the_leased_start() {
    let app = fixture();
    let source = direct_source(&app);
    let mut plain = download(&source, "playback_plain", "android");
    plain.as_object_mut().unwrap().remove("purpose");
    let mut explicit = plain.clone();
    explicit["purpose"] = json!("playback");
    let parse = |body: &Value| serde_json::from_value::<Start>(body.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(parse(&explicit)).unwrap(),
        serde_json::to_value(parse(&plain)).unwrap()
    );
    assert!(serde_json::to_value(parse(&plain)).unwrap().get("purpose").is_none());
    let (status, start) = admit(&app, "member-token-1", plain).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{start}");
    assert!(start["id"].is_string());
    assert_eq!(start["renew_after_seconds"], 20);
    let (status, retry) = admit(&app, "member-token-1", explicit).await;
    assert_eq!(status, StatusCode::OK, "{retry}");
    assert_eq!(retry["id"], start["id"]);
    let (entries, authority, _) = side_effects(&app);
    assert_eq!((entries, authority), (1, 1));
}
