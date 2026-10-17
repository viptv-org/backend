use super::*;

#[tokio::test]
async fn required_source_audio_is_negotiated_for_direct_and_enforced_by_gateway() {
    for (supports, explicit, old_gateway) in [
        (true, None, false),
        (false, None, false),
        (true, Some(3), false),
        (false, None, true),
        (false, Some(3), true),
    ] {
        let (app, peer) = setup().await;
        let source = source(&app);
        app.streams
            .lock()
            .unwrap()
            .get_mut(&source)
            .unwrap()
            .required_audio_index = Some(1);
        gateway(&app, "first", 0);
        peer.mode
            .store(if old_gateway { 11 } else { 9 }, Ordering::SeqCst);
        let mut input = body(&source, "required_audio", "android");
        input["client"]["supports_required_audio_index"] = json!(supports);
        if let Some(index) = explicit {
            input["audio_track"] = json!(index);
        }
        let (status, start) = request(
            &app,
            "member-token-1",
            "POST",
            "/api/playback",
            input.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{start}");
        let ready = settled(&app, start["id"].as_str().unwrap()).await;
        if old_gateway && explicit.is_none() {
            assert_eq!(ready["status"], "failed");
            assert_eq!(ready["error_code"], "delivery_unsupported");
            assert!(
                peer.outputs.lock().unwrap().is_empty(),
                "old gateway must not silently drop the requirement"
            );
        } else if supports && explicit.is_none() {
            assert_eq!(ready["delivery"]["kind"], "direct");
            assert_eq!(ready["delivery"]["required_audio_index"], 1);
            assert!(peer.outputs.lock().unwrap().is_empty());
            let mut changed = input.clone();
            changed["client"]["supports_required_audio_index"] = json!(false);
            assert_eq!(request(&app, "member-token-1", "POST", "/api/playback", changed).await.0,
                StatusCode::CONFLICT, "negotiation remains bound to the full admission hash");
        } else {
            assert_eq!(ready["delivery"]["kind"], "gateway", "{ready}");
            assert!(
                ready["delivery"].get("required_audio_index").is_none(),
                "remux output already applied the selection"
            );
            let outputs = peer.outputs.lock().unwrap();
            assert_eq!(outputs[0]["protocol"], "hls");
            if let Some(index) = explicit {
                assert_eq!(outputs[0]["audio_track"], index);
                assert!(
                    outputs[0].get("required_audio_index").is_none(),
                    "manual absolute input index wins"
                );
            } else {
                assert_eq!(outputs[0]["required_audio_index"], 1);
                assert!(outputs[0]["audio_track"].is_null());
            }
        }
        input["required_audio_index"] = json!(2);
        assert_eq!(
            request(&app, "member-token-1", "POST", "/api/playback", input)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
}
