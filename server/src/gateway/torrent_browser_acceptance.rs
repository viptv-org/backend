//! Real product API fixture for the isolated gateway/browser torrent qualification.
//! Generated media and ephemeral accounts only; never compiled into production.
use serde_json::{json, Value};
use std::time::Duration;

#[tokio::test]
#[ignore = "requires an isolated real gateway, generated addon, trusted HTTPS ingress"]
async fn actual_backend_torrent_browser_server() {
    let path =
        std::env::var("VIPTV_TORRENT_BROWSER_CONFIG").expect("private fixture config required");
    let config: Value =
        serde_json::from_slice(&std::fs::read(path).expect("private fixture config unavailable"))
            .expect("private fixture config invalid");
    let mut app = crate::auth_integration_tests::fixture();
    app.addons.allow_test_loopback = true;
    app.gateway_client = super::client::Client::fixture(
        config["gateway_control"]
            .as_str()
            .expect("fixture control endpoint required")
            .parse()
            .unwrap(),
    );
    app.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE auth_accounts SET name='Gateway browser fixture'",
            [],
        )
        .unwrap();
    let (status,_)=crate::test_support::request(&app,"member-token-1","POST","/api/v2/gateways",json!({
        "name":"Generated torrent gateway","endpoint":config["gateway_endpoint"],"namespace":"fixture","priority":10,"integration_key":config["gateway_key"]
    })).await;
    assert_eq!(
        status,
        axum::http::StatusCode::OK,
        "real gateway registration/capability verification must succeed"
    );
    let (status, _) = crate::test_support::request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/addons",
        json!({"manifest_url":config["addon_manifest"]}),
    )
    .await;
    assert_eq!(
        status,
        axum::http::StatusCode::OK,
        "real encrypted addon registration must succeed"
    );
    let listener = tokio::net::TcpListener::bind(
        config["backend_bind"]
            .as_str()
            .expect("fixture listener required"),
    )
    .await
    .unwrap();
    println!("Actual backend account/profile/vault/addon/gateway router ready.");
    tokio::time::timeout(
        Duration::from_secs(900),
        axum::serve(listener, crate::router(app, None)),
    )
    .await
    .expect("owned backend fixture deadline")
    .unwrap();
}
