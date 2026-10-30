use super::*;

async fn admitted(app: &App, request_id: &str) -> String {
    let source = source(app);
    {
        let mut streams = app.streams.lock().unwrap();
        let entry = streams.get_mut(&source).unwrap();
        entry.media_id = Some("fixture-series-episode".into());
        entry.kind = "series".into();
        entry.exact_vod = Some(crate::app_state::ExactVod {
            title: "Fixture show".into(),
            series: Some("Fixture show".into()),
            season: Some(1),
            episode: Some(2),
        });
    }
    let (_, admission) = request(
        app,
        "member-token-1",
        "POST",
        "/api/playback",
        body(&source, request_id, "roku"),
    )
    .await;
    let ready = settled(
        app,
        admission["id"]
            .as_str()
            .unwrap_or_else(|| panic!("admission: {admission}")),
    )
    .await;
    ready["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn segments_answer_nothing_for_a_playback_with_no_anime_identity() {
    let (app, _peer) = setup().await;
    gateway(&app, "first", 0);
    let id = admitted(&app, "segments-none").await;
    let (status, value) = request(
        &app,
        "member-token-1",
        "GET",
        &format!("/api/playback/{id}/segments"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value, json!({"segments":[]}));
}

#[tokio::test]
async fn segments_require_an_active_authorized_playback() {
    let (app, _peer) = setup().await;
    gateway(&app, "first", 0);
    let id = admitted(&app, "segments-lease").await;
    let (status, value) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/playback/fixture-unknown/segments",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(value["error_code"], "playback_not_found");
    app.gateway_playbacks.lapse_lease(&id);
    let (status, value) = request(
        &app,
        "member-token-1",
        "GET",
        &format!("/api/playback/{id}/segments"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::GONE);
    assert_eq!(value["error_code"], "playback_expired");
}
