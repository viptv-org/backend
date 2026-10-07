use super::*;
use crate::{auth, auth_integration_tests::fixture, test_support::request};
use axum::{body::Body, http::Request};
use tower::ServiceExt;

#[tokio::test]
async fn default_native_admission_supports_phone_and_tv_with_two_grants_per_session() {
    for platform in ["android", "android_tv"] {
        for paired in [false, true] {
            let app = setup().await;
            if paired {
                app.db
                    .lock()
                    .unwrap()
                    .execute("UPDATE auth_sessions SET kind='device' WHERE id='s1'", [])
                    .unwrap();
            }
            let source = torrent(&app, Some(0));
            if paired {
                for owner in app.resource_owners.lock().unwrap().values_mut() {
                    let auth::Principal::Account { role, .. } = &mut owner.lease.principal;
                    *role = "device".into();
                }
            }
            let input = |id: &str| {
                let mut input = body(&source, id);
                input["client"]["platform"] = json!(platform);
                input
            };
            let first = ready(&app, input("first_default")).await;
            ready(&app, input("second_default")).await;
            assert_eq!(
                request(
                    &app,
                    "member-token-1",
                    "POST",
                    "/api/v2/playback",
                    input("third_default")
                )
                .await
                .1["error_code"],
                "playback_capacity"
            );
            request(
                &app,
                "member-token-1",
                "DELETE",
                &format!("/api/v2/playback/{}", first["id"].as_str().unwrap()),
                Value::Null,
            )
            .await;
            ready(&app, input("after_release_default")).await;
        }
    }
}

