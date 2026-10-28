use super::xtream_projection_tests::cached;
use crate::{test_support::request, *};

#[tokio::test]
async fn private_xtream_cached_guide_revalidates_revision_after_waiting_for_directory() {
    let (app, connection, channel, _) = cached().await;
    let gate = app.live_fetch_gates.connection(&connection).unwrap();
    let held = gate.lock().await;
    let reader = app.clone();
    let task = tokio::spawn(async move {
        request(
            &reader,
            "member-token-1",
            "GET",
            &format!("/api/profiles/1/live/{channel}/guide"),
            Value::Null,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        while Arc::strong_count(&gate) < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let(status,_)=request(&app,"member-token-1","PUT","/api/profiles/1/source-integrations",json!({"revision":1,"plugin_id":"xtream","configuration":{"server_url":"https://replacement.example","username":"new-user","password":"new-secret"}})).await;
    assert_eq!(status, StatusCode::OK);
    drop(held);
    let (status, body) = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "queued cache read cannot publish after credentials/profile change: {body}"
    );
}
#[tokio::test]
async fn private_xtream_server_egress_rejects_loopback_before_sending_credentials() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let requests = Arc::new(AtomicUsize::new(0));
    let count = requests.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = Router::new().fallback(move |method: axum::http::Method| {
        let count = count.clone();
        async move {
            // Count backend egress only: the host probes every new loopback
            // listener with a safe `HEAD /` (backend#48).
            if method != axum::http::Method::HEAD {
                count.fetch_add(1, Ordering::SeqCst);
            }
            StatusCode::OK
        }
    });
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    // The dev host's engine probes every new loopback listener with `HEAD /`
    // (backend#48). Send the same probe so the fixture is checked against it.
    reqwest::Client::new()
        .head(format!("http://{address}/"))
        .send()
        .await
        .expect("HEAD / probe")
        .error_for_status()
        .expect("fixture answers the port probe");
    let app = crate::auth_integration_tests::fixture();
    let(status,_)=request(&app,"member-token-1","PUT","/api/profiles/1/source-integrations",json!({"revision":0,"plugin_id":"xtream","configuration":{"server_url":format!("http://{address}"),"username":"private-user","password":"private-secret"}})).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "configuration validation must remain separate from host egress policy"
    );
    let (status, body) = tokio::time::timeout(
        Duration::from_secs(3),
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/profiles/1/live/channels",
            Value::Null,
        ),
    )
    .await
    .unwrap();
    assert_ne!(status, StatusCode::OK);
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    assert!(
        !body.to_string().contains("private-user") && !body.to_string().contains("private-secret")
    );
    server.abort();
}
#[tokio::test]
async fn private_xtream_connections_and_cached_channels_stay_with_their_selected_profile() {
    for second_account in [1, 2] {
        let (app, first, channel, _) = cached().await;
        {
            let db = app.db.lock().unwrap();
            db.execute("INSERT INTO profiles(id,name,avatar_seed,presentation_complete) VALUES(2,'Second','second',1)",[]).unwrap();
            db.execute(
                "INSERT INTO profile_owners(profile_id,account_id,created_at) VALUES(2,?1,0)",
                [second_account],
            )
            .unwrap();
            db.execute(
                "UPDATE auth_sessions SET account_id=?1,profile_id=2,kind='device' WHERE id='s2'",
                [second_account],
            )
            .unwrap();
        }
        let(status,_)=request(&app,"member-token-2","PUT","/api/profiles/2/source-integrations",json!({"revision":0,"plugin_id":"xtream","configuration":{"server_url":"https://second.example","username":"second-user","password":"second-secret"}})).await;
        assert_eq!(status, StatusCode::OK);
        let (_, snapshot) = request(
            &app,
            "member-token-2",
            "GET",
            "/api/profiles/2/source-integrations",
            Value::Null,
        )
        .await;
        assert!(
            snapshot["connections"]
                .as_array()
                .unwrap()
                .iter()
                .all(|entry| entry["id"] != first)
        );
        let (status, _) = request(
            &app,
            "member-token-2",
            "GET",
            &format!("/api/profiles/2/live/{channel}/guide"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = request(
            &app,
            "member-token-2",
            "DELETE",
            &format!("/api/profiles/2/source-integrations/{first}?revision=1"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(
            app.db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM engine_xtream_connections",
                    [],
                    |row| row.get::<_, u64>(0)
                )
                .unwrap(),
            2
        );
    }
}
