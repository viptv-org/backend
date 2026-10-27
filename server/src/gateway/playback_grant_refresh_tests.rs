//! A source grant stays reopenable while playback keeps using it (backend#18).
use super::*;
use std::time::Instant;

const PAST_GRANT_WINDOW: Duration = Duration::from_secs(31 * 60);

/// Ages the discovery grant and its ownership record as if discovery ran
/// 31 minutes ago.
fn age_grant(app: &App, stream: &str) {
    let discovered = Instant::now().checked_sub(PAST_GRANT_WINDOW).unwrap();
    app.streams.lock().unwrap().get_mut(stream).unwrap().created = discovered;
    app.resource_owners
        .lock()
        .unwrap()
        .get_mut(&format!("stream:{stream}"))
        .unwrap()
        .created = discovered;
}

/// Lapses the playback lease the way a long network drop would.
async fn lapse(app: &App, id: &str) {
    app.gateway_playbacks.lapse_lease(id);
    let (status, response) = request(
        app,
        "member-token-1",
        "POST",
        &format!("/api/playback/{id}/heartbeat"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::GONE, "{response}");
    assert_eq!(response["error_code"], "playback_expired");
}

async fn admit(app: &App, stream: &str, request_id: &str, platform: &str) -> (StatusCode, Value) {
    request(
        app,
        "member-token-1",
        "POST",
        "/api/playback",
        body(stream, request_id, platform),
    )
    .await
}

#[tokio::test]
async fn heartbeat_keeps_an_old_grant_reopenable_after_the_lease_lapses() {
    for platform in ["android", "roku"] {
        let (app, _peer) = setup().await;
        gateway(&app, "first", 1);
        let stream = source(&app);
        let (status, start) = admit(&app, &stream, "watching", platform).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{start}");
        let id = start["id"].as_str().unwrap().to_owned();
        assert_eq!(settled(&app, &id).await["status"], "ready");

        // 31 minutes into the episode the heartbeat is still alive.
        age_grant(&app, &stream);
        let (status, response) = request(
            &app,
            "member-token-1",
            "POST",
            &format!("/api/playback/{id}/heartbeat"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{response}");

        lapse(&app, &id).await;
        app.prune();
        let (status, reopened) = admit(&app, &stream, "reopen", platform).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{platform}: {reopened}");
        assert_eq!(
            settled(&app, reopened["id"].as_str().unwrap()).await["status"],
            "ready"
        );
    }
}

#[tokio::test]
async fn first_frame_keeps_an_old_grant_reopenable_after_the_lease_lapses() {
    let (app, _peer) = setup().await;
    gateway(&app, "first", 1);
    let stream = source(&app);
    let (_, start) = admit(&app, &stream, "watching", "roku").await;
    let id = start["id"].as_str().unwrap().to_owned();
    assert_eq!(settled(&app, &id).await["status"], "ready");

    age_grant(&app, &stream);
    let (status, response) = request(
        &app,
        "member-token-1",
        "POST",
        &format!("/api/playback/{id}/first-frame"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");

    lapse(&app, &id).await;
    app.prune();
    let (status, reopened) = admit(&app, &stream, "reopen", "roku").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{reopened}");
}

#[tokio::test]
async fn heartbeat_never_extends_a_grant_bound_to_an_upstream_lifetime() {
    let (app, _peer) = setup().await;
    gateway(&app, "first", 1);
    let stream = source(&app);
    let (_, start) = admit(&app, &stream, "watching", "android").await;
    let id = start["id"].as_str().unwrap().to_owned();
    assert_eq!(settled(&app, &id).await["status"], "ready");
    let resolved = Instant::now()
        .checked_sub(Duration::from_secs(25 * 60))
        .unwrap();
    {
        let mut streams = app.streams.lock().unwrap();
        let grant = streams.get_mut(&stream).unwrap();
        grant.fixed_lifetime = true;
        grant.created = resolved;
    }
    let (status, response) = request(
        &app,
        "member-token-1",
        "POST",
        &format!("/api/playback/{id}/heartbeat"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(app.streams.lock().unwrap()[&stream].created, resolved);
}

#[tokio::test]
async fn without_playback_an_old_grant_still_expires() {
    let (app, _peer) = setup().await;
    gateway(&app, "first", 1);
    let stream = source(&app);
    age_grant(&app, &stream);
    let (status, response) = admit(&app, &stream, "late", "android").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{response}");
    assert_eq!(response["error_code"], "source_not_found");
}
