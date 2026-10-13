use super::*;

#[tokio::test]
async fn metadata_and_tracking_require_exact_identity_and_current_profile_link() {
    let (app, task, writes) = server().await;
    link(&app).await;
    let (status, body) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v3/profiles/1/simkl/search?category=movie&query=Movie",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["cards"][0]["identity"]["id"], "simkl:movie:10");
    let (status, body) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v3/profiles/1/simkl/meta/tv/20",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["episodes"][0]["identity"]["episode"]["episode"], 2);
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/v3/profiles/1/simkl/meta/movie/11",
            json!({})
        )
        .await
        .0,
        StatusCode::BAD_GATEWAY
    );
    for input in [
        json!({"category":"tv","id":20}),
        json!({"category":"tv","id":20,"season":1,"episode":1}),
    ] {
        assert_eq!(
            request(
                &app,
                "member-token-1",
                "POST",
                "/api/v3/profiles/1/simkl/watched",
                input
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(writes.load(Ordering::SeqCst), 0);
    let (status, body) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v3/profiles/1/simkl/watched",
        json!({"category":"tv","id":20,"season":1,"episode":2}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["recorded"], true);
    assert_eq!(writes.load(Ordering::SeqCst), 1);
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
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/v3/profiles/1/simkl/watched",
            json!({"category":"movie","id":10})
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    task.abort();
}

#[tokio::test]
async fn unlink_invalidates_pending_oauth_and_disabled_assignment_blocks_tracking() {
    let (app, task, writes) = server().await;
    link(&app).await;
    let state = start(&app).await;
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
    assert!(callback(
        State(app.clone()),
        Query(crate::simkl::oauth::Callback {
            state,
            code: Some("fixture-code".into()),
            error: None
        })
    )
    .await
    .is_err());
    link(&app).await;
    let (status, body) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v3/profiles/1/integrations",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["connections"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["id"] == "simkl:1"));
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/v3/profiles/1/integrations",
            json!({"revision":0,"browsing_plugin":"stremio","assignments":[]})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/v3/profiles/1/simkl/watched",
            json!({"category":"movie","id":10})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(writes.load(Ordering::SeqCst), 0);
    task.abort();
}
