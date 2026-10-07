//! Isolated owned Android qualification server; excluded from production builds.
use crate::{app_state::ExactVod, app_state::ResourceLease, auth};
use serde_json::{json, Value};
use std::{
    net::{Ipv4Addr, SocketAddrV4},
    path::PathBuf,
    time::Duration,
};

#[tokio::test]
#[ignore = "requires private generated owned-media configuration and isolated Android fixture APK"]
async fn actual_backend_android_native_server() {
    let path = PathBuf::from(
        std::env::var("VIPTV_ANDROID_NATIVE_CONFIG").expect("private config required"),
    );
    let config: Value =
        serde_json::from_slice(&std::fs::read(&path).expect("fixture config unavailable"))
            .expect("fixture config invalid");
    let hash = config["info_hash"].as_str().expect("owned hash required");
    assert!(hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit()));
    let app = crate::auth_integration_tests::fixture();
    {
        let db = app.db.lock().unwrap();
        db.execute("INSERT INTO profiles(id,name,avatar_seed,presentation_complete) VALUES(2,'Other owned profile','owned-two',1)", []).unwrap();
        db.execute(
            "INSERT INTO profile_owners(profile_id,account_id,created_at) VALUES(2,1,0)",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO auth_profiles VALUES(1,2)", [])
            .unwrap();
        db.execute(
            "UPDATE auth_sessions SET kind='device',refresh_hash=?1 WHERE id='s1'",
            [auth::hash("owned-unused-refresh")],
        )
        .unwrap();
    }
    app.gateway_playbacks.enable_owned_native_fixture();
    app.db.lock().unwrap().execute("INSERT INTO addons(id,name,manifest_url,enabled,manifest,account_id) VALUES(1,'Owned episodes','https://fixture.invalid/manifest.json',1,'{}',1)", []).unwrap();
    crate::test_support::encrypt_fixture_sources(&app);
    let lease = ResourceLease {
        policy_revision: 0,
        principal: auth::Principal::Account {
            account_id: 1,
            role: "member".into(),
            profile_id: Some(1),
            session_id: Some("s1".into()),
        },
        session_id: Some("s1".into()),
    };
    let mut sources = Vec::new();
    for index in [1_u32, 2, 99] {
        let (cards, error) = app.clone().with_lease(lease.clone()).register(
            "addon:1",
            vec![json!({"infoHash": hash, "fileIdx": index})],
            "series",
        );
        assert!(error.is_none());
        let id = cards[0]["id"].as_str().unwrap().to_owned();
        app.streams.lock().unwrap().get_mut(&id).unwrap().exact_vod = Some(ExactVod {
            title: format!("owned_episode_{index}"),
            series: Some("owned_series".into()),
            season: Some(1),
            episode: Some(index),
        });
        sources.push(json!({"index": index, "stream_id": id}));
    }
    let origin: url::Url = config["origin"].as_str().unwrap().parse().unwrap();
    assert_eq!(origin.scheme(), "https");
    assert_eq!(origin.host_str(), Some("127.0.0.1"));
    assert!(
        origin.username().is_empty()
            && origin.password().is_none()
            && origin.path() == "/"
            && origin.query().is_none()
            && origin.fragment().is_none()
    );
    let mut ordinary_sources = Vec::new();
    for (kind, path) in [
        ("direct", "/owned/episode.mp4"),
        ("hls", "/owned/hls/index.m3u8"),
    ] {
        let (cards, error) = app.clone().with_lease(lease.clone()).register(
            "addon:1",
            vec![json!({"url": origin.join(path).unwrap().as_str()})],
            "movie",
        );
        assert!(error.is_none());
        let id = cards[0]["id"].as_str().unwrap().to_owned();
        ordinary_sources.push(json!({"kind":kind,"stream_id":id}));
    }
    let port: u16 = config["port"]
        .as_u64()
        .expect("fixture port required")
        .try_into()
        .unwrap();
    let listener = tokio::net::TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
        .await
        .unwrap();
    let ready = path.with_file_name("backend-ready.json");
    std::fs::write(ready, serde_json::to_vec(&json!({"port": listener.local_addr().unwrap().port(), "access_token": "member-token-1", "profile_id": "1", "sources": sources, "ordinary_sources": ordinary_sources,
        "core_session": {"sessionId":"s1","accountId":"1","profileId":"1","accessToken":"member-token-1","refreshToken":"owned-unused-refresh","expiresIn":3600}})).unwrap()).unwrap();
    // This finite process owns only its in-memory database. Commands cannot
    // affect production accounts, fixtures in another process, or an owner APK.
    let commands = path.with_file_name("backend-command.json");
    let command_app = app.clone();
    let watcher = tokio::spawn(async move {
        loop {
            if let Ok(bytes) = std::fs::read(&commands) {
                if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                    let db = command_app.db.lock().unwrap();
                    if value["disable_source"] == true {
                        db.execute("UPDATE addons SET enabled=0 WHERE id=1", [])
                            .unwrap();
                    }
                    if value["revoke"] == true {
                        db.execute("UPDATE auth_sessions SET access_expires=0,refresh_expires=0 WHERE id='s1'", []).unwrap();
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });
    println!("Owned Android authorization fixture ready.");
    let result = tokio::time::timeout(
        Duration::from_secs(900),
        axum::serve(listener, crate::router(app, None)),
    )
    .await;
    watcher.abort();
    if let Ok(result) = result {
        result.unwrap();
    }
}
