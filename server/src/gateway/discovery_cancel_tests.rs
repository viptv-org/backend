use super::*;
#[tokio::test]
async fn cancelled_discovery_preserves_stremio_and_sdk_grants_through_playback_admission() {
    let mut producers = vec!["stremio"];
    if cfg!(feature = "vsources-runtime") {
        producers.push("vsources");
    }
    for producer in producers {
        let (app, peer) = setup().await;
        gateway(&app, "first", 1);
        let addon_grant = source(&app);
        let owned = app.clone().with_lease(lease());
        let (grant, source, revision) = if producer == "vsources" {
            let(status,configured)=request(&app,"member-token-1","PUT","/api/profiles/1/source-integrations",json!({"revision":0,"plugin_id":"vsources","configuration":{"tmdb_api_key":"isolated-fixture-key"}})).await;
            assert_eq!(status, StatusCode::OK, "{configured}");
            let source = configured["configuration"]["assignments"]
                .as_array()
                .unwrap()
                .iter()
                .find_map(|a| {
                    a["connection_id"]
                        .as_str()
                        .filter(|id| id.starts_with("vsources:"))
                })
                .unwrap()
                .to_owned();
            let (cards, error) = owned.register(
                &source,
                vec![json!({"url":"http://source.fixture.invalid/movie.mp4","name":"SDK fixture"})],
                "movie",
            );
            assert!(error.is_none());
            (
                cards[0]["id"].as_str().unwrap().to_owned(),
                source,
                Some((1, 1)),
            )
        } else {
            (addon_grant, "addon:1".into(), None)
        };
        let job_id = format!("cancel-{producer}");
        owned.own_resource("job", &job_id);
        let job = Arc::new(crate::app_state::Job {metadata_proof:None,
            source_slots: std::sync::atomic::AtomicUsize::new(1),
            cancelled: std::sync::atomic::AtomicBool::new(false),
            configuration_revision: revision,
            media_id: Some("opaque-movie".into()),
            exact_vod: None,
            kind: "movie".into(),
            created: std::time::Instant::now(),
            state: Mutex::new(crate::app_state::JobState {
                events: vec![json!({"seq":1,"source":source,"streams":[{"id":grant}]})],
                pending: 1,
            }),
            notify: tokio::sync::Notify::new(),
        });
        app.jobs.lock().unwrap().insert(job_id.clone(), job);
        assert_eq!(
            request(
                &app,
                "member-token-2",
                "DELETE",
                &format!("/api/streams/{job_id}"),
                Value::Null
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        for _ in 0..2 {
            assert_eq!(
                request(
                    &app,
                    "member-token-1",
                    "DELETE",
                    &format!("/api/streams/{job_id}"),
                    Value::Null
                )
                .await
                .0,
                StatusCode::OK
            );
        }
        let (status, start) = request(
            &app,
            "member-token-1",
            "POST",
            "/api/playback",
            body(&grant, &format!("admit-{producer}"), "roku"),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{start}");
        let playback = start["id"].as_str().unwrap();
        let ready = settled(&app, playback).await;
        assert_eq!(ready["status"], "ready", "{ready}");
        assert_eq!(ready["delivery"]["kind"], "gateway");
        assert_eq!(peer.inputs.lock().unwrap().len(), 1);
        assert_eq!(
            request(
                &app,
                "member-token-1",
                "DELETE",
                &format!("/api/streams/{job_id}"),
                Value::Null
            )
            .await
            .0,
            StatusCode::OK
        );
        assert_eq!(
            request(
                &app,
                "member-token-1",
                "GET",
                &format!("/api/playback/{playback}"),
                Value::Null
            )
            .await
            .1["status"],
            "ready"
        );
        assert_eq!(
            request(
                &app,
                "member-token-1",
                "DELETE",
                &format!("/api/playback/{playback}"),
                Value::Null
            )
            .await
            .0,
            StatusCode::OK
        );
    }
}
