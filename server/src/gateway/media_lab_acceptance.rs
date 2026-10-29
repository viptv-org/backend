use serde_json::{json, Value};

#[tokio::test]
#[ignore = "owned media lab, synthetic account and trusted HTTPS ingress required"]
async fn media_lab_backend() {
    let path = std::path::PathBuf::from(
        std::env::var_os("VIPTV_MEDIA_LAB_CONFIG").expect("explicit media lab config"),
    );
    let config: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let bind: std::net::SocketAddr = config["backend_bind"].as_str().unwrap().parse().unwrap();
    assert!(bind.ip().is_loopback() && bind.port() >= 1024);
    let target: url::Url = config["gateway_control"].as_str().unwrap().parse().unwrap();
    assert_eq!(target.scheme(), "http");
    assert_eq!(target.host_str(), Some("127.0.0.1"));
    let mut app = crate::auth_integration_tests::fixture();
    if let Some(password) = config["browser_password"].as_str() {
        use argon2::{
            password_hash::{rand_core::OsRng, PasswordHasher, SaltString},
            Argon2,
        };
        assert!(password.len() >= 12, "explicit disposable browser password required");
        let hash = Argon2::default()
            .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
            .unwrap()
            .to_string();
        app.db.lock().unwrap().execute(
            "UPDATE auth_accounts SET password_hash=?1 WHERE id=1", [hash],
        ).unwrap();
    }
    app.addons.allow_test_loopback = true;
    app.gateway_client = super::client::Client::fixture(target);
    app.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE auth_sessions SET access_expires=?1,refresh_expires=?1",
            [crate::util::now() + 172800],
        )
        .unwrap();
    let (status, _) = crate::test_support::request(
        &app,
        "member-token-1",
        "POST",
        "/api/gateways",
        json!({
            "name":"Media laboratory", "endpoint":config["gateway_endpoint"],
            "namespace":"fixture", "priority":10, "integration_key":config["gateway_key"]
        }),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let (status, _) = crate::test_support::request(
        &app,
        "member-token-1",
        "POST",
        "/api/addons",
        json!({
            "manifest_url":config["addon_manifest"]
        }),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let selected: Vec<Value> = config["selection_file"]
        .as_str()
        .map(|path| serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap())
        .unwrap_or_default();
    let live: Vec<_> = selected
        .iter()
        .filter(|item| item["live"] == true)
        .collect();
    if !live.is_empty() {
        #[cfg(not(feature = "live-runtime"))]
        panic!("media lab live sources require --features live-runtime");
        #[cfg(feature = "live-runtime")]
        {
            let (status, _) = crate::test_support::request(&app, "member-token-1", "PUT", "/api/profiles/1/source-integrations", json!({
                "revision":0,"plugin_id":"m3u","configuration":{"playlist_url":"https://media-lab.example/owned.m3u"}
            })).await;
            assert_eq!(status, axum::http::StatusCode::OK);
            let (_, schemas) = crate::test_support::request(
                &app,
                "member-token-1",
                "GET",
                "/api/profiles/1/source-integrations",
                Value::Null,
            )
            .await;
            let connection = schemas["connections"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["plugin_id"] == "m3u")
                .unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned();
            let mut playlist = String::from("#EXTM3U\n");
            for item in live {
                let id = item["id"].as_str().unwrap();
                let file = item["file"].as_str().unwrap();
                assert!(!id.contains(['\r', '\n']) && !file.contains(['\r', '\n']));
                playlist.push_str(&format!(
                    "#EXTINF:-1,{id}\n{}/{file}\n",
                    config["source_origin"].as_str().unwrap()
                ));
            }
            let channels = viptv_engine::live::parse_m3u(
                &playlist,
                &url::Url::parse("https://media-lab.example/owned.m3u").unwrap(),
                &connection,
            )
            .unwrap();
            app.live_cache.lock().await.insert(
                connection,
                crate::engine_v3::live::Cache {
                    revision: 1,
                    expires: std::time::Instant::now() + std::time::Duration::from_secs(172800),
                    channels,
                    groups: vec![],
                    guides: Default::default(),
                    xmltv: None,
                },
            );
        }
    }
    let listener = tokio::net::TcpListener::bind(bind).await.unwrap();
    println!("MEDIA_LAB_BACKEND_READY");
    axum::serve(listener, crate::router(app))
        .await
        .unwrap();
}
