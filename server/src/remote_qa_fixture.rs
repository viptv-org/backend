use crate::*;
#[path = "remote_qa_gateway.rs"]
mod qa_gateway;

#[tokio::test]
#[ignore = "Starts a loopback QA server; run explicitly with --ignored --nocapture"]
async fn remote_addon_browser_fixture() {
    let directory = tempfile::Builder::new()
        .prefix("viptv-remote-addon-")
        .tempdir()
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let database = directory.path().join("qa.sqlite");
    let mut app = test_support::app_with_db(Connection::open(&database).unwrap());
    test_support::configure_vault(&mut app);
    app.addons.allow_test_loopback = true;
    let password = format!("qa-{}", Uuid::new_v4());
    let profile = {
        let mut db = app.db.lock().unwrap();
        db.execute("DELETE FROM addons", []).unwrap();
        auth::create_owner_offline(&mut db, "qa-parent", "QA Parent", &password).unwrap();
        let profile = auth::create_profile(&db, 1, &json!({"name":"QA Parent"})).unwrap();
        db.execute("INSERT INTO auth_onboarding(account_id,state) VALUES(1,'complete') ON CONFLICT(account_id) DO UPDATE SET state='complete'",[]).unwrap();
        profile["id"].as_str().unwrap().to_owned()
    };
    let clip = directory.path().join("fixture.mp4");
    let generated = std::process::Command::new("/usr/bin/ffmpeg")
        .args([
            "-nostdin",
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=12",
            "-t",
            "30",
            "-an",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-profile:v",
            "baseline",
            "-pix_fmt",
            "yuv420p",
            "-g",
            "24",
            "-bf",
            "0",
            "-movflags",
            "+faststart",
        ])
        .arg(&clip)
        .output()
        .unwrap();
    assert!(
        generated.status.success(),
        "Could not generate the disposable clip"
    );
    let movie = Arc::new(std::fs::read(&clip).unwrap());
    let addon = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addon_origin = format!("http://{}", addon.local_addr().unwrap());
    let manifest = format!("{addon_origin}/manifest.json");
    let movie_url = format!("{addon_origin}/movie.mp4");
    let gateway = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway_upstream = format!("http://{}/", gateway.local_addr().unwrap());
    let gateway_key = format!("pgk_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    app.gateway_client = gateway::client::Client::fixture(gateway_upstream.parse().unwrap());
    gateway::registry::register(&app.db.lock().unwrap(), app.secret_vault.as_ref().unwrap(), 1,
        serde_json::from_value(json!({"name":"Disposable FFmpeg QA", "endpoint":"https://viptv.local.test:18443/gateway/",
        "namespace":"browser-qa", "priority":1, "integration_key":gateway_key})).unwrap()).unwrap();
    let gateway_routes = qa_gateway::router(
        movie_url.clone(),
        directory.path().join("gateway"),
        gateway_key.clone(),
    );
    std::fs::create_dir(directory.path().join("gateway")).unwrap();
    let gateway_task = tokio::spawn(async move {
        axum::serve(gateway, gateway_routes).await.unwrap();
    });
    let upstream = tokio::spawn(async move {
        let mock = Router::new().fallback(move |request: Request| {
            let origin = addon_origin.clone();
            let movie = movie.clone();
            async move {
                use axum::response::IntoResponse;
                let path = request.uri().path();
                if path == "/movie.mp4" {
                    return ([ ("content-type", "video/mp4"), ("cache-control", "private, no-store") ], movie.as_ref().clone()).into_response();
                }
                if path == "/english.srt" {
                    return ([ ("content-type", "application/x-subrip") ], "1\n00:00:00,000 --> 00:00:25,000\nVIPTV PROVIDER SUBTITLE\n\n2\n00:00:25,000 --> 00:00:30,000\nSECOND PROVIDER CUE\n").into_response();
                }
                if path == "/spanish.vtt" {
                    return ([ ("content-type", "text/vtt") ], "WEBVTT\n\n00:00.000 --> 00:30.000 line:12%\nVTT PROVIDER SUBTITLE\n").into_response();
                }
                axum::Json(if path == "/manifest.json" {
                    json!({"id":"viptv.qa.addon","version":"1.0.0","name":"QA Catalog","description":"Disposable local addon fixture","resources":["catalog","meta","stream","subtitles"],"types":["movie"],"catalogs":[{"id":"qa","type":"movie","name":"QA Movies","extra":[{"name":"search","isRequired":false}]}]})
                } else if path.starts_with("/meta/") {
                    json!({"meta":{"id":"tt1234567","type":"movie","name":"Fixture Movie","description":"Deterministic local QA title"}})
                } else if path.starts_with("/stream/") {
                    json!({"streams":[{"name":"QA H264 clip","url":format!("{origin}/movie.mp4")} ]})
                } else if path.starts_with("/subtitles/") {
                    json!({"subtitles":[{"id":"english-srt","lang":"eng","url":format!("{origin}/english.srt")},{"id":"spanish-vtt","lang":"spa","url":format!("{origin}/spanish.vtt")} ]})
                } else {
                    json!({"metas":[{"id":"tt1234567","type":"movie","name":"Fixture Movie","description":"Deterministic local QA title"}]})
                }).into_response()
            }
        });
        axum::serve(addon, mock).await.unwrap();
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:18183")
        .await
        .unwrap();
    let credentials = directory.path().join("fixture.json");
    std::fs::write(&credentials,serde_json::to_vec_pretty(&json!({"username":"qa-parent","password":password,"profile_id":profile,"profile_name":"QA Parent","manifest_url":manifest,"api_upstream":"http://127.0.0.1:18183","origin":"https://viptv.local.test:18443","database":database,"gateway_upstream":gateway_upstream,"gateway_integration_key":gateway_key,"clip_path":clip})).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [&credentials, &database] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
    println!("Disposable QA fixture: {}", credentials.display());
    axum::serve(listener, router(app))
        .with_graceful_shutdown(async {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        })
        .await
        .unwrap();
    upstream.abort();
    gateway_task.abort();
}
