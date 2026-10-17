use super::*;

async fn mapping_fixture() -> (App, tokio::task::JoinHandle<()>, Arc<Mutex<Vec<String>>>) {
    let paths = Arc::new(Mutex::new(Vec::new()));
    let observed = paths.clone();
    let mock = Router::new().fallback(move |request: Request| {
        let observed = observed.clone();
        async move {
            let path = request.uri().path().to_owned();
            observed.lock().unwrap().push(path.clone());
            axum::Json(match path.as_str() {
                "/movies/10" => json!({"title":"Movie","ids":{"simkl":10,"imdb":"tt1234567"}}),
                "/movies/11" => json!({"title":"Unmapped","ids":{"simkl":11}}),
                "/tv/20" => json!({"title":"Show","ids":{"simkl":20,"imdb":"tt7654321"}}),
                "/tv/episodes/20" => json!([{"title":"Episode","season":2,"episode":4,"ids":{"simkl_id":30}}]),
                "/addon/stream/movie/tt1234567.json" | "/addon/stream/series/tt7654321:2:4.json" => json!({"streams":[{"name":"Mapped source","url":"https://example.com/video.mp4"}]}),
                "/addon/subtitles/movie/tt1234567.json" | "/addon/subtitles/series/tt7654321:2:4.json" => json!({"subtitles":[{"lang":"eng","url":"https://example.com/subtitle.vtt"}]}),
                _ => json!({"error":"unexpected fixture request"}),
            })
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let mut app = fixture();
    app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
    app.addons.allow_test_loopback = true;
    let manifest = json!({"id":"mapped","name":"Mapped addon","version":"1","resources":[{"name":"stream","types":["movie","series"],"idPrefixes":["tt"]},{"name":"subtitles","types":["movie","series"],"idPrefixes":["tt"]}],"types":["movie","series"]});
    app.db.lock().unwrap().execute("INSERT INTO addons(name,manifest_url,manifest,account_id) VALUES('Mapped addon',?1,?2,1)",params![format!("http://{address}/addon/manifest.json"),manifest.to_string()]).unwrap();
    crate::test_support::encrypt_fixture_sources(&app);
    (app, task, paths)
}

#[tokio::test]
async fn sources_and_subtitles_use_cached_authoritative_ids_and_preserve_canonical_playback_identity(
) {
    let (app, task, paths) = mapping_fixture().await;
    for (kind, id, expected) in [
        ("movie", "simkl:movie:10", "tt1234567"),
        ("series", "simkl:tv:20:2:4", "tt7654321:2:4"),
    ] {
        let title_id = if kind == "movie" { id } else { "simkl:tv:20" };
        assert_eq!(
            request(
                &app,
                "member-token-1",
                "GET",
                &format!("/api/meta/{kind}/{title_id}"),
                json!({})
            )
            .await
            .0,
            StatusCode::OK
        );
        let (status, start) = request(
            &app,
            "member-token-1",
            "POST",
            "/api/streams",
            json!({"type":kind,"id":id,"only_addons":true,"imdb_id":"tt9999999","tmdb_id":"999"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{start}");
        let job_id = start["id"].as_str().unwrap();
        let finished = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let (_, poll) = request(
                    &app,
                    "member-token-1",
                    "GET",
                    &format!("/api/streams/{job_id}"),
                    json!({}),
                )
                .await;
                if poll["done"] == true {
                    break poll;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            finished["events"][0]["streams"].as_array().unwrap().len(),
            1,
            "{finished}"
        );
        let jobs = app.jobs.lock().unwrap();
        let exact = jobs[job_id].exact_vod.as_ref().unwrap();
        assert_eq!(exact.title, id);
        if kind == "series" {
            assert_eq!(exact.series.as_deref(), Some("simkl:tv:20"));
            assert_eq!(exact.season, Some(2));
            assert_eq!(exact.episode, Some(4));
        }
        assert_eq!(jobs[job_id].media_id.as_deref(), Some(id));
        drop(jobs);
        let stream = finished["events"][0]["streams"][0]["id"].as_str().unwrap();
        assert_eq!(app.streams.lock().unwrap()[stream].media_id.as_deref(), Some(id));
        let (status, subs) = request(
            &app,
            "member-token-1",
            "GET",
            &format!("/api/subtitles/{kind}/{id}"),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{subs}");
        assert_eq!(subs["candidates"].as_array().unwrap().len(), 1, "{subs}");
        let observed = paths.lock().unwrap();
        assert!(observed.contains(&format!("/addon/stream/{kind}/{expected}.json")));
        assert!(observed.contains(&format!("/addon/subtitles/{kind}/{expected}.json")));
        assert!(!observed.iter().any(|p| p.contains("simkl:")));
    }
    let observed = paths.lock().unwrap();
    for path in ["/movies/10", "/tv/20", "/tv/episodes/20"] {
        assert_eq!(
            observed.iter().filter(|p| p.as_str() == path).count(),
            1,
            "metadata is shared by details, sources and subtitles"
        );
    }
    drop(observed);
    task.abort();
}

#[tokio::test]
async fn unresolved_or_mismatched_episode_mapping_returns_explicit_unavailable_without_addon_request(
) {
    let (app, task, paths) = mapping_fixture().await;
    for input in [
        json!({"type":"movie","id":"simkl:movie:11"}),
        json!({"type":"series","id":"simkl:tv:20:2:9"}),
        json!({"type":"series","id":"simkl:tv:20:2:4","season":1,"episode":4}),
    ] {
        let (status, body) =
            request(&app, "member-token-1", "POST", "/api/streams", input).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["error_code"], "metadata_mapping_unavailable");
    }
    let (status, body) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/subtitles/movie/simkl:movie:11",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error_code"], "metadata_mapping_unavailable");
    assert!(!paths
        .lock()
        .unwrap()
        .iter()
        .any(|p| p.starts_with("/addon/")));
    task.abort();
}
