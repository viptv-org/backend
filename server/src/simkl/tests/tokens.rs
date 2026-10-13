use super::*;

pub(super) fn expired(app: &App) {
    let value = json!({"access_token":"expired-access","refresh_token":"refresh-private","scope":"media:read media:write","expires_at":1});
    let sealed = app
        .secret_vault
        .as_ref()
        .unwrap()
        .seal(1, "simkl", "1", value.to_string().as_bytes())
        .unwrap();
    app.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE simkl_links SET credential=?1 WHERE profile_id=1",
            [sealed],
        )
        .unwrap();
}

#[tokio::test]
async fn expired_profile_token_refresh_is_coalesced_sealed_and_preserves_link_revision() {
    let (mut app, initial, _) = server().await;
    link(&app).await;
    initial.abort();
    expired(&app);
    let count = Arc::new(AtomicUsize::new(0));
    let called = count.clone();
    let routes=axum::Router::new().route("/oauth2/token",post(move|axum::Form(body):axum::Form<HashMap<String,String>>|{let called=called.clone();async move{
        assert_eq!(body["grant_type"],"refresh_token");assert_eq!(body["refresh_token"],"refresh-private");called.fetch_add(1,Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(20)).await;
        axum::Json(json!({"access_token":"fresh-access","refresh_token":"fresh-refresh","token_type":"Bearer","expires_in":604800,"scope":"media:read media:write"}))
    }})).route("/search/movie",get(|headers:axum::http::HeaderMap|async move{
        if headers.get(header::AUTHORIZATION).and_then(|v|v.to_str().ok())!=Some("Bearer fresh-access"){return StatusCode::UNAUTHORIZED.into_response();}
        axum::Json(json!([{"title":"Movie","ids":{"simkl":10}}])).into_response()
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
    let revision: i64 = app
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT revision FROM simkl_links WHERE profile_id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let requests = (0..6).map(|_| {
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/v3/profiles/1/simkl/search?category=movie&query=Movie",
            json!({}),
        )
    });
    for (status, _) in futures::future::join_all(requests).await {
        assert_eq!(status, StatusCode::OK);
    }
    assert_eq!(count.load(Ordering::SeqCst), 1);
    let (current, sealed): (i64, String) = app
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT revision,credential FROM simkl_links WHERE profile_id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(revision, current);
    assert!(!sealed.contains("fresh-access"));
    assert!(!sealed.contains("fresh-refresh"));
    let clear = app
        .secret_vault
        .as_ref()
        .unwrap()
        .open(1, "simkl", "1", &sealed)
        .unwrap();
    let stored: Value = serde_json::from_slice(clear.expose()).unwrap();
    assert_eq!(stored["refresh_token"], "fresh-refresh");
    assert!(stored["expires_at"].as_i64().unwrap() > util::now());
    task.abort();
}

#[tokio::test]
async fn wrong_oauth_issuer_does_not_consume_valid_state_or_exchange_code() {
    let (app, task, _) = server().await;
    let state = start(&app).await;
    assert!(callback(
        State(app.clone()),
        Query(crate::simkl::oauth::Callback {
            state: state.clone(),
            code: Some("fixture-code".into()),
            error: None,
            iss: Some("https://simkl.com.attacker.invalid".into())
        })
    )
    .await
    .is_err());
    assert!(callback(
        State(app),
        Query(crate::simkl::oauth::Callback {
            state,
            code: Some("fixture-code".into()),
            error: None,
            iss: Some("https://simkl.com".into())
        })
    )
    .await
    .is_ok());
    task.abort();
}
