use super::*;
use std::os::unix::fs::PermissionsExt;
fn fixture() -> (App, App, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let probe = dir.path().join("probe");
    let engine = dir.path().join("engine");
    std::fs::write(&probe,r#"#!/bin/sh
if [ -f "$0.hold" ]; then touch "$0.started"; while [ -f "$0.hold" ]; do sleep 0.02; done; fi
printf '%s' '{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":1280,"height":720,"pix_fmt":"yuv420p","level":31,"avg_frame_rate":"30/1"},{"index":1,"codec_type":"audio","codec_name":"aac","channels":2,"tags":{"language":"eng"}}],"format":{"duration":"600"}}'
"#).unwrap();
    std::fs::write(&engine,r#"#!/bin/sh
if [ "$1" = '-version' ]; then exit 0; fi
for output do :; done
directory=${output%/*}
printf fixture > "$directory/segment-000000000.ts"
printf '#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:1,\nsegment-000000000.ts\n' > "$output"
exec sleep 60
"#).unwrap();
    for file in [&probe, &engine] {
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let app = App::new(
        Connection::open_in_memory().unwrap(),
        reqwest::Client::new(),
        PlaybackManager::new(crate::playback::Config {
            ffmpeg: engine,
            ffprobe: probe,
            root: dir.path().join("hls"),
            max_sessions: 3,
            ttl: Duration::from_secs(30),
        }),
    )
    .unwrap();
    {
        let db = app.db.lock().unwrap();
        db.execute_batch("INSERT INTO auth_accounts(id,username,password_hash,role,recovery_hash,created_at) VALUES(1,'fixture','unused','owner','unused',0); INSERT INTO profiles(id,name,avatar_seed,presentation_complete) VALUES(1,'Family','fixture',1); INSERT INTO profile_owners(profile_id,account_id,created_at) VALUES(1,1,0); INSERT INTO auth_profiles VALUES(1,1);").unwrap();
        for id in ["one", "two"] {
            db.execute("INSERT INTO auth_sessions(id,account_id,access_hash,refresh_hash,csrf_hash,profile_id,kind,device_name,access_expires,refresh_expires,created_at) VALUES(?1,1,?1,?1,'unused',1,'browser','test',4102444800,4102444800,0)",[id]).unwrap();
        }
    }
    fn owned(app: &App, id: &str) -> App {
        let principal = auth::Principal::Account {
            account_id: 1,
            role: "owner".into(),
            profile_id: Some(1),
            session_id: Some(id.into()),
        };
        let a = app.clone().with_lease(ResourceLease {
            policy_revision: 0,
            principal,
            session_id: Some(id.into()),
        });
        a.streams.lock().unwrap().insert(
            id.into(),
            StreamEntry {
                provider_id: None,
                kind: "movie".into(),
                live: false,
                url: "http://fixture.invalid/movie.mp4".into(),
                headers: HashMap::new(),
                created: Instant::now(),
            },
        );
        a.own_resource("stream", id);
        a
    }
    (owned(&app, "one"), owned(&app, "two"), dir)
}
fn request(id: &str, position: f64) -> PlaybackRequest {
    serde_json::from_value(json!({"stream_id":id,"position":position})).unwrap()
}
#[tokio::test]
async fn vod_reuses_available_timeline_and_keeps_seeking_and_revocation_independent() {
    for provider in [false, true] {
        let (a, b, _dir) = fixture();
        if provider {
            a.db.lock().unwrap().execute("INSERT INTO providers(id,name,url,username,password,max_connections) VALUES(1,'VOD fixture','http://fixture.invalid','fixture','fixture',2)",[]).unwrap();
            for source in a.streams.lock().unwrap().values_mut() {
                source.provider_id = Some(1);
            }
        }
        let first = start(a.clone(), request("one", 0.0)).await.unwrap().0;
        let second = start(b.clone(), request("two", 0.0)).await.unwrap().0;
        assert_eq!(a.playback.active_count().await, 1);
        assert_ne!(first["id"], second["id"]);
        let sought = start(b.clone(), request("two", 120.0)).await.unwrap().0;
        assert_eq!(a.playback.active_count().await, 2);
        a.db.lock()
            .unwrap()
            .execute("DELETE FROM auth_sessions WHERE id='one'", [])
            .unwrap();
        tokio::time::sleep(Duration::from_millis(350)).await;
        assert_eq!(a.playback.active_count().await, 2);
        let second_id = second["id"].as_str().unwrap();
        assert!(heartbeat(&b, second_id, None).await.unwrap().is_ok());
        assert!(stop(&b, second_id).await);
        assert_eq!(a.playback.active_count().await, 1);
        assert!(stop(&b, sought["id"].as_str().unwrap()).await);
        assert_eq!(a.playback.active_count().await, 0);
        a.playback.shutdown().await;
    }
}
#[tokio::test]
async fn cancelled_creator_during_probe_does_not_cancel_the_other_viewer() {
    let (a, b, dir) = fixture();
    let hold = dir.path().join("probe.hold");
    std::fs::write(&hold, "hold").unwrap();
    let worker_a = a.clone();
    let first = tokio::spawn(async move { start(worker_a, request("one", 0.0)).await });
    tokio::time::timeout(Duration::from_secs(3), async {
        while !dir.path().join("probe.started").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let worker_b = b.clone();
    let second = tokio::spawn(async move { start(worker_b, request("two", 0.0)).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while a.shared_playback.ids().len() != 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    first.abort();
    let _ = first.await;
    a.db.lock()
        .unwrap()
        .execute("DELETE FROM auth_sessions WHERE id='one'", [])
        .unwrap();
    std::fs::remove_file(hold).unwrap();
    let viewer = tokio::time::timeout(Duration::from_secs(5), second)
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .0;
    assert_eq!(a.playback.active_count().await, 1);
    assert!(stop(&b, viewer["id"].as_str().unwrap()).await);
    assert_eq!(a.playback.active_count().await, 0);
    a.playback.shutdown().await;
}

#[tokio::test]
async fn shared_identity_separates_requested_audio_languages() {
    let (a, _, _dir) = fixture();
    let mut english = request("one", 0.0);
    english.audio_language = Some("eng".into());
    let mut japanese = english.clone();
    japanese.audio_language = Some("jpn".into());
    assert_ne!(
        identity(&a, &english).await.unwrap().0,
        identity(&a, &japanese).await.unwrap().0
    );
    assert_eq!(
        identity(&a, &english).await.unwrap().0,
        identity(&a, &english.clone()).await.unwrap().0
    );
    a.playback.shutdown().await;
}

#[tokio::test]
async fn original_media_uses_private_viewer_capabilities_and_revocable_bodies() {
    use axum::{
        http::{HeaderMap, Method},
        response::Response,
        routing::get,
        Router,
    };
    let (a, b, dir) = fixture();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/movie.mp4", listener.local_addr().unwrap());
    let app = Router::new().route(
        "/movie.mp4",
        get(|headers: HeaderMap| async move {
            let data = b"0123456789abcdef";
            let range = headers[header::RANGE]
                .to_str()
                .unwrap()
                .trim_start_matches("bytes=");
            let (start, end) = range.split_once('-').unwrap();
            let start = start.parse::<usize>().unwrap();
            let end = end.parse::<usize>().unwrap();
            Response::builder()
                .status(206)
                .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/16"))
                .body(axum::body::Body::from(data[start..=end].to_vec()))
                .unwrap()
        }),
    );
    let origin = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    std::fs::write(dir.path().join("probe"),r#"#!/bin/sh
printf '%s' '{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":640,"height":360,"pix_fmt":"yuv420p","profile":"High","level":41,"avg_frame_rate":"30/1","r_frame_rate":"30/1","field_order":"progressive"},{"index":1,"codec_type":"audio","codec_name":"aac","profile":"LC","channels":2,"tags":{"language":"eng"}}],"format":{"format_name":"mov,mp4","duration":"100"}}'
"#).unwrap();
    for source in a.streams.lock().unwrap().values_mut() {
        source.url = url.clone();
    }
    let mut first_request = request("one", 0.0);
    first_request.capabilities = Some(playback::Capabilities {
        direct_play: true,
        ..Default::default()
    });
    let mut second_request = first_request.clone();
    second_request.stream_id = Some("two".into());
    let first = start(a.clone(), first_request).await.unwrap().0;
    let second = start(b.clone(), second_request).await.unwrap().0;
    assert_eq!(first["mode"], "direct");
    assert_eq!(a.playback.active_count().await, 1);
    assert_ne!(first["url"], second["url"]);
    let parts = second["url"]
        .as_str()
        .unwrap()
        .split('/')
        .collect::<Vec<_>>();
    let tuple = (
        parts[2].to_owned(),
        parts[3].to_owned(),
        parts[4].to_owned(),
    );
    let mut headers = HeaderMap::new();
    headers.insert(header::RANGE, "bytes=4-9".parse().unwrap());
    let response = crate::session::media(
        State(b.clone()),
        Path(tuple.clone()),
        Method::GET,
        headers.clone(),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), 206);
    assert_eq!(
        axum::body::to_bytes(response.into_body(), 100)
            .await
            .unwrap(),
        "456789"
    );
    let pending = crate::session::media(State(b.clone()), Path(tuple), Method::GET, headers)
        .await
        .unwrap();
    b.db.lock()
        .unwrap()
        .execute("DELETE FROM auth_sessions WHERE id='two'", [])
        .unwrap();
    assert!(
        axum::body::to_bytes(pending.into_body(), 100)
            .await
            .is_err(),
        "revoked viewer must receive no buffered media"
    );
    assert!(stop(&a, first["id"].as_str().unwrap()).await);
    a.playback.shutdown().await;
    origin.abort();
}
