//! Opt-in disposable producer for TV-web HTTPS acceptance. Only the external
//! add-on is a fixture; auth, profiles, discovery and mutations use real routes.
use super::*;

#[tokio::test]
#[ignore = "owned TV-web HTTPS ingress and synthetic add-on required"]
async fn remote_tv_backend() {
    let path = std::env::var_os("VIPTV_REMOTE_TV_FIXTURE").expect("explicit fixture config");
    let config: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let bind: std::net::SocketAddr = config["bind"].as_str().unwrap().parse().unwrap();
    assert!(bind.ip().is_loopback() && bind.port() >= 1024);
    let addon: url::Url = config["addon_manifest"].as_str().unwrap().parse().unwrap();
    assert_eq!(addon.host_str(), Some("127.0.0.1"));
    assert_eq!(addon.scheme(), "http");
    let dist = std::path::PathBuf::from(config["dist"].as_str().unwrap());
    assert!(dist.is_absolute() && dist.join("index.html").is_file());
    let mut app = crate::auth_integration_tests::fixture();
    app.addons.allow_test_loopback = true;
    // Keep only the seeded test operators alive for a long acceptance session.
    // Device grants created later still use the normal auth/refresh lifetimes.
    app.db.lock().unwrap().execute("UPDATE auth_sessions SET access_expires=?1,refresh_expires=?1",[crate::util::now()+172800]).unwrap();
    let (status, result) = crate::test_support::request(
        &app, "member-token-1", "POST", "/api/addons",
        json!({"manifest_url":addon.as_str()}),
    ).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let addon_id = result["id"].as_i64().unwrap();
    // Pin fixture discovery to this owned add-on, regardless of optional
    // Simkl test environment settings used by the wider server test suite.
    let (status, result) = crate::test_support::request(
        &app, "member-token-1", "PUT", "/api/profiles/1/integrations",
        json!({"revision":0,"browsing_plugin":"stremio","assignments":[{
            "connection_id":format!("addon:{addon_id}"),
            "enabled":["Metadata","Sources","Discovery","Search"]
        }]}),
    ).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let listener = tokio::net::TcpListener::bind(bind).await.unwrap();
    println!("REMOTE_TV_BACKEND_READY");
    axum::serve(listener, crate::router_with_viewers(app, Some(dist)))
        .await.unwrap();
}
