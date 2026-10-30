use crate::*;
use crate::{auth_integration_tests::fixture, test_support::request};
use axum::routing::get;
use std::sync::atomic::{AtomicUsize, Ordering};

async fn stack() -> (App, tokio::task::JoinHandle<()>, Arc<AtomicUsize>) {
    let revision = Arc::new(AtomicUsize::new(0));
    let routes=Router::new()
      .route("/a/:addon/meta/:kind/:id",get({let revision=revision.clone();move|Path((addon,kind,id)):Path<(i64,String,String)>|{let revision=revision.clone();async move{
        let id=id.trim_end_matches(".json");let alias=if addon==102{"tt999"}else if revision.load(Ordering::SeqCst)==0{"tt123"}else{"tt456"};
        axum::Json(json!({"meta":{"id":id,"type":kind,"name":format!("Title {alias}"),"imdb_id":alias,"videos":[{"id":format!("{id}:1:1"),"season":1,"episode":1,"title":"Pilot","released":"2020-01-01T00:00:00Z"},{"id":format!("{id}:1:2"),"season":1,"episode":2,"title":"Second","released":"2020-01-02T00:00:00Z"}]}}))
      }}}))
      .route("/a/:addon/stream/:kind/:id",get(||async{axum::Json(json!({"streams":[{"url":"http://fixture.invalid/private.mp4","name":"Private fixture"}]}))}))
      .route("/a/:addon/subtitles/:kind/:id",get(|headers:axum::http::HeaderMap|async move{let host=headers[header::HOST].to_str().unwrap();axum::Json(json!({"subtitles":[{"lang":"eng","url":format!("http://{host}/caption.srt")}]}))}))
      .route("/caption.srt",get(||async{([(header::CONTENT_TYPE,"application/x-subrip")],"1\n00:00:01,000 --> 00:00:02,000\nOrigin fixture\n")}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    let mut app = fixture();
    app.addons.allow_test_loopback = true;
    for id in [101, 102] {
        let manifest = json!({"id":format!("origin{id}"),"name":format!("Origin {id}"),"resources":["meta","stream","subtitles"],"types":["movie","series"]});
        app.db.lock().unwrap().execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id)VALUES(?1,'Origin',?2,?3,1)",params![id,format!("http://{address}/a/{id}/manifest.json"),manifest.to_string()]).unwrap();
    }
    crate::test_support::encrypt_fixture_sources(&app);
    (app, task, revision)
}
fn query(path: &str, origin: &Value, card: Option<&Value>) -> String {
    let mut q = url::form_urlencoded::Serializer::new(String::new());
    q.append_pair("metadata_origin", &origin.to_string());
    if let Some(card) = card {
        q.append_pair("card", &card.to_string());
    }
    format!("{path}?{}", q.finish())
}
async fn detail(app: &App, addon: i64) -> Value {
    let (status, value) = request(
        app,
        "member-token-1",
        "GET",
        &format!("/api/meta/series/private-show?provider_id=stremio&addon_id={addon}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value
}
#[tokio::test]
async fn private_origin_approval_detail_next_caption_source_and_revocation_are_isolated() {
    let (app, task, _) = stack().await;
    let a = detail(&app, 101).await;
    let b = detail(&app, 102).await;
    let card = &a["item"];
    let origin = &card["metadata_origin"];
    assert_eq!(origin["connection_id"], "addon:101");
    assert_eq!(card["identity"]["ids"][0]["value"], "tt123");
    let body = json!({"type":"series","id":"private-show","approved":true,"metadata_origin":origin,"card":card});
    let (status, result) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/profiles/1/approvals",
        body,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO kids_profiles(profile_id,enabled,max_age,revision)VALUES(1,1,7,1)ON CONFLICT(profile_id)DO UPDATE SET enabled=1,max_age=7,revision=revision+1",
            [],
        )
        .unwrap();
    let (status, approved) = request(
        &app,
        "member-token-1",
        "GET",
        &query("/api/meta/series/private-show", origin, Some(card)),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{approved}");
    assert_eq!(approved["episodes"].as_array().unwrap().len(), 2);
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            &query(
                "/api/meta/series/private-show",
                &b["item"]["metadata_origin"],
                Some(&b["item"])
            ),
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let episode = &approved["episodes"][0];
    let next = json!({"identity":episode["identity"],"parent_identity":episode["parent_identity"],"metadata_origin":origin,"card":episode});
    let (status, next) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/profiles/1/continue/next",
        next,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{next}");
    assert_eq!(next["status"], "Next");
    assert_eq!(next["item"]["metadata_origin"], *origin);
    let subtitle = query(
        "/api/subtitles/series/private-show:1:1",
        origin,
        Some(episode),
    );
    let (status, subtitles) = request(&app, "member-token-1", "GET", &subtitle, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{subtitles}");
    assert_eq!(subtitles["candidates"].as_array().unwrap().len(), 1);
    let caption = format!(
        "/api/subtitle-content/{}",
        subtitles["candidates"][0]["id"].as_str().unwrap()
    );
    {
        use tower::ServiceExt;
        let response = crate::router(app.clone())
            .oneshot(
                axum::http::Request::builder()
                    .uri(&caption)
                    .header(header::AUTHORIZATION, "Bearer member-token-1")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert!(String::from_utf8(bytes.to_vec())
            .unwrap()
            .starts_with("WEBVTT"));
    }
    let source = json!({"type":"series","id":"private-show:1:1","series_id":"private-show","season":1,"episode":1,"metadata_origin":origin,"card":episode,"only_addons":true});
    let (status, job) = request(&app, "member-token-1", "POST", "/api/streams", source).await;
    assert_eq!(status, StatusCode::OK, "{job}");
    let poll = format!("/api/streams/{}", job["id"].as_str().unwrap());
    let found = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let (status, found) = request(&app, "member-token-1", "GET", &poll, Value::Null).await;
            assert_eq!(status, StatusCode::OK, "{found}");
            if found["done"] == true {
                break found;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let events = found["events"].as_array().unwrap();
    assert!(events.iter().all(|event| event["source"] != "addon:102"));
    let source = events
        .iter()
        .flat_map(|event| event["streams"].as_array().into_iter().flatten())
        .next()
        .expect("mock-addon emitted source")["id"]
        .as_str()
        .unwrap();
    let(status,playback)=request(&app,"member-token-1","POST","/api/playback",json!({"request_id":"origin-child","stream_id":source,"client":{"platform":"android","can_play_direct":true,"max_width":1920,"max_height":1080,"video_codecs":["h264"],"audio_codecs":["aac"]},"position":0})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{playback}");
    let playback_path = format!("/api/playback/{}", playback["id"].as_str().unwrap());
    let ready = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let (status, value) =
                request(&app, "member-token-1", "GET", &playback_path, Value::Null).await;
            assert_eq!(status, StatusCode::OK, "{value}");
            if value["status"] != "starting" {
                break value;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(ready["status"], "ready", "{ready}");
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            &format!("{playback_path}/heartbeat"),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO parent_grants(session_id,expires)VALUES('s1',?1)",
            [util::now() + 120],
        )
        .unwrap();
    let (status, list) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/approvals",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let stored = &list["items"][0]["card"];
    let(status,result)=request(&app,"member-token-1","POST","/api/profiles/1/approvals",json!({"type":"series","id":"private-show","approved":false,"metadata_origin":origin,"card":stored})).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            &query("/api/meta/series/private-show", origin, Some(card)),
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&app, "member-token-1", "GET", &caption, Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&app, "member-token-1", "GET", &poll, Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    // Drive the stale-session sweep through this endpoint instead of racing its one-second tick.
    assert_eq!(
        request(&app, "member-token-1", "GET", &playback_path, Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            &format!("{playback_path}/heartbeat"),
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    task.abort();
}

#[tokio::test]
async fn private_origin_stale_expected_alias_and_reinstalled_connector_are_rejected() {
    let (app, task, revision) = stack().await;
    let a = detail(&app, 101).await;
    let card = &a["item"];
    let origin = &card["metadata_origin"];
    revision.store(1, Ordering::SeqCst);
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE addons SET name='Reconfigured' WHERE id=101", [])
        .unwrap();
    let (status, error) = request(
        &app,
        "member-token-1",
        "GET",
        &query("/api/meta/series/private-show", origin, Some(card)),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    let(status,error)=request(&app,"member-token-1","POST","/api/profiles/1/approvals",json!({"type":"series","id":"private-show","approved":true,"metadata_origin":origin,"card":card})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    let db = app.db.lock().unwrap();
    let row: (String, String, String) = db
        .query_row(
            "SELECT name,manifest_url,manifest FROM addons WHERE id=101",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    db.execute("DELETE FROM addons WHERE id=101", []).unwrap();
    db.execute(
        "INSERT INTO addons(id,name,manifest_url,manifest,account_id)VALUES(101,?1,?2,?3,1)",
        params![row.0, row.1, row.2],
    )
    .unwrap();
    drop(db);
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            &query("/api/meta/series/private-show", origin, Some(card)),
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    task.abort();
}

#[tokio::test]
async fn reserved_namespace_and_posted_matching_hints_never_mint_scoped_authority() {
    let (app, task, _) = stack().await;
    let detail = detail(&app, 101).await;
    let mut spoof = detail["item"].clone();
    spoof["identity"]["id"] = json!("simkl:tv:10");
    for (path, method, body) in [
        (
            query(
                "/api/meta/series/simkl:tv:10",
                &spoof["metadata_origin"],
                Some(&spoof),
            ),
            "GET",
            Value::Null,
        ),
        (
            "/api/profiles/1/approvals".into(),
            "POST",
            json!({"type":"series","id":"simkl:tv:10","approved":true,"metadata_origin":spoof["metadata_origin"],"card":spoof}),
        ),
        (
            "/api/profiles/1/library".into(),
            "PUT",
            json!({"card":spoof}),
        ),
        (
            "/api/streams".into(),
            "POST",
            json!({"type":"series","id":"simkl:tv:10","metadata_origin":spoof["metadata_origin"],"card":spoof}),
        ),
    ] {
        assert_eq!(
            request(&app, "member-token-1", method, &path, body).await.0,
            StatusCode::FORBIDDEN
        );
    }
    let host: viptv_engine::presentation::Card =
        serde_json::from_value(detail["item"].clone()).unwrap();
    let safe = super::origins::source_value(&host);
    assert_eq!(safe["id"], "private-show");
    assert_ne!(safe["name"], "unrelated posted title");
    assert!(safe.get("aliases").is_none());
    assert!(safe.get("only_provider_id").is_none());
    task.abort();
}

#[tokio::test]
async fn captions_with_same_raw_id_require_the_exact_origin_and_assertion_context() {
    let (app, task, _) = stack().await;
    let a = detail(&app, 101).await;
    let b = detail(&app, 102).await;
    let lease = ResourceLease {
        policy_revision: 0,
        principal: auth::Principal::Account {
            account_id: 1,
            role: "owner".into(),
            profile_id: Some(1),
            session_id: Some("s1".into()),
        },
        session_id: Some("s1".into()),
    };
    let scoped = app.clone().with_lease(lease.clone());
    let expected: viptv_engine::presentation::Card =
        serde_json::from_value(a["episodes"][0].clone()).unwrap();
    let proof = super::origins::MetadataProof::verified(&scoped, &expected)
        .await
        .unwrap();
    let mut selections = Vec::new();
    for episode in [&a["episodes"][0], &b["episodes"][0]] {
        let (status, result) = request(
            &app,
            "member-token-1",
            "GET",
            &query(
                "/api/subtitles/series/private-show:1:1",
                &episode["metadata_origin"],
                Some(episode),
            ),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{result}");
        selections.push(result["candidates"][0]["id"].as_str().unwrap().to_owned());
        app.subtitle_selections
            .retain_content(&selections.last().unwrap(), &lease, Arc::from("WEBVTT\n\n"))
            .unwrap();
    }
    assert!(super::subtitles::validate_playback_context(
        &scoped,
        &lease,
        &selections[0],
        Some(&proof)
    )
    .is_ok());
    assert_eq!(
        super::subtitles::validate_playback_context(&scoped, &lease, &selections[1], Some(&proof))
            .unwrap_err()
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        super::subtitles::validate_playback_context(&scoped, &lease, &selections[0], None)
            .unwrap_err()
            .0,
        StatusCode::FORBIDDEN
    );
    task.abort();
}

#[tokio::test]
async fn delegated_metadata_proof_covers_the_actual_matching_producer_beyond_unrelated_types() {
    let routes=Router::new().route("/a/:addon/meta/:kind/:id",get(|Path((_,kind,id)):Path<(i64,String,String)>|async move{axum::Json(json!({"meta":{"id":id.trim_end_matches(".json"),"type":kind,"name":"Delegated movie","imdb_id":"tt1234567"}}))}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    let mut app = fixture();
    app.addons.allow_test_loopback = true;
    for id in 1..=34 {
        let kind = if id == 34 { "movie" } else { "series" };
        let manifest = json!({"id":format!("delegate{id}"),"name":"Delegate","resources":["meta"],"types":[kind]});
        app.db.lock().unwrap().execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id)VALUES(?1,'Delegate',?2,?3,1)",params![id,format!("http://{address}/a/{id}/manifest.json"),manifest.to_string()]).unwrap();
    }
    let manifest = json!({"id":"search","name":"Search issuer","resources":["catalog"],"types":["movie"],"catalogs":[{"id":"search","type":"movie","extra":[{"name":"search"}]}]});
    app.db.lock().unwrap().execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id)VALUES(200,'Search',?1,?2,1)",params![format!("http://{address}/a/200/manifest.json"),manifest.to_string()]).unwrap();
    crate::test_support::encrypt_fixture_sources(&app);
    let (status, detail) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/meta/movie/private-movie?provider_id=stremio&addon_id=200",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    let card: viptv_engine::presentation::Card =
        serde_json::from_value(detail["item"].clone()).unwrap();
    let lease = ResourceLease {
        policy_revision: 0,
        principal: auth::Principal::Account {
            account_id: 1,
            role: "owner".into(),
            profile_id: Some(1),
            session_id: Some("s1".into()),
        },
        session_id: Some("s1".into()),
    };
    let scoped = app.clone().with_lease(lease.clone());
    let proof = super::origins::MetadataProof::verified(&scoped, &card)
        .await
        .unwrap();
    let db = app.db.lock().unwrap();
    db.execute("UPDATE addons SET manifest=json_set(manifest,'$.description','changed metadata authority')WHERE id=34",[]).unwrap();
    assert_eq!(
        proof.validate(&db, &lease).unwrap_err().0,
        StatusCode::CONFLICT
    );
    server.abort();
}
