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
    assert_eq!(bind, "127.0.0.1:18185");
    let gateway = std::env::var("VIPTV_QA_GATEWAY_CONTROL").expect("owned gateway control");
    let target = url::Url::parse(&gateway).unwrap();
    assert_eq!(target.scheme(), "http");
    assert_eq!(target.host_str(), Some("127.0.0.1"));
    assert_eq!(target.port(), Some(18187));
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
    let listener = tokio::net::TcpListener::bind(&bind).await.unwrap();
    println!("Real provider/gateway QA backend ready on owned loopback listener");
    axum::serve(listener, router(app, None))
        .with_graceful_shutdown(async { tokio::signal::ctrl_c().await.unwrap() })
        .await
        .unwrap();
}