#[tokio::test]
async fn default_native_admission_is_independent_for_other_accounts_and_sessions() {
    let app = setup().await;
    let first_source = torrent(&app, Some(0));
    ready(&app, body(&first_source, "account_one_first")).await;
    ready(&app, body(&first_source, "account_one_second")).await;
    app.db.lock().unwrap().execute_batch("INSERT INTO profiles(id,name,avatar_seed,presentation_complete) VALUES(2,'Second','two',1); INSERT INTO profile_owners VALUES(2,2,0); INSERT INTO auth_profiles VALUES(2,2); UPDATE auth_sessions SET profile_id=2 WHERE account_id=2; INSERT INTO addons(id,name,manifest_url,enabled,manifest,account_id) VALUES(2,'Second','https://second.fixture.invalid/manifest.json',1,'{}',2);").unwrap();
    crate::test_support::encrypt_fixture_sources(&app);
    let second_lease = ResourceLease {
        policy_revision: 0,
        principal: auth::Principal::Account {
            account_id: 2,
            role: "member".into(),
            profile_id: Some(2),
            session_id: Some("s2".into()),
        },
        session_id: Some("s2".into()),
    };
    let (cards, error) = app.clone().with_lease(second_lease).register(
        "addon:2",
        vec![json!({"infoHash":"0000000000000000000000000000000000000000", "fileIdx":0})],
        "movie",
    );
    assert!(error.is_none());
    let second_source = cards[0]["id"].as_str().unwrap();
    app.streams
        .lock()
        .unwrap()
        .get_mut(second_source)
        .unwrap()
        .exact_vod = Some(crate::app_state::ExactVod {
        title: "second_movie".into(),
        series: None,
        season: None,
        episode: None,
    });
    assert_eq!(
        request(
            &app,
            "member-token-2",
            "GET",
            "/api/v2/playback-protocol",
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    ready_as(
        &app,
        "member-token-2",
        body(second_source, "account_two_first"),
    )
    .await;
    ready_as(
        &app,
        "member-token-2",
        body(second_source, "account_two_second"),
    )
    .await;
    assert_eq!(
        request(
            &app,
            "member-token-2",
            "POST",
            "/api/v2/playback",
            body(second_source, "account_two_third")
        )
        .await
        .1["error_code"],
        "playback_capacity"
    );
}

fn lease() -> ResourceLease {
    ResourceLease {
        policy_revision: 0,
        principal: auth::Principal::Account {
            account_id: 1,
            role: "member".into(),
            profile_id: Some(1),
            session_id: Some("s1".into()),
        },
        session_id: Some("s1".into()),
    }
}
fn torrent(app: &App, index: Option<u32>) -> String {
    app.db.lock().unwrap().execute("INSERT OR IGNORE INTO addons(id,name,manifest_url,enabled,manifest,account_id) VALUES(1,'Fixture','https://fixture.invalid/manifest.json',1,'{}',1)",[]).unwrap();
    crate::test_support::encrypt_fixture_sources(app);
    let mut input = json!({"infoHash":"0000000000000000000000000000000000000000"});
    if let Some(index) = index {
        input["fileIdx"] = json!(index);
    }
    let (cards, error) = app
        .clone()
        .with_lease(lease())
        .register("addon:1", vec![input], "series");
    assert!(error.is_none());
    let id = cards[0]["id"].as_str().unwrap().to_owned();
    app.streams.lock().unwrap().get_mut(&id).unwrap().exact_vod =
        Some(crate::app_state::ExactVod {
            title: "episode_fixture".into(),
            series: Some("series_fixture".into()),
            season: Some(1),
            episode: Some(2),
        });
    id
}
fn body(source: &str, id: &str) -> Value {
    json!({"request_id":id,"stream_id":source,"client":{"platform":"android_tv","can_play_direct":false,"max_width":1920,"max_height":1080,"video_codecs":["h264"],"audio_codecs":["aac"],"native_torrent":{"version":1,"network_policy":"public_dht_tcp_v1"}}})
}
async fn setup() -> App {
    let app = fixture();
    let (status, value) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v2/playback-protocol",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["native_torrent_versions"], json!([1]));
    app
}
async fn ready(app: &App, input: Value) -> Value {
    ready_as(app, "member-token-1", input).await
}
async fn ready_as(app: &App, token: &str, input: Value) -> Value {
    let (status, value) = request(app, token, "POST", "/api/v2/playback", input).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{value}");
    let id = value["id"].as_str().unwrap();
    for _ in 0..100 {
        let (_, value) = request(
            app,
            token,
            "GET",
            &format!("/api/v2/playback/{id}"),
            Value::Null,
        )
        .await;
        if value["status"] != "starting" {
            assert_eq!(value["status"], "ready", "{value}");
            return value;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("native did not settle")
}
#[tokio::test]
async fn native_grants_are_exact_independent_transient_and_poll_never_renews() {
    let app = setup().await;
    let source = torrent(&app, Some(7));
    let first = ready(&app, body(&source, "first")).await;
    let second = ready(&app, body(&source, "second")).await;
    assert_eq!(first["delivery"]["grant"]["file_index"], 7);
    assert_ne!(
        first["delivery"]["grant"]["id"],
        second["delivery"]["grant"]["id"]
    );
    assert!(first["delivery"].get("url").is_none());
    let id = first["id"].as_str().unwrap();
    let initial_expiry = first["expires_at"].clone();
    let (_, polled) = request(
        &app,
        "member-token-1",
        "GET",
        &format!("/api/v2/playback/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(polled["expires_at"], initial_expiry);
    let (status, retry) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/playback",
        body(&source, "first"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        retry["delivery"]["grant"]["id"],
        first["delivery"]["grant"]["id"]
    );
    let (_, cancelled) = request(
        &app,
        "member-token-1",
        "DELETE",
        "/api/v2/playback-requests/first",
        Value::Null,
    )
    .await;
    assert_eq!(cancelled, json!({"ok":true}));
    let (_, retired) = request(
        &app,
        "member-token-1",
        "GET",
        &format!("/api/v2/playback/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(retired["status"], "released");
    assert!(retired["delivery"].is_null());
    assert!(!retired.to_string().contains("magnet:"));
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            &format!("/api/v2/playback/{}", second["id"].as_str().unwrap()),
            Value::Null
        )
        .await
        .1["status"],
        "ready"
    );
    assert_ne!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/v2/playback",
            body(&source, "first")
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
}
#[tokio::test]
async fn native_negotiation_exact_selection_and_server_owned_controls_gate_admission() {
    let app = setup().await;
    let source = torrent(&app, Some(0));
    for (field, value) in [
        ("force_gateway", json!(true)),
        ("conversion", json!("audio")),
        ("audio_track", json!(1)),
        ("subtitle_track", json!(2)),
        ("audio_language", json!("fr")),
        ("subtitles_off", json!(true)),
    ] {
        let mut input = body(&source, field);
        input[field] = value;
        assert_eq!(
            request(&app, "member-token-1", "POST", "/api/v2/playback", input)
                .await
                .1["error_code"],
            "gateway_required"
        );
    }
    let missing = torrent(&app, None);
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/v2/playback",
            body(&missing, "missing")
        )
        .await
        .1["error_code"],
        "gateway_required"
    );
    let mut preferred = body(&source, "preferred");
    preferred["preferred_audio_language"] = json!("fr");
    assert_eq!(
        ready(&app, preferred).await["delivery"]["preferences"]["audio_language"],
        "fr"
    );
    app.gateway_playbacks.negotiated.lock().unwrap().clear();
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/v2/playback",
            body(&source, "unnegotiated")
        )
        .await
        .1["error_code"],
        "gateway_required"
    );
}
#[tokio::test]
async fn conflicting_retries_and_other_scopes_cannot_change_source_file_or_transport() {
    let app = setup().await;
    let source = torrent(&app, Some(0));
    let other = torrent(&app, Some(1));
    let first = ready(&app, body(&source, "bound")).await;
    for input in [body(&other, "bound"), {
        let mut input = body(&source, "bound");
        input["force_gateway"] = json!(true);
        input
    }] {
        let (status, value) =
            request(&app, "member-token-1", "POST", "/api/v2/playback", input).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(value["error_code"], "playback_conflict");
    }
    assert_eq!(
        request(
            &app,
            "member-token-2",
            "DELETE",
            "/api/v2/playback-requests/bound",
            Value::Null
        )
        .await
        .1,
        json!({"ok":true})
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            &format!("/api/v2/playback/{}", first["id"].as_str().unwrap()),
            Value::Null
        )
        .await
        .1["status"],
        "ready"
    );
}
#[tokio::test]
async fn same_session_different_profile_cancellation_does_not_mutate_existing_authority() {
    let app = setup().await;
    let source = torrent(&app, Some(0));
    ready(&app, body(&source, "profile-bound")).await;
    {
        let db = app.db.lock().unwrap();
        db.execute("INSERT INTO profiles(id,name,avatar_seed,presentation_complete) VALUES(2,'Other','fixture-two',1)", []).unwrap();
        db.execute(
            "INSERT INTO profile_owners(profile_id,account_id,created_at) VALUES(2,1,0)",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO auth_profiles VALUES(1,2)", [])
            .unwrap();
        db.execute("UPDATE auth_sessions SET profile_id=2 WHERE id='s1'", [])
            .unwrap();
    }
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "DELETE",
            "/api/v2/playback-requests/profile-bound",
            Value::Null
        )
        .await
        .1,
        json!({"ok":true})
    );
    let cancelled: bool = app.db.lock().unwrap().query_row("SELECT cancelled FROM playback_request_authority WHERE session_id='s1' AND request_id='profile-bound'",[],|r|r.get(0)).unwrap();
    assert!(!cancelled);
}
#[tokio::test]
async fn cancellation_races_admission_and_survives_entry_retirement_and_registry_restart() {
    let mut app = setup().await;
    let source = torrent(&app, Some(0));
    for index in 0..20 {
        let id = format!("race{index}");
        let cancellation_path = format!("/api/v2/playback-requests/{id}");
        let (_, cancelled) = tokio::join!(
            request(
                &app,
                "member-token-1",
                "POST",
                "/api/v2/playback",
                body(&source, &id)
            ),
            request(
                &app,
                "member-token-1",
                "DELETE",
                &cancellation_path,
                Value::Null
            )
        );
        assert_eq!(cancelled.1, json!({"ok":true}));
        assert_ne!(
            request(
                &app,
                "member-token-1",
                "POST",
                "/api/v2/playback",
                body(&source, &id)
            )
            .await
            .0,
            StatusCode::ACCEPTED
        );
    }
    app.gateway_playbacks = Registry::new(app.db.clone());
    assert_ne!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/v2/playback",
            body(&source, "race0")
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "DELETE",
            "/api/v2/playback-requests/race0",
            Value::Null
        )
        .await
        .1,
        json!({"ok":true})
    );
}
#[tokio::test]
async fn quota_exhaustion_keeps_tombstones_and_refuses_admission() {
    let app = setup().await;
    let source = torrent(&app, Some(0));
    {
        let db = app.db.lock().unwrap();
        let transaction = db.unchecked_transaction().unwrap();
        for index in 0..REQUEST_QUOTA {
            transaction.execute("INSERT INTO playback_request_authority(session_id,request_id,scope,cancelled) VALUES('s1',?1,'scope',1)",[format!("cancelled{index}")]).unwrap();
        }
        transaction.commit().unwrap();
    }
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/v2/playback",
            body(&source, "capacity")
        )
        .await
        .1["error_code"],
        "playback_capacity"
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "DELETE",
            "/api/v2/playback-requests/cancelled0",
            Value::Null
        )
        .await
        .1,
        json!({"ok":true})
    );
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM playback_request_authority WHERE cancelled=1",
                [],
                |r| r.get::<_, usize>(0)
            )
            .unwrap(),
        REQUEST_QUOTA
    );
}
#[tokio::test]
async fn native_poll_and_heartbeat_revalidate_producer_and_terminal_responses_have_no_input() {
    let app = setup().await;
    let source = torrent(&app, Some(0));
    let first = ready(&app, body(&source, "revoke")).await;
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE addons SET enabled=0 WHERE id=1", [])
        .unwrap();
    let (_, value) = request(
        &app,
        "member-token-1",
        "GET",
        &format!("/api/v2/playback/{}", first["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(value["status"], "expired");
    assert!(value["delivery"].is_null());
    assert!(!value.to_string().contains("magnet:"));
}
#[tokio::test]
async fn extension_rejects_null_duplicates_wrong_platform_and_oversized_or_encoded_bodies() {
    let app = setup().await;
    let source = torrent(&app, Some(0));
    let valid = body(&source, "schema").to_string();
    let cases = [
        valid.replace("\"version\":1", "\"version\":1,\"version\":1"),
        valid.replace("\"version\":1", "\"version\":1.0"),
        valid.replace("android_tv", "web"),
        valid.replace("public_dht_tcp_v1", "unknown"),
    ];
    for input in cases {
        let response = crate::router(app.clone(), None)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v2/playback")
                    .header("authorization", "Bearer member-token-1")
                    .header("content-type", "application/json")
                    .body(Body::from(input))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    let mut null = body(&source, "null");
    null["client"]["native_torrent"] = Value::Null;
    assert_eq!(
        request(&app, "member-token-1", "POST", "/api/v2/playback", null)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    for (input, encoding) in [("x".repeat(16385), "identity"), (valid, "gzip")] {
        let response = crate::router(app.clone(), None)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v2/playback")
                    .header("authorization", "Bearer member-token-1")
                    .header("content-type", "application/json")
                    .header("content-encoding", encoding)
                    .body(Body::from(input))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn native_renewal_is_immutable_and_expired_authority_cannot_be_revived() {
    let app = setup().await;
    let source = torrent(&app, Some(2));
    let first = ready(&app, body(&source, "clock")).await;
    let id = first["id"].as_str().unwrap();
    let (status, refused) = request(
        &app,
        "member-token-1",
        "POST",
        &format!("/api/v2/playback/{id}/heartbeat"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(refused["error_code"], "invalid_playback_request");
    let (status, renewed) = request(
        &app,
        "member-token-1",
        "POST",
        &format!("/api/v2/playback/{id}/heartbeat"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let grant = &renewed["delivery"]["grant"];
    for field in ["id", "info_hash", "file_index", "network_policy", "input"] {
        assert_eq!(grant[field], first["delivery"]["grant"][field]);
    }
    assert_eq!(grant["expires_at"], renewed["expires_at"]);
    assert!(grant["expires_at"].as_u64().unwrap() - grant["server_time"].as_u64().unwrap() <= 60);
    {
        let entry = app
            .gateway_playbacks
            .snapshot(id, &lease())
            .unwrap_or_else(|_| panic!("playback fixture missing"));
        entry.state.lock().unwrap().touched = Instant::now() - Duration::from_secs(61);
    }
    assert_ne!(
        request(
            &app,
            "member-token-1",
            "POST",
            &format!("/api/v2/playback/{id}/heartbeat"),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, terminal) = request(
        &app,
        "member-token-1",
        "GET",
        &format!("/api/v2/playback/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(terminal["status"], "expired");
    assert!(terminal["delivery"].is_null());
    for _ in 0..2 {
        assert_eq!(
            request(
                &app,
                "member-token-1",
                "DELETE",
                &format!("/api/v2/playback/{id}"),
                Value::Null
            )
            .await
            .1,
            json!({"ok":true})
        );
    }
}

#[tokio::test]
async fn exact_episode_proof_requires_explicit_context_and_survives_discovery_without_source_io() {
    let app = setup().await;
    let source = torrent(&app, Some(0));
    app.streams
        .lock()
        .unwrap()
        .get_mut(&source)
        .unwrap()
        .exact_vod = None;
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/v2/playback",
            body(&source, "inexact")
        )
        .await
        .1["error_code"],
        "gateway_required"
    );
    let scoped = app.clone().with_lease(lease());
    let (cards,error)=scoped.register("addon:1",vec![json!({"url":"https://source.fixture.invalid/exact.torrent","infoHash":"0000000000000000000000000000000000000000","fileIdx":3})],"series");
    assert!(error.is_none());
    assert_eq!(cards.len(), 1);
    let id = cards[0]["id"].as_str().unwrap();
    assert_eq!(
        app.streams
            .lock()
            .unwrap()
            .get(id)
            .unwrap()
            .info_hash
            .as_deref(),
        Some("0000000000000000000000000000000000000000")
    );
    assert!(!cards[0].to_string().contains("exact.torrent"));
    assert!(!cards[0]
        .to_string()
        .contains("0000000000000000000000000000000000000000"));
}

#[tokio::test]
async fn children_negotiate_cancel_and_keep_duplicate_validation_while_title_policy_rechecks() {
    let app = setup().await;
    {
        let db = app.db.lock().unwrap();
        db.execute(
            "INSERT INTO kids_profiles(profile_id,enabled,max_age) VALUES(1,1,12)",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO kids_media(account_id,kind,id,parent_id,metadata,age,updated_at) VALUES(1,'series','series_fixture','series_fixture',?1,7,0)",
            [json!({"id":"series_fixture","videos":[{"id":"episode_fixture","season":1,"episode":2}]}).to_string()]).unwrap();
        db.execute("INSERT INTO kids_media(account_id,kind,id,parent_id,metadata,age,updated_at) VALUES(1,'series','episode_fixture','series_fixture',?1,7,0)",
            [json!({"id":"episode_fixture","season":1,"episode":2}).to_string()]).unwrap();
    }
    let (status, protocol) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v2/playback-protocol",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(protocol["native_torrent_versions"], json!([1]));
    let source = torrent(&app, Some(0));
    let duplicated = body(&source, "duplicate_child")
        .to_string()
        .replace("\"version\":1", "\"version\":1,\"version\":1");
    let response = crate::router(app.clone(), None)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v2/playback")
                .header("authorization", "Bearer member-token-1")
                .header("content-type", "application/json")
                .body(Body::from(duplicated))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let first = ready(&app, body(&source, "child")).await;
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "DELETE",
            "/api/v2/playback-requests/absent_child",
            Value::Null
        )
        .await
        .1,
        json!({"ok":true})
    );
    // Metadata denial can change without a profile revision bump; recheck the exact title.
    app.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE kids_media SET age=18 WHERE id='episode_fixture'",
            [],
        )
        .unwrap();
    let (status, refused) = request(
        &app,
        "member-token-1",
        "GET",
        &format!("/api/v2/playback/{}", first["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(!refused.to_string().contains("magnet:"));
}

#[tokio::test]
async fn starting_heartbeat_renews_authority_without_exposing_private_input() {
    let app = setup().await;
    let source = torrent(&app, Some(0));
    let first = ready(&app, body(&source, "preparing")).await;
    let id = first["id"].as_str().unwrap();
    let entry = app
        .gateway_playbacks
        .snapshot(id, &lease())
        .unwrap_or_else(|_| panic!("playback fixture missing"));
    {
        let mut state = entry.state.lock().unwrap();
        state.status = "starting";
        state.native.as_mut().unwrap().grant = None;
        state.native.as_mut().unwrap().expires_at = util::now() as u64 + 25;
    }
    let (status, renewed) = request(
        &app,
        "member-token-1",
        "POST",
        &format!("/api/v2/playback/{id}/heartbeat"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(renewed["status"], "starting");
    assert!(renewed["delivery"].is_null());
    assert!(renewed["expires_at"].as_u64().unwrap() > util::now() as u64 + 25);
    assert_eq!(renewed["renew_after_seconds"], 20);
}
#[tokio::test]
async fn failed_private_metainfo_preparation_settles_without_input_or_gateway_fallback() {
    let app = setup().await;
    let source = torrent(&app, Some(0));
    app.streams.lock().unwrap().get_mut(&source).unwrap().url =
        "http://127.0.0.1/private.torrent".into();
    let (status, admitted) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v2/playback",
        body(&source, "bad-metainfo"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let id = admitted["id"].as_str().unwrap();
    for _ in 0..100 {
        let (_, settled) = request(
            &app,
            "member-token-1",
            "GET",
            &format!("/api/v2/playback/{id}"),
            Value::Null,
        )
        .await;
        if settled["status"] != "starting" {
            assert_eq!(settled["status"], "failed");
            assert_eq!(settled["error_code"], "native_metainfo_invalid");
            assert!(settled["delivery"].is_null());
            assert!(!settled.to_string().contains("private.torrent"));
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("metainfo preparation did not settle");
}
