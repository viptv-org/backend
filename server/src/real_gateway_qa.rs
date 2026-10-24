//! Explicit local qualification host. Real gateway control/media are unchanged;
//! only the control DNS/TLS mapping is injected, and absent from production.
use crate::*;

#[tokio::test]
#[ignore = "Own isolated QA database/keyring and real gateway are required"]
async fn real_provider_gateway_backend() {
    let database = std::env::var("VIPTV_DATABASE").expect("explicit disposable database");
    let path = std::path::Path::new(&database);
    assert!(path.starts_with("/tmp") && database.contains("viptv-real-provider-"));
    assert!(path.exists());
    let bind = std::env::var("VIPTV_BIND").expect("explicit loopback bind");
    let bind_address: std::net::SocketAddr = bind.parse().expect("explicit socket address");
    assert_eq!(bind_address.ip(), std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
    assert!(bind_address.port() >= 1024, "owned nonprivileged loopback port");
    let gateway = std::env::var("VIPTV_QA_GATEWAY_CONTROL").expect("owned gateway control");
    let target = url::Url::parse(&gateway).unwrap();
    assert_eq!(target.scheme(), "http");
    assert_eq!(target.host_str(), Some("127.0.0.1"));
    assert!(target.port().is_some_and(|port| port >= 1024 && port != bind_address.port()));
    assert!(target.username().is_empty() && target.password().is_none());
    assert!(target.query().is_none() && target.fragment().is_none());
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(25))
        .connect_timeout(Duration::from_secs(8))
        .user_agent("VIPTV/0.1")
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let mut app = App::new(Connection::open(path).unwrap(), client).unwrap();
    app.gateway_client = gateway::client::Client::fixture(target);
    // An opt-in local add-on fixture supplies metadata/streams/captions only.
    // Gateway upstream egress still uses its ordinary production policy.
    if std::env::var("VIPTV_QA_ALLOW_LOOPBACK_ADDONS").as_deref() == Ok("true") {
        app.addons.allow_test_loopback = true;
    }
    let listener = tokio::net::TcpListener::bind(&bind).await.unwrap();
    println!("Real provider/gateway QA backend ready on owned loopback listener");
    axum::serve(listener, router(app))
        .with_graceful_shutdown(async { tokio::signal::ctrl_c().await.unwrap() })
        .await
        .unwrap();
}
