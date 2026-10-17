use crate::{auth_integration_tests::fixture, test_support::request, *};

#[tokio::test]
async fn paired_profile_can_save_private_xtream_without_creating_account_shared_provider() {
    let app = fixture();
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE auth_sessions SET kind='device' WHERE id='s1'", [])
        .unwrap();
    let(status,body)=request(&app,"member-token-1","PUT","/api/profiles/1/source-integrations",json!({
        "revision":0,"plugin_id":"xtream","configuration":{"server_url":"https://provider.example","username":"fixture-user","password":"fixture-password"}
    })).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "profile-scoped Live config must not require account-wide provider management: {body}"
    );
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM providers", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        0
    );
}

pub(super) async fn setup() -> (App, String) {
    let app = fixture();
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE auth_sessions SET kind='device' WHERE id='s1'", [])
        .unwrap();
    let(status,_)=request(&app,"member-token-1","PUT","/api/profiles/1/source-integrations",json!({"revision":0,"plugin_id":"xtream","configuration":{"server_url":"https://provider.example","username":"fixture-user","password":"fixture-password"}})).await;
    assert_eq!(status, StatusCode::OK);
    let (status, snapshot) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/source-integrations",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let connection = snapshot["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["plugin_id"] == "xtream")
        .unwrap();
    assert_eq!(connection["private_profile_id"], "1");
    (app, connection["id"].as_str().unwrap().into())
}
#[tokio::test]
async fn private_xtream_updates_one_encrypted_connection_and_preserves_metadata_owner() {
    let (app, connection) = setup().await;
    let update = |revision, password| json!({"revision":revision,"plugin_id":"xtream","configuration":{"server_url":"https://provider.example","username":"fixture-user","password":password}});
    {
        let db = app.db.lock().unwrap();
        let secret: String = db
            .query_row("SELECT secret FROM engine_xtream_connections", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(
            !secret.contains("fixture-password")
                && !secret.contains("fixture-user")
                && !secret.contains("provider.example")
        );
    }
    let (status, saved) = request(
        &app,
        "member-token-1",
        "PUT",
        "/api/profiles/1/source-integrations",
        update(1, "replacement-secret"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["configuration"]["browsing_plugin"], "stremio");
    assert_eq!(
        saved["configuration"]["assignments"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        saved["configuration"]["assignments"][0]["connection_id"],
        connection
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/profiles/1/source-integrations",
            update(1, "stale-secret")
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(
            &app,
            "member-token-2",
            "GET",
            "/api/profiles/1/source-integrations",
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let db = app.db.lock().unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM engine_xtream_connections", [], |r| {
            r.get::<_, u64>(0)
        })
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row("SELECT revision FROM engine_xtream_connections", [], |r| {
            r.get::<_, u64>(0)
        })
        .unwrap(),
        2
    );
}

#[tokio::test]
async fn paired_profile_removal_cannot_delete_a_legacy_account_shared_provider() {
    let (app, _) = setup().await;
    {
        let db = app.db.lock().unwrap();
        db.execute("INSERT INTO providers(id,name,url,username,password) VALUES(10,'Shared','https://provider.example','fixture','secret')",[]).unwrap();
        db.execute("INSERT INTO provider_ownership VALUES(10,1)", [])
            .unwrap();
    }
    let (status, _) = request(
        &app,
        "member-token-1",
        "DELETE",
        "/api/profiles/1/source-integrations/iptv:10?revision=1",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM providers WHERE id=10", [], |row| row
                .get::<_, u64>(
                0
            ))
            .unwrap(),
        1
    );
}
