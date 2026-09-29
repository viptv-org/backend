use super::*;
use crate::{auth_integration_tests::fixture, secret_store::Vault, test_support::request, App};
use axum::{
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

struct Peer {
    task: tokio::task::JoinHandle<()>,
    calls: Arc<AtomicUsize>,
    mode: Arc<AtomicUsize>,
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn key() -> String {
    format!("pgk_{}", "a".repeat(64))
}
fn vault(byte: u8) -> Vault {
    Vault::from_json(
        &json!({"active":"test","keys":{"test":STANDARD.encode([byte;32])}}).to_string(),
    )
    .unwrap()
}
fn registration() -> Value {
    json!({"name":"Private gateway","endpoint":"https://gateway.example.test/","namespace":"fixture","priority":10,"integration_key":key()})
}
async fn setup() -> (App, Peer) {
    let mut app = fixture();
    app.secret_vault = Some(Arc::new(vault(7)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    app.gateway_client = client::Client::fixture(
        format!("http://{}/", listener.local_addr().unwrap())
            .parse()
            .unwrap(),
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let mode = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let behavior = mode.clone();
    let routes=Router::new().route("/v1/capabilities",axum::routing::get(move|headers:HeaderMap|{
        let count=count.clone();let behavior=behavior.clone();async move {
            count.fetch_add(1,Ordering::SeqCst);
            assert_eq!(headers.get("authorization").unwrap().to_str().unwrap(),format!("Bearer {}",key()));
            match behavior.load(Ordering::SeqCst) {
                1=>StatusCode::UNAUTHORIZED.into_response(),
                2=>(StatusCode::FOUND,[("location","/must-not-follow")]).into_response(),
                3=>(StatusCode::OK,"x".repeat(65537)).into_response(),
                mode=>Json(json!({"version":1,"ready":mode!=5,"protocols":["hls","progressive"],"namespaces":["fixture"],"scopes":if mode==4 {vec!["capabilities"]}else{vec!["capabilities","create","read","renew","release"]}})).into_response(),
            }
        }
    })).route("/must-not-follow",axum::routing::get(||async{panic!("redirect must not be followed"); #[allow(unreachable_code)] StatusCode::OK}));
    let task = tokio::spawn(async move {
        axum::serve(listener, routes).await.unwrap();
    });
    (app, Peer { task, calls, mode })
}

#[tokio::test]
async fn gateway_keys_are_encrypted_and_family_gateway_has_no_implicit_public_grant() {
    let (app, peer) = setup().await;
    let (status, added) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/gateways",
        registration(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{added}");
    let id = added["id"].as_str().unwrap();
    assert!(!added.to_string().contains(&key()) && added.get("secret").is_none());
    let envelope: String = app
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT secret FROM playback_gateways WHERE id=?1",
            [id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!envelope.contains(&key()));
    assert!(app
        .secret_vault
        .as_ref()
        .unwrap()
        .open(2, "gateway_key", id, &envelope)
        .is_err());
    let (_, other) = request(
        &app,
        "member-token-2",
        "GET",
        "/api/v2/gateways",
        Value::Null,
    )
    .await;
    assert!(other["items"].as_array().unwrap().is_empty());
    assert_eq!(
        request(
            &app,
            "member-token-2",
            "POST",
            &format!("/api/v2/gateways/{id}/check"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(peer.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            &format!("/api/v2/gateways/{id}/grants"),
            json!({"account_id":2,"enabled":true})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE auth_accounts SET role='owner' WHERE id=1", [])
        .unwrap();
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            &format!("/api/v2/gateways/{id}/grants"),
            json!({"account_id":2,"enabled":true})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, other) = request(
        &app,
        "member-token-2",
        "GET",
        "/api/v2/gateways",
        Value::Null,
    )
    .await;
    assert_eq!(other["items"][0]["id"], id);
    assert_eq!(other["items"][0]["can_manage"], false);
    assert_eq!(
        request(
            &app,
            "member-token-2",
            "POST",
            &format!("/api/v2/gateways/{id}/check"),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "member-token-2",
            "PATCH",
            &format!("/api/v2/gateways/{id}"),
            json!({"enabled":false})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    app.db
        .lock()
        .unwrap()
        .execute_batch("UPDATE auth_accounts SET role='member' WHERE id=1; UPDATE auth_accounts SET role='owner' WHERE id=2;")
        .unwrap();
    assert_eq!(
        request(
            &app,
            "member-token-2",
            "PUT",
            &format!("/api/v2/gateways/{id}"),
            registration()
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    app.db.lock().unwrap().execute_batch("UPDATE auth_accounts SET role='member' WHERE id=2; UPDATE auth_accounts SET role='owner' WHERE id=1;").unwrap();
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PATCH",
            &format!("/api/v2/gateways/{id}"),
            json!({"name":"Label change","priority":20})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, listed) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v2/gateways",
        Value::Null,
    )
    .await;
    assert_eq!(
        listed["items"][0]["revision"], 1,
        "cosmetic/ranking edits must not invalidate existing playback affinity"
    );
    let mut replacement = registration();
    replacement["name"] = json!("Renamed gateway");
    let (_, updated) = request(
        &app,
        "member-token-1",
        "PUT",
        &format!("/api/v2/gateways/{id}"),
        replacement,
    )
    .await;
    assert_eq!(updated["id"], id);
    assert_eq!(updated["revision"], 2);
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            &format!("/api/v2/gateways/{id}/grants"),
            json!({"account_id":2,"enabled":false})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "member-token-2",
            "POST",
            &format!("/api/v2/gateways/{id}/check"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn invalid_endpoints_credentials_and_keyring_fail_closed_without_persistence() {
    let (mut app, peer) = setup().await;
    for endpoint in [
        "http://public.example.test",
        "https://127.0.0.1",
        "https://[::ffff:127.0.0.1]",
        "https://169.254.169.254",
        "https://user:secret@example.test",
        "https://example.test/?key=private",
    ] {
        let mut body = registration();
        body["endpoint"] = json!(endpoint);
        assert_eq!(
            request(&app, "member-token-1", "POST", "/api/v2/gateways", body)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    let mut bootstrap = registration();
    bootstrap["integration_key"] = json!("bootstrap-private-value-that-must-not-be-sent");
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/v2/gateways",
            bootstrap
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(peer.calls.load(Ordering::SeqCst), 0);
    for (mode, code, status) in [
        (1, "gateway_key_rejected", StatusCode::UNPROCESSABLE_ENTITY),
        (
            2,
            "gateway_redirect_rejected",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            3,
            "gateway_protocol_invalid",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (4, "gateway_scope_missing", StatusCode::UNPROCESSABLE_ENTITY),
        (5, "gateway_not_ready", StatusCode::BAD_GATEWAY),
    ] {
        peer.mode.store(mode, Ordering::SeqCst);
        let (actual, error) = request(
            &app,
            "member-token-1",
            "POST",
            "/api/v2/gateways",
            registration(),
        )
        .await;
        assert_eq!(actual, status);
        assert_eq!(error["error_code"], code);
        assert!(!error.to_string().contains(&key()));
    }
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM playback_gateways", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    peer.mode.store(0, Ordering::SeqCst);
    let (_, added) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/gateways",
        registration(),
    )
    .await;
    app.secret_vault = Some(Arc::new(vault(8)));
    let (status, error) = request(
        &app,
        "member-token-1",
        "POST",
        &format!("/api/v2/gateways/{}/check", added["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error["error_code"], "secret_authentication_failed");
    app.secret_vault = None;
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/v2/gateways",
            registration()
        )
        .await
        .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[test]
fn gateway_network_policy_rejects_private_reserved_and_transition_addresses() {
    for ip in [
        "127.0.0.1",
        "10.0.0.1",
        "192.168.1.1",
        "169.254.169.254",
        "100.64.0.1",
        "198.18.0.1",
        "192.0.2.1",
        "::1",
        "::ffff:127.0.0.1",
        "fc00::1",
        "2002:7f00:1::",
        "2001:db8::1",
    ] {
        assert!(!client::public_ip(ip.parse().unwrap()));
    }
    for ip in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
        assert!(client::public_ip(ip.parse().unwrap()));
    }
    assert_eq!(
        client::endpoint("https://gateway.example.test/prefix")
            .unwrap()
            .join("v1/capabilities")
            .unwrap()
            .path(),
        "/prefix/v1/capabilities"
    );
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires VIPTV_TEST_GATEWAY_BINARY and qualified real FFmpeg/ffprobe"]
async fn registration_interoperates_with_the_independent_gateway_service() {
    use std::time::Duration;
    let binary = std::env::var("VIPTV_TEST_GATEWAY_BINARY").unwrap();
    let ffmpeg = std::env::var("VIPTV_TEST_FFMPEG").unwrap();
    let ffprobe = std::env::var("VIPTV_TEST_FFPROBE").unwrap();
    let root = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let bootstrap = "backend-gateway-fixture-bootstrap-only";
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
            assert!(
                child.try_wait().unwrap().is_none(),
                "gateway fixture exited before listening"
            );
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
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let issued:Value=network.post(format!("http://{address}/v1/keys")).bearer_auth(bootstrap)
        .json(&json!({"label":"Backend fixture","namespaces":["fixture"],"scopes":["capabilities","create","read","renew","release"],"quotas":{"inputs":1,"outputs":1,"viewers":5},"expires_at":null}))
        .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    let mut app = fixture();
    app.secret_vault = Some(Arc::new(vault(7)));
    app.gateway_client = client::Client::fixture(format!("http://{address}/").parse().unwrap());
    let mut body = registration();
    body["integration_key"] = issued["secret"].clone();
    let (status, registered) =
        request(&app, "member-token-1", "POST", "/api/v2/gateways", body).await;
    assert_eq!(status, StatusCode::OK, "{registered}");
    assert!(!registered
        .to_string()
        .contains(issued["secret"].as_str().unwrap()));
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            &format!(
                "/api/v2/gateways/{}/check",
                registered["id"].as_str().unwrap()
            ),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(tokio::process::Command::new("kill")
        .args(["-TERM", &child.id().unwrap().to_string()])
        .status()
        .await
        .unwrap()
        .success());
    assert!(tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .unwrap()
        .unwrap()
        .success());
}
