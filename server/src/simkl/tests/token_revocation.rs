use super::*;

fn scoped(app: &App) -> App {
    app.clone().with_lease(ResourceLease {
        policy_revision: 0,
        session_id: Some("s1".into()),
        principal: auth::Principal::Account {
            account_id: 1,
            role: "member".into(),
            profile_id: Some(1),
            session_id: Some("s1".into()),
        },
    })
}

#[tokio::test]
async fn unlink_while_refresh_waits_for_owner_prevents_dispatch() {
    let (app, task, _) = server().await;
    link(&app).await;
    tokens::expired(&app);
    let request_app = scoped(&app);
    let credential = store::credential(&request_app, 1, Capability::Search).unwrap();
    let guard = app.simkl.refresh_gate.lock().await;
    let mut pending = Box::pin(crate::simkl::tokens::access(&request_app, 1, &credential));
    assert!(futures::poll!(pending.as_mut()).is_pending());
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "DELETE",
            "/api/v3/profiles/1/simkl",
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    drop(guard);
    let error = pending.await.err().unwrap();
    assert_eq!(error.0, StatusCode::CONFLICT);
    assert_eq!(error.1, "Simkl link changed");
    task.abort();
}

#[tokio::test]
async fn unlink_during_refresh_does_not_restore_sealed_credentials() {
    let (mut app, initial, _) = server().await;
    link(&app).await;
    tokens::expired(&app);
    initial.abort();
    let entered = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    let start = entered.clone();
    let finish = resume.clone();
    let routes = axum::Router::new().route("/oauth2/token",post(move || {
        let start=start.clone(); let finish=finish.clone(); async move {
            start.notify_one(); finish.notified().await;
            axum::Json(json!({"access_token":"late-access","refresh_token":"refresh-private","token_type":"Bearer","expires_in":604800,"scope":"media:read media:write"}))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    app.simkl = Arc::new(Service::fixture(&format!(
        "http://{}",
        listener.local_addr().unwrap()
    )));
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    let request_app = scoped(&app);
    let credential = store::credential(&request_app, 1, Capability::Search).unwrap();
    let pending =
        tokio::spawn(
            async move { crate::simkl::tokens::access(&request_app, 1, &credential).await },
        );
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "DELETE",
            "/api/v3/profiles/1/simkl",
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    resume.notify_one();
    assert_eq!(
        pending.await.unwrap().err().unwrap().0,
        StatusCode::CONFLICT
    );
    let credential: Option<String> = app
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT credential FROM simkl_links WHERE profile_id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(credential.is_none());
    task.abort();
}

#[test]
fn insufficient_or_malformed_token_grants_are_never_saved() {
    let valid = json!({"access_token":"access","refresh_token":"refresh","token_type":"Bearer","expires_in":604800,"scope":"media:read media:write"});
    assert!(crate::simkl::tokens::document(&valid).is_ok());
    for (key, value) in [
        ("scope", json!("media:read")),
        ("scope", json!("media:read media:write:fake")),
        ("expires_in", json!(0)),
        ("access_token", json!("bad\nvalue")),
        ("refresh_token", Value::Null),
    ] {
        let mut invalid = valid.clone();
        invalid[key] = value;
        assert!(crate::simkl::tokens::document(&invalid).is_err(), "{key}");
    }
}
