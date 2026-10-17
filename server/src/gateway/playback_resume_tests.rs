use super::*;

#[tokio::test]
async fn canonical_resume_is_opt_in_explicit_position_wins_and_retries_keep_admission_snapshot() {
    let app = fixture();
    let source = source(&app);
    {
        let mut streams = app.streams.lock().unwrap();
        let entry = streams.get_mut(&source).unwrap();
        entry.media_id = Some("opaque-original-episode".into());
        entry.kind = "series".into();
    }
    app.db.lock().unwrap().execute("INSERT INTO progress(profile_id,id,type,name,position,duration,updated_at) VALUES(1,'opaque-original-episode','series','Episode',12.5,100,1),(1,'mapped-imdb-episode','series','Other',80,100,1)",[]).unwrap();
    let mut resume = body(&source, "canonical_resume", "android");
    resume["resume"] = json!(true);
    let (status, started) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/playback",
        resume.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{started}");
    let id = started["id"].as_str().unwrap();
    let first = settled(&app, id).await;
    assert_eq!(first["delivery"]["position"], 12.5);
    app.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE progress SET position=20 WHERE id='opaque-original-episode'",
            [],
        )
        .unwrap();
    let (status, repeated) =
        request(&app, "member-token-1", "POST", "/api/playback", resume).await;
    assert_eq!(status, StatusCode::OK, "{repeated}");
    assert_eq!(repeated["id"], id);
    assert_eq!(repeated["delivery"]["position"], 12.5);
    for (attempt, resume, position, expected) in [
        ("legacy_zero", false, 0.0, 0.0),
        ("explicit_seek", true, 4.25, 4.25),
        ("fresh_resume", true, 0.0, 20.0),
    ] {
        let mut body = body(&source, attempt, "android");
        body["resume"] = json!(resume);
        body["position"] = json!(position);
        let (status, started) =
            request(&app, "member-token-1", "POST", "/api/playback", body).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{started}");
        assert_eq!(
            settled(&app, started["id"].as_str().unwrap()).await["delivery"]["position"],
            expected
        );
    }
    let mut forged = body(&source, "client_forged_identity", "android");
    forged["resume"] = json!(true);
    forged["media_id"] = json!("mapped-imdb-episode");
    assert_eq!(
        request(&app, "member-token-1", "POST", "/api/playback", forged)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn unsafe_direct_probe_falls_back_to_registered_gateway_or_fails_without_raw_delivery() {
    let (mut app, peer) = setup().await;
    app.direct_probe = Arc::new(super::super::direct_probe::Probe::default());
    let source = source(&app);
    let (_, start) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/playback",
        body(&source, "unsafe_without_gateway", "android"),
    )
    .await;
    let failed = settled(&app, start["id"].as_str().unwrap()).await;
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["error_code"], "gateway_required");
    assert!(failed["delivery"].is_null());
    assert!(peer.inputs.lock().unwrap().is_empty());
    gateway(&app, "first", 0);
    let (_, start) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/playback",
        body(&source, "unsafe_with_gateway", "android"),
    )
    .await;
    let ready = settled(&app, start["id"].as_str().unwrap()).await;
    assert_eq!(ready["delivery"]["kind"], "gateway");
    assert_eq!(peer.inputs.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn gateway_resume_reports_applied_source_clock_and_renew_retains_delivery() {
    let (app, peer) = setup().await;
    gateway(&app, "first", 0);
    let source = source(&app);
    let mut body = body(&source, "gateway_source_clock", "android");
    body["position"] = json!(48.6);
    body["force_gateway"] = json!(true);
    let (status, created) = request(&app, "member-token-1", "POST", "/api/playback", body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{created}");
    let id = created["id"].as_str().unwrap();
    let ready = settled(&app, id).await;
    assert_eq!(ready["delivery"]["kind"], "gateway");
    assert_eq!(ready["delivery"]["position"], 0.0);
    assert_eq!(ready["delivery"]["timeline_offset_seconds"], 48.6);
    assert_eq!(ready["delivery"]["duration"], 600.0);
    assert_eq!(peer.inputs.lock().unwrap().len(), 1);
    let (_, renewed) = request(
        &app,
        "member-token-1",
        "POST",
        &format!("/api/playback/{id}/heartbeat"),
        json!({}),
    )
    .await;
    assert_eq!(renewed["delivery"], ready["delivery"]);
}

#[tokio::test]
async fn native_mp4_is_capability_negotiated_and_preserves_one_native_seek() {
    let (app,peer)=setup().await;gateway(&app,"first",0);peer.mode.store(9,Ordering::SeqCst);
    let source=source(&app);
    for (index,platform,expected) in [(0,"android","mp4"),(1,"android_tv","mp4"),(2,"desktop","mp4"),(3,"web","hls"),(4,"roku","hls")] {
        let mut input=body(&source,&format!("native_mp4_{index}"),platform);input["force_gateway"]=json!(true);input["position"]=json!(48.6);
        let (_,created)=request(&app,"member-token-1","POST","/api/playback",input).await;
        let ready=settled(&app,created["id"].as_str().unwrap()).await;
        assert_eq!(ready["delivery"]["format"],expected,"{ready}");
        assert_eq!(ready["delivery"]["position"],if expected=="mp4"{48.6}else{0.0});
        assert_eq!(ready["delivery"]["timeline_offset_seconds"],if expected=="mp4"{0.0}else{48.6});
        assert_eq!(peer.outputs.lock().unwrap()[index]["protocol"],if expected=="mp4"{"native_mp4"}else{"hls"});
    }
}
