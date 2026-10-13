use super::*;

#[tokio::test]
async fn oauth_is_single_use_encrypted_and_isolated() {
    let (app, task, _) = server().await;
    let state = link(&app).await;
    let repeated = callback(
        State(app.clone()),
        Query(crate::simkl::oauth::Callback {
            state,
            code: Some("fixture-code".into()),
            error: None,
            iss: Some("https://simkl.com".into()),
        }),
    )
    .await;
    assert!(repeated.is_err());
    let db = app.db.lock().unwrap();
    let sealed: String = db
        .query_row(
            "SELECT credential FROM simkl_links WHERE profile_id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!sealed.contains("fixture-private-token"));
    assert!(app
        .secret_vault
        .as_ref()
        .unwrap()
        .open(2, "simkl", "1", &sealed)
        .is_err());
    assert!(app
        .secret_vault
        .as_ref()
        .unwrap()
        .open(1, "simkl", "2", &sealed)
        .is_err());
    drop(db);
    let (status, body) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v3/profiles/1/simkl",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["linked"], true);
    assert!(!body.to_string().contains("fixture-private-token"));
    assert_eq!(
        request(
            &app,
            "member-token-2",
            "GET",
            "/api/v3/profiles/1/simkl",
            json!({})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/v3/profiles/2/simkl",
            json!({})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    task.abort();
}

#[tokio::test]
async fn revoked_and_expired_authorizations_cannot_link() {
    let (app, task, _) = server().await;
    let state = start(&app).await;
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE simkl_oauth_states SET expires=0", [])
        .unwrap();
    assert!(callback(
        State(app.clone()),
        Query(crate::simkl::oauth::Callback {
            state,
            code: Some("fixture-code".into()),
            error: None,
            iss: Some("https://simkl.com".into())
        })
    )
    .await
    .is_err());
    let state = start(&app).await;
    app.db
        .lock()
        .unwrap()
        .execute("DELETE FROM auth_sessions WHERE id='s1'", [])
        .unwrap();
    assert!(callback(
        State(app.clone()),
        Query(crate::simkl::oauth::Callback {
            state,
            code: Some("fixture-code".into()),
            error: None,
            iss: Some("https://simkl.com".into())
        })
    )
    .await
    .is_err());
    let count: usize = app
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM simkl_links WHERE credential IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    task.abort();
}

#[tokio::test]
async fn unconfigured_service_advertises_no_simkl_and_cannot_authorize() {
    let app = fixture();
    let (_, status) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v3/profiles/1/simkl",
        json!({}),
    )
    .await;
    assert_eq!(status["configured"], false);
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/v3/profiles/1/simkl/authorize",
            json!({})
        )
        .await
        .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    let (_, body) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v3/bootstrap",
        json!({}),
    )
    .await;
    assert!(!body["integrations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["id"] == "simkl"));
}

#[tokio::test]
async fn paired_device_cannot_initiate_simkl_oauth() {
    let (app, task, _) = server().await;
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE auth_sessions SET kind='device' WHERE id='s1'", [])
        .unwrap();
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/v3/profiles/1/simkl/authorize",
            json!({})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    task.abort();
}
