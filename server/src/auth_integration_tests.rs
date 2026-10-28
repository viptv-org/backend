//! Authorization regression tests, entirely in process with synthetic credentials.
use super::*;
use axum::{
    body::{to_bytes, Body},
    http::Request,
};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

use crate::test_support::request;

pub(crate) fn fixture() -> App {
    let mut a = crate::test_support::app();
    crate::test_support::configure_vault(&mut a);
    {
        let db = a.db.lock().unwrap();
        db.execute("DELETE FROM addons", []).unwrap();
        for id in 1..=2 {
            db.execute("INSERT INTO auth_accounts(id,username,password_hash,role,recovery_hash,created_at) VALUES(?1,?2,'unused','member','unused',0)",params![id,format!("member{id}")]).unwrap();
            let token = format!("member-token-{id}");
            let hash = format!("{:x}", Sha256::digest(token.as_bytes()));
            db.execute("INSERT INTO auth_sessions(id,account_id,access_hash,refresh_hash,csrf_hash,kind,device_name,access_expires,refresh_expires,created_at) VALUES(?1,?2,?3,?4,'unused','browser','test',?5,?5,0)",params![format!("s{id}"),id,hash,format!("refresh{id}"),util::now()+3600]).unwrap();
        }
        db.execute("INSERT INTO profiles(id,name,avatar_seed,presentation_complete) VALUES(1,'Owned','fixture-one',1)", []).unwrap();
        db.execute(
            "INSERT INTO profile_owners(profile_id,account_id,created_at) VALUES(1,1,0)",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO auth_profiles VALUES(1,1)", [])
            .unwrap();
        db.execute(
            "UPDATE auth_sessions SET profile_id=1 WHERE account_id=1",
            [],
        )
        .unwrap();
    }
    a
}
#[tokio::test]
async fn numeric_and_legacy_pairing_codes_support_qr_expiry_and_single_use() {
    let a = fixture();
    for (index, user_code) in ["00123456", "AB12CD34EF"].into_iter().enumerate() {
        let secret = format!("synthetic-device-secret-{index}");
        a.db.lock().unwrap().execute(
            "INSERT INTO auth_pairings(code_hash,device_hash,device_name,expires) VALUES(?1,?2,'Fixture TV',?3)",
            params![auth::hash(user_code), auth::hash(&secret), util::now() + 600],
        ).unwrap();
        let qr_path = format!("/api/auth/device/qr?code={user_code}");
        let qr = router(a.clone())
            .oneshot(
                Request::builder()
                    .uri(&qr_path)
                    .header(header::HOST, "tv.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(qr.status(), StatusCode::OK);
        assert_eq!(qr.headers()[header::CONTENT_TYPE], "image/png");
        assert_eq!(qr.headers()[header::CACHE_CONTROL], "no-store");
        assert!(to_bytes(qr.into_body(), 1_000_000)
            .await
            .unwrap()
            .starts_with(b"\x89PNG\r\n\x1a\n"));
        let payload = json!({"user_code":user_code});
        assert_eq!(
            request(
                &a,
                "invalid-token",
                "POST",
                "/api/device/lookup",
                payload.clone()
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
        let (status, lookup) = request(
            &a,
            "member-token-2",
            "POST",
            "/api/device/lookup",
            payload.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(lookup["user_code"], user_code);
        assert_eq!(
            request(
                &a,
                "member-token-2",
                "POST",
                "/api/device/approve",
                json!({"user_code":user_code,"account_id":1})
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            request(
                &a,
                "member-token-2",
                "POST",
                "/api/device/approve",
                payload.clone()
            )
            .await
            .0,
            StatusCode::OK
        );
        assert_eq!(
            request(
                &a,
                "",
                "POST",
                "/api/device/token",
                json!({"device_code":user_code})
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        let (status, grant) = request(
            &a,
            "",
            "POST",
            "/api/device/token",
            json!({"device_code":secret}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(grant["account_id"], 2);
        assert!(grant["profile_id"].is_null());
        assert_eq!(
            request(
                &a,
                "",
                "POST",
                "/api/device/token",
                json!({"device_code":secret})
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            request(&a, "member-token-2", "POST", "/api/device/approve", payload)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        let expired_qr = router(a.clone())
            .oneshot(
                Request::builder()
                    .uri(&qr_path)
                    .header(header::HOST, "tv.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(expired_qr.status(), StatusCode::NOT_FOUND);
    }
    a.db.lock().unwrap().execute("INSERT INTO auth_pairings(code_hash,device_hash,device_name,expires) VALUES(?1,?2,'Expired TV',?3)", params![auth::hash("00000002"),auth::hash("expired-secret"),util::now()-1]).unwrap();
    let expired_qr = router(a.clone())
        .oneshot(
            Request::builder()
                .uri("/api/auth/device/qr?code=00000002")
                .header(header::HOST, "tv.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(expired_qr.status(), StatusCode::NOT_FOUND);
    for endpoint in ["lookup", "approve"] {
        assert_eq!(
            request(
                &a,
                "member-token-2",
                "POST",
                &format!("/api/device/{endpoint}"),
                json!({"user_code":"00000002"})
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        request(
            &a,
            "",
            "POST",
            "/api/device/token",
            json!({"device_code":"expired-secret"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn numeric_pairing_lookup_guesses_are_rate_limited_per_account() {
    let a = fixture();
    for _ in 0..120 {
        assert_eq!(
            request(
                &a,
                "member-token-2",
                "POST",
                "/api/device/lookup",
                json!({"user_code":"00123456"})
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        request(
            &a,
            "member-token-2",
            "POST",
            "/api/device/lookup",
            json!({"user_code":"00000001"})
        )
        .await
        .0,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn public_pairing_qr_guesses_use_a_global_rate_limit() {
    let a = fixture();
    let make_request = || {
        Request::builder()
            .uri("/api/auth/device/qr?code=00123456")
            .header(header::HOST, "tv.example")
            .body(Body::empty())
            .unwrap()
    };
    assert_eq!(
        router(a.clone())
            .oneshot(make_request())
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    {
        let db = a.db.lock().unwrap();
        let count: i64 = db
            .query_row(
                "SELECT count FROM auth_limits WHERE bucket='auth:global:/auth/device/qr'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        db.execute(
            "UPDATE auth_limits SET count=600 WHERE bucket='auth:global:/auth/device/qr'",
            [],
        )
        .unwrap();
    }
    assert_eq!(
        router(a.clone())
            .oneshot(make_request())
            .await
            .unwrap()
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    a.db.lock().unwrap().execute("INSERT INTO auth_pairings(code_hash,device_hash,device_name,expires) VALUES(?1,?2,'Valid TV',?3)", params![auth::hash("00000001"),auth::hash("valid-secret"),util::now()+600]).unwrap();
    let valid_request = Request::builder()
        .uri("/api/auth/device/qr?code=00000001")
        .header(header::HOST, "tv.example")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        router(a.clone())
            .oneshot(valid_request)
            .await
            .unwrap()
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    a.db.lock()
        .unwrap()
        .execute(
            "UPDATE auth_limits SET expires=?1 WHERE bucket='auth:global:/auth/device/qr'",
            [util::now() - 1],
        )
        .unwrap();
    assert_eq!(
        router(a.clone())
            .oneshot(make_request())
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn profile_lists_are_filtered_and_new_profiles_are_granted_atomically() {
    let a = fixture();
    let (_, first) = request(&a, "member-token-1", "GET", "/api/profiles", Value::Null).await;
    assert_eq!(first["profiles"].as_array().unwrap().len(), 1);
    let (_, second) = request(&a, "member-token-2", "GET", "/api/profiles", Value::Null).await;
    assert_eq!(second["profiles"], json!([]));
    let (status, profile) = request(
        &a,
        "member-token-2",
        "POST",
        "/api/profiles",
        json!({"name":"Private"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = profile["id"]
        .as_i64()
        .or_else(|| profile["id"].as_str()?.parse().ok())
        .unwrap();
    let (_, second) = request(&a, "member-token-2", "GET", "/api/profiles", Value::Null).await;
    assert_eq!(second["profiles"], json!([profile]));
    a.db.lock()
        .unwrap()
        .execute(
            "UPDATE auth_sessions SET profile_id=?1 WHERE account_id=2",
            [id],
        )
        .unwrap();
    for suffix in ["favorites", "progress"] {
        let path = if suffix == "progress" {
            format!("/api/profiles/{id}/progress/page")
        } else {
            format!("/api/profiles/{id}/{suffix}")
        };
        assert_eq!(
            request(&a, "member-token-1", "GET", &path, Value::Null)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            request(&a, "member-token-2", "GET", &path, Value::Null)
                .await
                .0,
            StatusCode::OK
        );
    }
}
#[tokio::test]
async fn favorites_and_progress_require_the_exact_selected_profile() {
    let a = fixture();
    {
        let db = a.db.lock().unwrap();
        db.execute("INSERT INTO profiles(id,name,avatar_seed,presentation_complete) VALUES(2,'Also Owned','fixture-two',1)", []).unwrap();
        db.execute(
            "INSERT INTO profile_owners(profile_id,account_id,created_at) VALUES(2,1,0)",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO auth_profiles VALUES(1,2)", [])
            .unwrap();
    }

    // Profile selection and profile CRUD remain account-wide.
    let (status, profiles) =
        request(&a, "member-token-1", "GET", "/api/profiles", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(profiles["profiles"].as_array().unwrap().len(), 2);
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "PATCH",
            "/api/profiles/2",
            json!({"name":"Renamed B"}),
        )
        .await
        .0,
        StatusCode::OK
    );

    let favorite = json!({"id":"tt-favorite","type":"movie","name":"Favorite"});
    let progress = json!({
        "record": {
            "identity": {"id":"tt-progress","kind":"movie","ids":[],"episode":null,"edition":null},
            "provider_id": "fixture",
            "title": "Progress",
            "poster": null,
            "landscape": null,
            "position_millis": 12_000,
            "duration_millis": 120_000,
            "updated_at_millis": 0,
            "completed_at_millis": null
        },
        "ended": false
    });
    for (put_path, get_path, body, items_key) in [
        (
            "/api/profiles/1/favorites",
            "/api/profiles/1/favorites",
            favorite,
            None,
        ),
        (
            "/api/profiles/1/progress",
            "/api/profiles/1/progress/page",
            progress,
            Some("items"),
        ),
    ] {
        assert_eq!(
            request(&a, "member-token-1", "PUT", put_path, body.clone(),)
                .await
                .0,
            StatusCode::OK
        );
        let (status, selected_items) =
            request(&a, "member-token-1", "GET", get_path, Value::Null).await;
        assert_eq!(status, StatusCode::OK);
        let items = match items_key {
            Some(key) => selected_items[key].as_array().unwrap().clone(),
            None => selected_items.as_array().unwrap().clone(),
        };
        assert_eq!(items.len(), 1);

        let other_owned_put = put_path.replace("/profiles/1/", "/profiles/2/");
        let other_owned_get = get_path.replace("/profiles/1/", "/profiles/2/");
        assert_eq!(
            request(&a, "member-token-1", "GET", &other_owned_get, Value::Null,)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            request(&a, "member-token-1", "PUT", &other_owned_put, body,)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "PUT",
            "/api/profiles/1/progress",
            json!({"record":{"identity":{"id":"tt-invalid","kind":"movie","ids":[],"episode":null,"edition":null},"provider_id":"fixture","title":"Bad","poster":null,"landscape":null,"position_millis":-1,"duration_millis":100_000,"updated_at_millis":0,"completed_at_millis":null},"ended":false})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );

    let db = a.db.lock().unwrap();
    let other_favorites: i64 = db
        .query_row(
            "SELECT count(*) FROM favorites WHERE profile_id=2",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let other_progress: i64 = db
        .query_row(
            "SELECT count(*) FROM progress WHERE profile_id=2",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!((other_favorites, other_progress), (0, 0));
}
#[tokio::test]
async fn members_cannot_administer_providers_addons_or_other_resources() {
    let a = fixture();
    {
        let db = a.db.lock().unwrap();
        db.execute("INSERT INTO profiles(id,name,avatar_seed,presentation_complete) VALUES(2,'Second','fixture-two',1)", [])
            .unwrap();
        db.execute(
            "INSERT INTO profile_owners(profile_id,account_id,created_at) VALUES(2,2,0)",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO auth_profiles VALUES(2,2)", [])
            .unwrap();
        db.execute(
            "UPDATE auth_sessions SET profile_id=2 WHERE account_id=2",
            [],
        )
        .unwrap();
    }
    let mut owned = a.clone();
    owned.principal = Some(auth::Principal::Account {
        account_id: 1,
        role: "member".into(),
        profile_id: Some(1),
        session_id: Some("s1".into()),
    });
    owned.lease = Some(ResourceLease {
        policy_revision: 0,
        principal: owned.identity(),
        session_id: Some("s1".into()),
    });
    owned.own_resource("job", "private-job");
    for path in [
        "/api/streams/private-job",
        "/api/streams/private-job/events",
    ] {
        assert_eq!(
            request(&a, "member-token-2", "GET", path, Value::Null)
                .await
                .0,
            StatusCode::NOT_FOUND
        );
    }
}
#[tokio::test]
async fn profile_scope_is_frozen_into_resource_ownership_and_revocation_is_immediate() {
    let a = fixture();
    {
        let db = a.db.lock().unwrap();
        db.execute("INSERT INTO profiles(id,name,avatar_seed,presentation_complete) VALUES(2,'Second','fixture-two',1)", []).unwrap();
        db.execute(
            "INSERT INTO profile_owners(profile_id,account_id,created_at) VALUES(2,1,0)",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO auth_profiles VALUES(1,2)", [])
            .unwrap();
        db.execute(
            "UPDATE auth_sessions SET profile_id=1 WHERE account_id=1",
            [],
        )
        .unwrap();
    }
    let mut owned = a.clone();
    owned.principal = Some(auth::Principal::Account {
        account_id: 1,
        role: "member".into(),
        profile_id: Some(1),
        session_id: Some("s1".into()),
    });
    owned.lease = Some(ResourceLease {
        policy_revision: 0,
        principal: owned.identity(),
        session_id: Some("s1".into()),
    });
    owned.own_resource("job", "profile-job");
    owned.own_resource("playback", "profile-playback");
    let (cards, _) = owned.register(
        "addon:1",
        vec![json!({"url":"https://example.com/video.mp4"})],
        "movie",
    );
    a.jobs.lock().unwrap().insert(
        "profile-job".into(),
        Arc::new(Job {metadata_proof:None,
        source_slots:std::sync::atomic::AtomicUsize::new(0),
        cancelled: std::sync::atomic::AtomicBool::new(false),
        configuration_revision: None,
            media_id: None,
            exact_vod: None,
            kind: "movie".into(),
            created: Instant::now(),
            state: Mutex::new(JobState {
                events: vec![],
                pending: 0,
            }),
            notify: Notify::new(),
        }),
    );
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "GET",
            "/api/streams/profile-job",
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    a.db.lock()
        .unwrap()
        .execute(
            "UPDATE auth_sessions SET profile_id=2 WHERE account_id=1",
            [],
        )
        .unwrap();
    for (method, path) in [
        ("GET", "/api/streams/profile-job"),
        ("GET", "/api/streams/profile-job/events"),
        ("POST", "/api/playback/profile-playback/heartbeat"),
        ("DELETE", "/api/playback/profile-playback"),
    ] {
        assert_eq!(
            request(&a, "member-token-1", method, path, Value::Null)
                .await
                .0,
            if method == "DELETE" {
                // Bodyless release is idempotent and does not disclose another scope.
                StatusCode::OK
            } else {
                StatusCode::NOT_FOUND
            }
        );
    }
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "POST",
            "/api/playback",
            json!({"request_id":"private-scope","stream_id":cards[0]["id"],"client":{"platform":"android","can_play_direct":true,"max_width":1280,"max_height":720,"video_codecs":["h264"],"audio_codecs":["aac"]}})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    a.db.lock()
        .unwrap()
        .execute(
            "UPDATE auth_sessions SET profile_id=1 WHERE account_id=1",
            [],
        )
        .unwrap();
    a.db.lock()
        .unwrap()
        .execute(
            "DELETE FROM profile_owners WHERE account_id=1 AND profile_id=1",
            [],
        )
        .unwrap();
    for path in [
        "/api/streams/profile-job",
        "/api/profiles/1/favorites",
        "/api/profiles/1/progress",
    ] {
        let status = request(&a, "member-token-1", "GET", path, Value::Null)
            .await
            .0;
        assert!(status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN);
    }
}
#[tokio::test]
async fn owner_paired_device_has_no_administration_privileges() {
    let a = fixture();
    {
        let db = a.db.lock().unwrap();
        db.execute("UPDATE auth_accounts SET role='owner' WHERE id=1", [])
            .unwrap();
        db.execute(
            "UPDATE auth_sessions SET kind='device',profile_id=1 WHERE account_id=1",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO auth_device_profiles(session_id,profile_id) VALUES('s1',1)",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO profiles(id,name) VALUES(2,'Other')", [])
            .unwrap();
        db.execute("INSERT INTO auth_profiles VALUES(1,2)", [])
            .unwrap();
    }
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "GET",
            "/api/profiles/1/favorites",
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "GET",
            "/api/profiles/2/favorites",
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "POST",
            "/api/profiles",
            json!({"name":"Device profile"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "PATCH",
            "/api/profiles/1",
            json!({"name":"Changed"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "DELETE",
            "/api/profiles/1",
            Value::Null
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(&a, "member-token-1", "GET", "/api/auth/me", Value::Null)
            .await
            .1["can_create_profile"],
        true
    );
}
#[tokio::test]
async fn media_resource_lease_rechecks_grants_account_state_and_session_expiry() {
    let a = fixture();
    let db = a.db.lock().unwrap();
    db.execute(
        "UPDATE auth_sessions SET profile_id=1 WHERE account_id=1",
        [],
    )
    .unwrap();
    let lease = ResourceLease {
        policy_revision: 0,
        principal: auth::Principal::Account {
            account_id: 1,
            role: "member".into(),
            profile_id: Some(1),
            session_id: Some("s1".into()),
        },
        session_id: Some("s1".into()),
    };
    assert!(lease.validate(&db).is_ok());
    let mismatched_family = ResourceLease {
        policy_revision: 0,
        principal: lease.principal.clone(),
        session_id: Some("s2".into()),
    };
    assert!(mismatched_family.validate(&db).is_err());
    db.execute("DELETE FROM profile_owners WHERE account_id=1", [])
        .unwrap();
    assert!(lease.validate(&db).is_err());
    db.execute("UPDATE auth_accounts SET role='owner' WHERE id=1", [])
        .unwrap();
    db.execute("UPDATE auth_sessions SET kind='device' WHERE id='s1'", [])
        .unwrap();
    assert!(
        lease.validate(&db).is_err(),
        "An owner-bound device still needs the current grant"
    );
    db.execute(
        "INSERT INTO profile_owners(profile_id,account_id,created_at) VALUES(1,1,0)",
        [],
    )
    .unwrap();
    db.execute("UPDATE auth_accounts SET disabled=1 WHERE id=1", [])
        .unwrap();
    assert!(lease.validate(&db).is_err());
    db.execute("UPDATE auth_accounts SET disabled=0 WHERE id=1", [])
        .unwrap();
    db.execute(
        "UPDATE auth_sessions SET refresh_expires=0 WHERE id='s1'",
        [],
    )
    .unwrap();
    assert!(lease.validate(&db).is_err());
}
#[tokio::test]
async fn playback_ownership_cleanup_preserves_live_and_concurrently_created_sessions() {
    let a = fixture();
    let mut owned = a.clone();
    let principal = auth::Principal::Account {
        account_id: 1,
        role: "member".into(),
        profile_id: Some(1),
        session_id: Some("s1".into()),
    };
    owned.principal = Some(principal.clone());
    owned.lease = Some(ResourceLease {
        policy_revision: 0,
        principal,
        session_id: Some("s1".into()),
    });
    owned.own_resource("playback", "ended");
    owned.own_resource("playback", "still-live");
    owned.own_resource("job", "unrelated-job");
    let cutoff = Instant::now();
    owned.own_resource("playback", "created-during-snapshot");
    owned.retain_playback_owners(&HashSet::from(["still-live".to_owned()]), cutoff);
    assert!(a.resource_lease("playback", "ended").is_none());
    assert!(a.resource_lease("playback", "still-live").is_some());
    assert!(a
        .resource_lease("playback", "created-during-snapshot")
        .is_some());
    assert!(a.resource_lease("job", "unrelated-job").is_some());
    // The next snapshot can remove a session which never became active.
    a.retain_playback_owners(&HashSet::from(["still-live".to_owned()]), Instant::now());
    assert!(a
        .resource_lease("playback", "created-during-snapshot")
        .is_none());
}
#[tokio::test]
async fn stale_authenticated_profile_handlers_recheck_grants_before_db_access() {
    let a = fixture();
    let principal = auth::Principal::Account {
        account_id: 1,
        role: "member".into(),
        profile_id: Some(1),
        session_id: Some("s1".into()),
    };
    let request = a.clone().with_lease(ResourceLease {
        policy_revision: 0,
        principal,
        session_id: Some("s1".into()),
    });
    let item = json!({"id":"tt1","type":"movie","name":"Private","position":1,"duration":2});
    let _ = save_favorite(State(request.clone()), Path(1), axum::Json(item.clone()))
        .await
        .unwrap();
    let _ = save_progress(State(request.clone()), Path(1), axum::Json(item.clone()))
        .await
        .unwrap();
    // Simulate revocation after authentication/middleware but before the worker acquires DB.
    a.db.lock()
        .unwrap()
        .execute("DELETE FROM profile_owners WHERE account_id=1", [])
        .unwrap();
    assert_eq!(
        favorites(State(request.clone()), Path(1))
            .await
            .unwrap_err()
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        progress(State(request.clone()), Path(1))
            .await
            .unwrap_err()
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        save_favorite(State(request.clone()), Path(1), axum::Json(item.clone()))
            .await
            .unwrap_err()
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        save_progress(State(request.clone()), Path(1), axum::Json(item))
            .await
            .unwrap_err()
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        delete_favorite(State(request), Path((1, "movie".into(), "tt1".into())))
            .await
            .unwrap_err()
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        a.db.lock()
            .unwrap()
            .query_row("SELECT count(*) FROM favorites", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}
#[tokio::test]
async fn same_account_and_profile_other_session_cannot_reuse_owned_resources() {
    let a = fixture();
    let token_hash = format!("{:x}", Sha256::digest(b"other-session-token"));
    a.db.lock().unwrap().execute("INSERT INTO auth_sessions(id,account_id,access_hash,refresh_hash,csrf_hash,kind,device_name,access_expires,refresh_expires,created_at) VALUES('other-session',1,?1,'other-refresh','unused','browser','other',?2,?2,0)",params![token_hash,util::now()+3600]).unwrap();
    a.db.lock()
        .unwrap()
        .execute(
            "UPDATE auth_sessions SET profile_id=1 WHERE id='other-session'",
            [],
        )
        .unwrap();
    let owner = a.clone().with_lease(ResourceLease {
        policy_revision: 0,
        principal: auth::Principal::Account {
            account_id: 1,
            role: "member".into(),
            profile_id: Some(1),
            session_id: Some("s1".into()),
        },
        session_id: Some("s1".into()),
    });
    owner.own_resource("job", "family-job");
    owner.own_resource("playback", "family-playback");
    for (method, path) in [
        ("GET", "/api/streams/family-job"),
        ("GET", "/api/streams/family-job/events"),
        ("POST", "/api/playback/family-playback/heartbeat"),
        ("DELETE", "/api/playback/family-playback"),
    ] {
        assert_eq!(
            request(&a, "other-session-token", method, path, Value::Null)
                .await
                .0,
            if method == "DELETE" {
                // Bodyless release is idempotent and does not disclose another scope.
                StatusCode::OK
            } else {
                StatusCode::NOT_FOUND
            }
        );
    }
    let (cards, _) = owner.register(
        "addon:1",
        vec![json!({"url":"https://example.com/video.mp4"})],
        "movie",
    );
    assert_eq!(
        request(
            &a,
            "other-session-token",
            "POST",
            "/api/playback",
            json!({"request_id":"private-scope","stream_id":cards[0]["id"],"client":{"platform":"android","can_play_direct":true,"max_width":1280,"max_height":720,"video_codecs":["h264"],"audio_codecs":["aac"]}})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}
#[tokio::test]
async fn account_media_requires_selected_profile_and_ignores_profile_header() {
    let a = fixture();
    a.db.lock()
        .unwrap()
        .execute(
            "UPDATE auth_sessions SET profile_id=NULL WHERE account_id=1",
            [],
        )
        .unwrap();
    for (method, path) in [
        ("GET", "/api/catalogs"),
        ("GET", "/api/discover"),
        ("GET", "/api/meta/movie/tt1"),
        ("POST", "/api/streams"),
        ("GET", "/api/streams/job"),
        ("GET", "/api/profiles/1/favorites"),
        ("PUT", "/api/profiles/1/progress"),
    ] {
        let response = router(a.clone())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("authorization", "Bearer member-token-1")
                    .header("X-Profile-ID", "1")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
        let value: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(value["error_code"], "profile_required", "{path}");
    }
    assert_eq!(
        request(&a, "member-token-1", "GET", "/api/profiles", Value::Null)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "POST",
            "/api/profiles",
            json!({"name":"Another"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &a,
            "former-static-token-never-valid",
            "GET",
            "/api/catalogs",
            Value::Null
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
}
#[tokio::test]
async fn auth_routes_receive_authentication_but_status_is_public() {
    let a = fixture();
    assert_eq!(
        request(&a, "invalid", "GET", "/api/auth/status", Value::Null)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&a, "invalid", "GET", "/api/auth/me", Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(&a, "member-token-1", "GET", "/api/auth/me", Value::Null)
            .await
            .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn catalog_revision_is_private_profile_guarded_and_account_scoped() {
    let a = fixture();
    let path = "/api/catalogs/revision";
    assert_eq!(
        request(&a, "invalid", "GET", path, Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(&a, "member-token-2", "GET", path, Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let response = router(a.clone())
        .oneshot(
            Request::builder()
                .uri(path)
                .header("authorization", "Bearer member-token-1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    let baseline = body["revision"].as_str().unwrap();
    assert_eq!(baseline.len(), 32);
    assert!(baseline.bytes().all(|b| b.is_ascii_hexdigit()));
    a.db.lock().unwrap().execute("INSERT INTO addons(name,manifest_url,manifest,account_id) VALUES('Other','https://other.invalid/manifest.json','{}',2)", []).unwrap();
    assert_eq!(
        request(&a, "member-token-1", "GET", path, Value::Null)
            .await
            .1["revision"],
        baseline
    );
    a.db.lock().unwrap().execute("INSERT INTO addons(name,manifest_url,manifest,account_id) VALUES('Mine','https://mine.invalid/manifest.json','{}',1)", []).unwrap();
    assert_ne!(
        request(&a, "member-token-1", "GET", path, Value::Null)
            .await
            .1["revision"],
        baseline
    );
}

#[tokio::test]
async fn addons_are_account_shared_and_cross_account_mutations_are_isolated() {
    let a = fixture();
    {
        let db = a.db.lock().unwrap();
        for owner in 1..=2 {
            db.execute("INSERT INTO addons(id,account_id,name,manifest_url,manifest) VALUES(?1,?1,?2,'https://example.com/manifest.json',?3)",params![owner,format!("Account {owner}"),json!({"name":format!("Account {owner}"),"catalogs":[{"id":"top","type":"movie"}]}).to_string()]).unwrap();
        }
    }
    crate::test_support::encrypt_fixture_sources(&a);
    for owner in 1..=2 {
        let (status, list) = request(
            &a,
            &format!("member-token-{owner}"),
            "GET",
            "/api/addons",
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list["items"].as_array().unwrap().len(), 1);
        assert_eq!(list["items"][0]["id"], owner);
        assert!(list["items"][0]["priority"].is_null());
    }
    let (status, error) = request(
        &a,
        "member-token-1",
        "PATCH",
        "/api/addons/2",
        json!({"enabled":false}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error["error_code"], "addon_not_found");
    request(
        &a,
        "member-token-1",
        "DELETE",
        "/api/addons/2",
        Value::Null,
    )
    .await;
    let (_, other) = request(&a, "member-token-2", "GET", "/api/addons", Value::Null).await;
    assert_eq!(other["items"][0]["enabled"], true);
    let (_, catalogs) = request(&a, "member-token-1", "GET", "/api/catalogs", Value::Null).await;
    assert_eq!(catalogs.as_array().unwrap().len(), 1);
    assert_eq!(catalogs[0]["addon_id"], 1);
}

#[tokio::test]
async fn household_profile_deletion_protects_primary_and_revokes_selected_resources() {
    let a = fixture();
    let (status, _) = request(
        &a,
        "member-token-1",
        "DELETE",
        "/api/profiles/1",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (_, created) = request(
        &a,
        "member-token-1",
        "POST",
        "/api/profiles",
        json!({"name":"Temporary"}),
    )
    .await;
    let id = created["id"].as_str().unwrap();
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "POST",
            "/api/auth/profile",
            json!({"profile_id":id})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "PUT",
            &format!("/api/profiles/{id}/progress"),
            json!({"record":{"identity":{"id":"movie","kind":"movie","ids":[],"episode":null,"edition":null},"provider_id":"fixture","title":"Test","poster":null,"landscape":null,"position_millis":20_000,"duration_millis":100_000,"updated_at_millis":0,"completed_at_millis":null}})
        )
        .await
        .0,
        StatusCode::OK
    );
    let mut owned = a.clone();
    owned.principal = Some(auth::Principal::Account {
        account_id: 1,
        role: "member".into(),
        profile_id: Some(id.parse().unwrap()),
        session_id: Some("s1".into()),
    });
    owned.own_resource("playback", "deleted-profile-media");
    assert_eq!(
        request(
            &a,
            "member-token-2",
            "DELETE",
            &format!("/api/profiles/{id}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "DELETE",
            &format!("/api/profiles/{id}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&a, "member-token-1", "GET", "/api/auth/me", Value::Null)
            .await
            .1["profile_id"],
        Value::Null
    );
    assert_eq!(
        request(&a, "member-token-1", "GET", "/api/catalogs", Value::Null)
            .await
            .1["error_code"],
        "profile_required"
    );
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "GET",
            &format!("/api/profiles/{id}/progress/page"),
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert!(a
        .resource_lease("playback", "deleted-profile-media")
        .is_none());
    let (_, profiles) = request(&a, "member-token-1", "GET", "/api/profiles", Value::Null).await;
    assert_eq!(profiles["profiles"].as_array().unwrap().len(), 1);
    assert_eq!(profiles["profiles"][0]["is_primary"], true);
}

#[tokio::test]
async fn playback_preferences_are_scoped_validated_and_share_autoplay_setting() {
    let a = fixture();
    let (status, defaults) = request(
        &a,
        "member-token-1",
        "GET",
        "/api/profiles/1/preferences",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(defaults["preferences"]["subtitles_enabled"], true);
    assert_eq!(defaults["preferences"]["source_selection"], "ask");
    assert_eq!(defaults["revision"], 0);
    assert_eq!(
        request(
            &a,
            "member-token-2",
            "PUT",
            "/api/profiles/1/preferences",
            json!({"revision":0,"preferences":defaults["preferences"]})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let mut updated = defaults["preferences"].clone();
    updated["preferred_audio_language"] = json!("ja");
    updated["max_quality_height"] = json!(720);
    updated["subtitle_font_scale"] = json!(1.25);
    let (status, saved) = request(&a,"member-token-1","PUT","/api/profiles/1/preferences",json!({"revision":0,"preferences":updated})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["preferences"]["preferred_audio_language"], "ja");
    assert_eq!(saved["preferences"]["max_quality_height"], 720);
    assert_eq!(saved["revision"], 1);
    // A stale revision is rejected with a conflict.
    assert_eq!(
        request(&a,"member-token-1","PUT","/api/profiles/1/preferences",json!({"revision":0,"preferences":updated})).await.0,
        StatusCode::CONFLICT
    );
    // Values that fail validation are rejected.
    let mut invalid = updated.clone();
    invalid["max_quality_height"] = json!(123);
    assert_eq!(
        request(&a,"member-token-1","PUT","/api/profiles/1/preferences",json!({"revision":1,"preferences":invalid})).await.0,
        StatusCode::BAD_REQUEST
    );
    // Unknown fields are rejected.
    assert_eq!(
        request(&a,"member-token-1","PUT","/api/profiles/1/preferences",json!({"revision":1,"preferences":{"subtitles_enabled":true,"source_selection":"ask","autoplay":false}})).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "GET",
            "/api/profiles/1/preferences",
            Value::Null
        )
        .await
        .1,
        saved
    );
    // Subtitle mode and HDR preference round-trip; a sent mode decides
    // subtitles_enabled, and unknown values are rejected.
    assert_eq!(defaults["preferences"]["subtitle_mode"], "always");
    assert_eq!(defaults["preferences"]["hdr_preference"], "any");
    let mut modes = saved["preferences"].clone();
    modes["subtitle_mode"] = json!("foreign");
    modes["subtitles_enabled"] = json!(false);
    modes["hdr_preference"] = json!("prefer");
    let (status, foreign) = request(&a,"member-token-1","PUT","/api/profiles/1/preferences",json!({"revision":1,"preferences":modes})).await;
    assert_eq!(status, StatusCode::OK, "{foreign}");
    assert_eq!(foreign["preferences"]["subtitle_mode"], "foreign");
    assert_eq!(foreign["preferences"]["subtitles_enabled"], true);
    assert_eq!(foreign["preferences"]["hdr_preference"], "prefer");
    let (_, read_back) = request(&a,"member-token-1","GET","/api/profiles/1/preferences",Value::Null).await;
    assert_eq!(read_back, foreign);
    // An on/off-only client keeps foreign while subtitles stay on.
    let mut legacy_client = foreign["preferences"].clone();
    legacy_client.as_object_mut().unwrap().remove("subtitle_mode");
    let (status, kept) = request(&a,"member-token-1","PUT","/api/profiles/1/preferences",json!({"revision":2,"preferences":legacy_client})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(kept["preferences"]["subtitle_mode"], "foreign");
    for (key, value) in [("subtitle_mode", "sometimes"), ("hdr_preference", "maybe")] {
        let mut invalid = kept["preferences"].clone();
        invalid[key] = json!(value);
        assert_eq!(
            request(&a,"member-token-1","PUT","/api/profiles/1/preferences",json!({"revision":3,"preferences":invalid})).await.0,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{key}"
        );
    }
    // Rows stored before subtitle_mode derive it from subtitles_enabled.
    for (enabled, mode) in [(true, "always"), (false, "never")] {
        a.db.lock().unwrap().execute(
            "UPDATE engine_profile_preferences SET value=?1 WHERE profile_id=1",
            [json!({"preferred_audio_language":null,"preferred_subtitle_language":null,"subtitles_enabled":enabled,"source_selection":"ask","max_quality_height":null}).to_string()],
        ).unwrap();
        let (status, legacy) = request(&a,"member-token-1","GET","/api/profiles/1/preferences",Value::Null).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(legacy["preferences"]["subtitle_mode"], mode);
        assert_eq!(legacy["preferences"]["subtitles_enabled"], enabled);
        assert_eq!(legacy["preferences"]["hdr_preference"], "any");
    }
}

#[tokio::test]
async fn kids_policy_requires_parent_pin_and_blocks_unknown_content_and_exit() {
    let a = fixture();
    let (_, child) = request(
        &a,
        "member-token-1",
        "POST",
        "/api/profiles",
        json!({"name":"Kids"}),
    )
    .await;
    let id = child["id"].as_str().unwrap();
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "PUT",
            &format!("/api/profiles/{id}/kids"),
            json!({"enabled":true,"max_age":12})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "PUT",
            "/api/parent/pin",
            json!({"pin":"7248"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "PUT",
            &format!("/api/profiles/{id}/kids"),
            json!({"enabled":true,"max_age":12})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "POST",
            "/api/auth/profile",
            json!({"profile_id":id})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "POST",
            "/api/streams",
            json!({"type":"movie","id":"adult","name":"Kids cartoon","contentRating":"G"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "POST",
            "/api/auth/profile",
            json!({"profile_id":"1"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "PATCH",
            "/api/profiles/1",
            json!({"name":"Changed"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "POST",
            "/api/parent/unlock",
            json!({"pin":"1111"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "POST",
            "/api/parent/unlock",
            json!({"pin":"7248"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "POST",
            "/api/auth/profile",
            json!({"profile_id":"1"})
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn library_pages_and_atomic_toggle_are_profile_scoped() {
    let a = fixture();
    for id in ["c", "a", "b"] {
        let (status, _) = request(
            &a,
            "member-token-1",
            "PUT",
            "/api/profiles/1/favorites",
            json!({"id":id,"type":"movie","name":id}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, page) = request(
        &a,
        "member-token-1",
        "GET",
        "/api/profiles/1/favorites/page?limit=2&offset=0",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["items"][0]["id"], "a");
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    assert_eq!(page["next_offset"], 2);
    let (_, result) = request(
        &a,
        "member-token-1",
        "POST",
        "/api/profiles/1/favorites/toggle",
        json!({"id":"a","type":"movie","name":"a"}),
    )
    .await;
    assert_eq!(result["saved"], false);
    let (_, page) = request(
        &a,
        "member-token-1",
        "GET",
        "/api/profiles/1/favorites/page?limit=2&offset=0",
        Value::Null,
    )
    .await;
    assert_eq!(page["total"], 2);
    assert_eq!(page["items"][0]["id"], "b");
    assert!(page["next_offset"].is_null());
    assert_eq!(
        request(
            &a,
            "member-token-2",
            "GET",
            "/api/profiles/1/favorites/page",
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn library_corrections_update_history_and_continue_without_losing_source() {
    let a = fixture();
    let episode = json!({"record":{"identity":{"id":"opaque-episode","kind":"series","ids":[],"episode":{"order":"aired","season":2,"episode":3},"edition":null},"provider_id":"fixture","title":"Series","poster":null,"landscape":null,"position_millis":35_000,"duration_millis":100_000,"updated_at_millis":0,"completed_at_millis":null,"parent_identity":{"id":"opaque-series","kind":"series","ids":[],"episode":null,"edition":null}}});
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "PUT",
            "/api/profiles/1/progress",
            episode.clone()
        )
        .await
        .0,
        StatusCode::OK
    );
    let mut correction = json!({"id":"opaque-episode","type":"series","name":"Series","series_id":"opaque-series","season":2,"episode":3,"duration":100,"source_addon_id":"provider:1","source_fingerprint":"stable","action":"watched"});
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "PUT",
            "/api/profiles/1/progress/correct",
            correction.clone()
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, page) = request(
        &a,
        "member-token-1",
        "GET",
        "/api/profiles/1/progress/page?limit=20",
        Value::Null,
    )
    .await;
    assert_eq!(page["items"][0]["position"], 100.0);
    assert_eq!(page["items"][0]["series_id"], "opaque-series");
    assert_eq!(page["items"][0]["source_fingerprint"], "stable");
    correction["action"] = json!("unwatched");
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "PUT",
            "/api/profiles/1/progress/correct",
            correction.clone()
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, queue) = request(
        &a,
        "member-token-1",
        "GET",
        "/api/profiles/1/continue",
        Value::Null,
    )
    .await;
    assert_eq!(queue["entries"][0]["record"]["identity"]["id"], "opaque-episode");
    assert_eq!(queue["entries"][0]["record"]["position_millis"], 0);
    correction["action"] = json!("position");
    correction["position"] = json!(24.0);
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "PUT",
            "/api/profiles/1/progress/correct",
            correction.clone()
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, page) = request(
        &a,
        "member-token-1",
        "GET",
        "/api/profiles/1/progress/page?limit=20",
        Value::Null,
    )
    .await;
    assert_eq!(page["items"][0]["position"], 24.0);
    correction["position"] = json!(200.0);
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "PUT",
            "/api/profiles/1/progress/correct",
            correction.clone()
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            &a,
            "member-token-2",
            "PUT",
            "/api/profiles/1/progress/correct",
            correction
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn episode_history_is_scoped_to_the_open_series() {
    let a = fixture();
    for (id, series) in [("a:1", "a"), ("b:1", "b")] {
        assert_eq!(request(&a,"member-token-1","PUT","/api/profiles/1/progress",json!({"record":{"identity":{"id":id,"kind":"series","ids":[],"episode":{"order":"aired","season":1,"episode":1},"edition":null},"provider_id":"fixture","title":series,"poster":null,"landscape":null,"position_millis":20_000,"duration_millis":20_000,"updated_at_millis":0,"completed_at_millis":null,"parent_identity":{"id":series,"kind":"series","ids":[],"episode":null,"edition":null}}})).await.0,StatusCode::OK);
    }
    let (status, items) = request(
        &a,
        "member-token-1",
        "GET",
        "/api/profiles/1/progress/series?series_id=a",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(items.as_array().unwrap().len(), 1);
    assert_eq!(items[0]["id"], "a:1");
}

#[tokio::test]
async fn manual_watched_does_not_invent_an_episode_duration() {
    let a = fixture();
    let item = json!({"id":"unknown-duration","type":"series","name":"Show","series_id":"show","season":1,"episode":1,"action":"watched"});
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "PUT",
            "/api/profiles/1/progress/correct",
            item
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, history) = request(
        &a,
        "member-token-1",
        "GET",
        "/api/profiles/1/progress/page",
        Value::Null,
    )
    .await;
    assert_eq!(history["items"][0]["duration"], 0.0);
    assert_eq!(history["items"][0]["watched"], true);
    let (_, queue) = request(
        &a,
        "member-token-1",
        "GET",
        "/api/profiles/1/continue",
        Value::Null,
    )
    .await;
    assert_eq!(queue["entries"][0]["status"], "Pending");
    assert_eq!(request(&a,"member-token-1","PUT","/api/profiles/1/progress",json!({"record":{"identity":{"id":"unknown-duration","kind":"series","ids":[],"episode":{"order":"aired","season":1,"episode":1},"edition":null},"provider_id":"fixture","title":"Show","poster":null,"landscape":null,"position_millis":10_000,"duration_millis":100_000,"updated_at_millis":0,"completed_at_millis":null,"parent_identity":{"id":"show","kind":"series","ids":[],"episode":null,"edition":null}}})).await.0,StatusCode::OK);
    let (_, history) = request(
        &a,
        "member-token-1",
        "GET",
        "/api/profiles/1/progress/page",
        Value::Null,
    )
    .await;
    assert_eq!(history["items"][0]["watched"], false);
}

#[tokio::test]
async fn home_favorites_page_does_not_let_live_channels_hide_saved_movies() {
    let a = fixture();
    for (id, kind, name) in [
        ("channel", "live", "A channel"),
        ("movie", "movie", "Z movie"),
    ] {
        assert_eq!(
            request(
                &a,
                "member-token-1",
                "PUT",
                "/api/profiles/1/favorites",
                json!({"id":id,"type":kind,"name":name})
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    let (_, page) = request(
        &a,
        "member-token-1",
        "GET",
        "/api/profiles/1/favorites/page?limit=1&exclude_live=true",
        Value::Null,
    )
    .await;
    assert_eq!(page["items"][0]["id"], "movie");
    assert_eq!(page["total"], 1);
}

#[tokio::test]
async fn playback_after_a_correction_becomes_the_current_episode_immediately() {
    let a = fixture();
    for _ in 0..3 {
        assert_eq!(request(&a,"member-token-1","PUT","/api/profiles/1/progress/correct",json!({"id":"ep1","type":"series","name":"Show","series_id":"show","season":1,"episode":1,"action":"watched"})).await.0,StatusCode::OK);
    }
    assert_eq!(request(&a,"member-token-1","PUT","/api/profiles/1/progress",json!({"record":{"identity":{"id":"ep2","kind":"series","ids":[],"episode":{"order":"aired","season":1,"episode":2},"edition":null},"provider_id":"fixture","title":"Show","poster":null,"landscape":null,"position_millis":5_000,"duration_millis":100_000,"updated_at_millis":0,"completed_at_millis":null,"parent_identity":{"id":"show","kind":"series","ids":[],"episode":null,"edition":null}}})).await.0,StatusCode::OK);
    let (_, queue) = request(
        &a,
        "member-token-1",
        "GET",
        "/api/profiles/1/continue",
        Value::Null,
    )
    .await;
    assert_eq!(queue["entries"][0]["record"]["identity"]["id"], "ep2");
}

async fn kids_metadata_fixture() -> (App, tokio::task::JoinHandle<()>) {
    let mut a = fixture();
    a.addons.allow_test_loopback = true;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let service = Router::new().route(
        "/meta/:kind/:id",
        get(|Path((kind, id)): Path<(String, String)>| async move {
            let id = id.trim_end_matches(".json");
            let mut meta = json!({"id":id,"type":kind,"name":format!("Trusted {id}")});
            if id == "mixed" {
                meta["contentRating"] = json!("TV-Y");
                meta["videos"] = json!([
                    {"id":"mixed-safe","season":1,"episode":1,"title":"Allowed"},
                    {"id":"mixed-adult","season":1,"episode":2,"title":"Adult","contentRating":"TV-MA"},
                    {"id":"mixed-older","season":1,"episode":3,"title":"Older","contentRating":"TV-14"},
                    {"id":"mixed-conflict","season":1,"episode":4,"title":"Conflicting","contentRating":"TV-Y","certification":"R"},
                    {"id":"episode-one","season":1,"episode":5,"title":"Ambiguous"}
                ]);
            }
            if id == "large" {
                meta["contentRating"] = json!("TV-Y");
                meta["videos"] = json!((0..2000).map(|n|json!({"id":format!("large-{n}"),"season":1,"episode":n+1,"title":format!("Episode {n}"),"description":"x".repeat(512),"contentRating":"TV-Y"})).collect::<Vec<_>>());
            }
            if id == "safe" || id == "series" {
                meta["contentRating"] = json!("TV-Y");
            }
            if id == "adult" || id == "adultseries" {
                meta["contentRating"] = json!("TV-MA");
            }
            if id == "conflict" {
                meta["contentRating"] = json!("TV-Y");
                meta["certification"] = json!("R");
            }
            if id == "series" || id == "adultseries" {
                meta["videos"] =
                    json!([{"id":"episode-one","season":1,"episode":1,"title":"Episode one"}]);
            }
            axum::Json(json!({"meta":meta}))
        }),
    );
    let metas = move |axum::extract::Path((kind, _)): axum::extract::Path<(String, String)>| {
        let metas = if kind == "series" {
            json!({"metas":[{"id":"safe","type":"series","name":"Trusted safe series","contentRating":"TV-Y"},{"id":"unknown","type":"series","name":"Unknown series"},{"id":"adult","type":"series","name":"Adult series","contentRating":"TV-MA"}]})
        } else {
            json!({"metas":[{"id":"safe","type":"movie","name":"Trusted safe","contentRating":"TV-Y"},{"id":"unknown","type":"movie","name":"Unknown"},{"id":"adult","type":"movie","name":"Adult","contentRating":"TV-MA"}]})
        };
        async move { axum::Json(metas) }
    };
    let service = service
        .route("/catalog/:kind/:catalog/:extra", get(metas))
        .route("/catalog/:kind/:catalog", get(metas));
    let task = tokio::spawn(async move { axum::serve(listener, service).await.unwrap() });
    {
        let db = a.db.lock().unwrap();
        db.execute("INSERT INTO addons(account_id,name,manifest_url,manifest) VALUES(1,'Fixture',?1,?2)",params![format!("http://{address}/manifest.json"),json!({"id":"fixture","name":"Fixture","resources":["meta","catalog"],"types":["movie","series"],"catalogs":[{"id":"top","type":"movie"},{"id":"top-series","type":"series"},{"id":"search","type":"movie","extra":[{"name":"search","isRequired":true}]}]}).to_string()]).unwrap();
        let hash = format!("{:x}", Sha256::digest(b"parent-token"));
        db.execute("INSERT INTO auth_sessions(id,account_id,access_hash,refresh_hash,csrf_hash,profile_id,kind,device_name,access_expires,refresh_expires,created_at) VALUES('parent',1,?1,'parent-refresh','unused',1,'browser','parent',?2,?2,0)",params![hash,util::now()+3600]).unwrap();
    }
    crate::test_support::encrypt_fixture_sources(&a);
    (a, task)
}
#[tokio::test]
async fn kids_library_uses_trusted_ratings_approvals_and_exact_episode_membership() {
    let (a, task) = kids_metadata_fixture().await;
    let (_, child) = request(
        &a,
        "parent-token",
        "POST",
        "/api/profiles",
        json!({"name":"Kids"}),
    )
    .await;
    let id = child["id"].as_str().unwrap();
    assert_eq!(
        request(
            &a,
            "parent-token",
            "PUT",
            "/api/parent/pin",
            json!({"pin":"7248"})
        )
        .await
        .0,
        StatusCode::OK
    );
    for (kind, title) in [
        ("movie", "safe"),
        ("movie", "unknown"),
        ("movie", "conflict"),
        ("movie", "adult"),
        ("series", "series"),
    ] {
        assert_eq!(
            request(
                &a,
                "parent-token",
                "GET",
                &format!("/api/meta/{kind}/{title}"),
                Value::Null
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    request(
        &a,
        "member-token-1",
        "POST",
        "/api/auth/profile",
        json!({"profile_id":id}),
    )
    .await;
    // The parent's own browsing seeds the trust store with the catalog items.
    let (_, parent_discover) = request(
        &a,
        "parent-token",
        "GET",
        "/api/discover?type=movie",
        Value::Null,
    )
    .await;
    assert_eq!(parent_discover["cards"].as_array().unwrap().len(), 3);
    // Origin-scoped detail fetches seed the trust store for the child's
    // restricted catalog lookups: each card carries the stamped origin and is
    // replayed as the lookup expectation.
    for card in parent_discover["cards"].as_array().unwrap() {
        let id = card["identity"]["id"].as_str().unwrap();
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query.append_pair("card", &card.to_string());
        let (status, _) = request(
            &a,
            "parent-token",
            "GET",
            &format!("/api/meta/movie/{id}?{}", query.finish()),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    for title in ["safe", "adult", "unknown"] {
        request(
            &a,
            "member-token-1",
            "PUT",
            &format!("/api/profiles/{id}/favorites"),
            json!({"id":title,"type":"movie","name":"Untrusted label"}),
        )
        .await;
    }
    assert_eq!(
        request(
            &a,
            "parent-token",
            "PUT",
            &format!("/api/profiles/{id}/kids"),
            json!({"enabled":true,"max_age":12})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, discover) = request(
        &a,
        "member-token-1",
        "GET",
        "/api/discover?type=movie",
        Value::Null,
    )
    .await;
    assert_eq!(
        discover["cards"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["identity"]["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["safe"]
    );
    let (_, favorites) = request(
        &a,
        "member-token-1",
        "GET",
        &format!("/api/profiles/{id}/favorites/page?limit=1"),
        Value::Null,
    )
    .await;
    assert_eq!(favorites["total"], 1);
    assert_eq!(favorites["items"][0]["name"], "Trusted safe");
    assert!(favorites["next_offset"].is_null());
    for (path, body) in [
        (
            "/api/streams".to_string(),
            json!({"id":"adult","type":"movie","name":"safe","contentRating":"TV-Y"}),
        ),
        (
            "/api/streams".to_string(),
            json!({"id":"unknown-episode","type":"series","series_id":"series","season":1,"episode":1,"name":"safe"}),
        ),
        (
            format!("/api/profiles/{id}/progress/correct"),
            json!({"id":"unknown-episode","type":"series","series_id":"series","name":"safe","action":"watched"}),
        ),
    ] {
        let method = if path.ends_with("correct") {
            "PUT"
        } else {
            "POST"
        };
        assert_eq!(
            request(&a, "member-token-1", method, &path, body).await.0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "GET",
            "/api/meta/movie/adult",
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&a, "member-token-1", "GET", "/api/addons", Value::Null)
            .await
            .1["error_code"],
        "parent_required"
    );
    assert_eq!(
        request(&a, "member-token-1", "POST", "/api/auth/logout", json!({}))
            .await
            .1["error_code"],
        "parent_required"
    );
    assert_eq!(
        request(
            &a,
            "parent-token",
            "POST",
            &format!("/api/profiles/{id}/approvals"),
            json!({"id":"adult","type":"movie","approved":true,"contentRating":"G"})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let (_, unknown_card) = request(
        &a,
        "parent-token",
        "GET",
        "/api/meta/movie/unknown",
        Value::Null,
    )
    .await;
    assert_eq!(
        request(
            &a,
            "parent-token",
            "POST",
            &format!("/api/profiles/{id}/approvals"),
            json!({"id":"unknown","type":"movie","approved":true,"card":unknown_card["item"]})
        )
        .await
        .0,
        StatusCode::OK
    );
    let mut unknown_query = url::form_urlencoded::Serializer::new(String::new());
    unknown_query.append_pair(
        "metadata_origin",
        &unknown_card["item"]["metadata_origin"].to_string(),
    );
    unknown_query.append_pair("card", &unknown_card["item"].to_string());
    let (_, member_unknown) = request(
        &a,
        "member-token-1",
        "GET",
        &format!("/api/meta/movie/unknown?{}", unknown_query.finish()),
        Value::Null,
    )
    .await;
    assert_eq!(member_unknown["item"]["title"], "Trusted unknown");
    // A known episode's client-supplied parent/label is replaced, not trusted.
    assert_eq!(request(&a,"member-token-1","PUT",&format!("/api/profiles/{id}/progress/correct"),json!({"id":"episode-one","type":"series","series_id":"adultseries","name":"Adult spoof","action":"watched"})).await.0,StatusCode::OK);
    let (_, history) = request(
        &a,
        "member-token-1",
        "GET",
        &format!("/api/profiles/{id}/progress/series?series_id=series"),
        Value::Null,
    )
    .await;
    assert_eq!(history[0]["series_id"], "series");
    let (_, series_card) = request(
        &a,
        "parent-token",
        "GET",
        "/api/meta/series/series",
        Value::Null,
    )
    .await;
    assert_eq!(
        request(
            &a,
            "parent-token",
            "POST",
            &format!("/api/profiles/{id}/approvals"),
            json!({"id":"series","type":"series","approved":true,"card":series_card["item"]})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (status, old_job) = request(
        &a,
        "member-token-1",
        "POST",
        "/api/streams",
        json!({"id":"episode-one","type":"series"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // Same episode ID under a contradictory adult series is quarantined even if the old root was approved.
    assert_eq!(
        request(
            &a,
            "parent-token",
            "GET",
            "/api/meta/series/adultseries",
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "GET",
            &format!(
                "/api/streams/{}?after=0",
                old_job["id"].as_str().unwrap()
            ),
            Value::Null
        )
        .await
        .1["error_code"],
        "profile_policy_changed"
    );
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "POST",
            "/api/streams",
            json!({"id":"episode-one","type":"series"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "GET",
            &format!("/api/profiles/{id}/progress/page"),
            Value::Null
        )
        .await
        .1["total"],
        0
    );
    task.abort();
}

#[tokio::test]
async fn parent_pin_rotation_invalidates_grants_and_attempts_are_rate_limited() {
    let a = fixture();
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "PUT",
            "/api/parent/pin",
            json!({"pin":"7248"})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (unlock, rotate) = tokio::join!(
        request(
            &a,
            "member-token-1",
            "POST",
            "/api/parent/unlock",
            json!({"pin":"7248"})
        ),
        request(
            &a,
            "member-token-1",
            "PUT",
            "/api/parent/pin",
            json!({"pin":"9632","current_pin":"7248"})
        )
    );
    assert_eq!(rotate.0, StatusCode::OK);
    assert!([StatusCode::OK, StatusCode::FORBIDDEN].contains(&unlock.0));
    let (_, status) = request(
        &a,
        "member-token-1",
        "GET",
        "/api/parent/status",
        Value::Null,
    )
    .await;
    assert_eq!(status["unlocked"], false);
    assert_eq!(status["pin_configured"], true);
    assert!(!status.to_string().contains("argon2"));
    assert!(!status.to_string().contains("9632"));
    assert_eq!(
        request(
            &a,
            "member-token-2",
            "POST",
            "/api/parent/unlock",
            json!({"pin":"9632"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    for _ in 0..5 {
        assert_eq!(
            request(
                &a,
                "member-token-1",
                "POST",
                "/api/parent/unlock",
                json!({"pin":"0000"})
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "POST",
            "/api/parent/unlock",
            json!({"pin":"9632"})
        )
        .await
        .0,
        StatusCode::TOO_MANY_REQUESTS
    );
}
#[tokio::test]
async fn kids_policy_changes_revoke_already_issued_resources() {
    let (a, task) = kids_metadata_fixture().await;
    let (_, child) = request(
        &a,
        "parent-token",
        "POST",
        "/api/profiles",
        json!({"name":"Kids"}),
    )
    .await;
    let id = child["id"].as_str().unwrap();
    request(
        &a,
        "parent-token",
        "PUT",
        "/api/parent/pin",
        json!({"pin":"7248"}),
    )
    .await;
    request(
        &a,
        "parent-token",
        "GET",
        "/api/meta/movie/safe",
        Value::Null,
    )
    .await;
    request(
        &a,
        "parent-token",
        "PUT",
        &format!("/api/profiles/{id}/kids"),
        json!({"enabled":true,"max_age":12}),
    )
    .await;
    request(
        &a,
        "member-token-1",
        "POST",
        "/api/auth/profile",
        json!({"profile_id":id}),
    )
    .await;
    let observed = a.db.lock().unwrap().query_row("SELECT age,conflict FROM kids_media WHERE account_id=1 AND kind='movie' AND id='safe'",[],|row|Ok((row.get::<_,Option<i64>>(0)?,row.get::<_,bool>(1)?))).optional().unwrap();
    assert_eq!(observed,Some((Some(0),false)),"the parent metadata observation must seed the legacy allowed-title proof");
    let (status, job) = request(
        &a,
        "member-token-1",
        "POST",
        "/api/streams",
        json!({"type":"movie","id":"safe"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{job}");
    let job_id = job["id"].as_str().unwrap();
    request(
        &a,
        "parent-token",
        "PUT",
        &format!("/api/profiles/{id}/kids"),
        json!({"enabled":true,"max_age":7}),
    )
    .await;
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "GET",
            &format!("/api/streams/{job_id}?after=0"),
            Value::Null
        )
        .await
        .1["error_code"],
        "profile_policy_changed"
    );
    task.abort();
}

#[tokio::test]
async fn kids_large_series_preserves_parent_response_and_exact_bounded_episode_membership() {
    let (a, task) = kids_metadata_fixture().await;
    let (status, original) = request(
        &a,
        "parent-token",
        "GET",
        "/api/meta/series/large",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // Detail episodes are served page-by-page; the full membership bound is
    // enforced by the progress checks below.
    assert_eq!(original["episodes"].as_array().unwrap().len(), 24);
    assert_eq!(
        original["episodes"][23]["description"]
            .as_str()
            .unwrap()
            .len(),
        512
    );
    // Replay the parent's card so the scoped trust store observes the series.
    let mut scoped_query = url::form_urlencoded::Serializer::new(String::new());
    scoped_query.append_pair("card", &original["item"].to_string());
    let (status, _) = request(
        &a,
        "parent-token",
        "GET",
        &format!("/api/meta/series/large?{}", scoped_query.finish()),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    request(
        &a,
        "parent-token",
        "PUT",
        "/api/parent/pin",
        json!({"pin":"7248"}),
    )
    .await;
    let (_, child) = request(
        &a,
        "parent-token",
        "POST",
        "/api/profiles",
        json!({"name":"Small"}),
    )
    .await;
    let id = child["id"].as_str().unwrap();
    assert_eq!(
        request(
            &a,
            "parent-token",
            "PUT",
            &format!("/api/profiles/{id}/kids"),
            json!({"enabled":true,"max_age":7})
        )
        .await
        .0,
        StatusCode::OK
    );
    request(
        &a,
        "member-token-1",
        "POST",
        "/api/auth/profile",
        json!({"profile_id":id}),
    )
    .await;
    // The parent's series browsing seeds the scoped trust store before
    // restriction: each discovered card is replayed as a detail lookup.
    let (status, parent_series) = request(
        &a,
        "parent-token",
        "GET",
        "/api/discover?type=series",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(parent_series["cards"].as_array().unwrap().len(), 3);
    for card in parent_series["cards"].as_array().unwrap() {
        let id = card["identity"]["id"].as_str().unwrap();
        let kind = card["identity"]["kind"].as_str().unwrap();
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query.append_pair("card", &card.to_string());
        let (status, _) = request(
            &a,
            "parent-token",
            "GET",
            &format!("/api/meta/{kind}/{id}?{}", query.finish()),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, page) = request(
        &a,
        "member-token-1",
        "GET",
        "/api/discover?type=series",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["cards"].as_array().unwrap().len(), 1);
    assert!(page["cards"][0]["parent_identity"].is_null());
    let mut large_query = url::form_urlencoded::Serializer::new(String::new());
    large_query.append_pair(
        "metadata_origin",
        &original["item"]["metadata_origin"].to_string(),
    );
    large_query.append_pair("card", &original["item"].to_string());
    let (status, meta) = request(
        &a,
        "member-token-1",
        "GET",
        &format!("/api/meta/series/large?{}", large_query.finish()),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(meta["episodes"].as_array().unwrap().len(), 24);
    // Restricted detail keeps the provider descriptions; per-episode safety
    // is enforced by the scoped approval checks, not by content stripping.
    assert!(meta["episodes"][0]["description"].is_string());
    // A missing episode no longer exists in any scoped catalog: the lookup
    // ends in a metadata conflict instead of a kids rejection.
    for (episode, expected) in [
        ("large-1999", StatusCode::OK),
        ("large-2000", StatusCode::CONFLICT),
    ] {
        assert_eq!(request(&a,"member-token-1","PUT",&format!("/api/profiles/{id}/progress"),json!({"record":{"identity":{"id":episode,"kind":"series","ids":[],"episode":{"order":"aired","season":1,"episode":2000},"edition":null},"provider_id":"stremio","title":episode,"poster":null,"landscape":null,"position_millis":1_000,"duration_millis":100_000,"updated_at_millis":0,"completed_at_millis":null,"metadata_origin":original["item"]["metadata_origin"],"parent_identity":{"id":"large","kind":"series","ids":[],"episode":null,"edition":null}}})).await.0,expected);
    }
    task.abort();
}

#[tokio::test]
async fn parent_title_search_uses_server_ids_and_requires_parent_authority() {
    let (a, task) = kids_metadata_fixture().await;
    let (status, result) = request(
        &a,
        "parent-token",
        "GET",
        "/api/parent/search?query=Trusted",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let rows = result["rows"].as_array().unwrap();
    let addon_row = rows
        .iter()
        .find(|row| row["addon_id"] == 1)
        .expect("addon search row");
    let items = addon_row["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["card"]["identity"]["id"], "safe");
    // Every row flags whether the result can be approved by the parent.
    assert!(items[0]["approval_supported"].is_boolean());
    // An empty query is rejected.
    assert_eq!(
        request(
            &a,
            "parent-token",
            "GET",
            "/api/parent/search?query=",
            Value::Null
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    request(
        &a,
        "parent-token",
        "PUT",
        "/api/parent/pin",
        json!({"pin":"7248"}),
    )
    .await;
    request(
        &a,
        "parent-token",
        "PUT",
        "/api/profiles/1/kids",
        json!({"enabled":true,"max_age":7}),
    )
    .await;
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "GET",
            "/api/parent/search?query=Trusted",
            Value::Null,
        )
        .await
        .1["error_code"],
        "parent_required"
    );
    task.abort();
}

#[tokio::test]
async fn kids_mixed_series_details_omit_every_episode_disallowed_by_sources() {
    let (a, task) = kids_metadata_fixture().await;
    let mut parent_cards = std::collections::HashMap::new();
    for title in ["series", "mixed"] {
        let (status, details) = request(
            &a,
            "parent-token",
            "GET",
            &format!("/api/meta/series/{title}"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        parent_cards.insert(title.to_string(), details["item"].clone());
    }
    let (status, pin) = request(
        &a,
        "parent-token",
        "PUT",
        "/api/parent/pin",
        json!({"pin":"7248"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "set parent PIN: {pin}");
    let (status, child) = request(
        &a,
        "parent-token",
        "POST",
        "/api/profiles",
        json!({"name":"Small"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create child profile: {child}");
    let id = child["id"].as_str().unwrap();
    let (status, kids) = request(
        &a,
        "parent-token",
        "PUT",
        &format!("/api/profiles/{id}/kids"),
        json!({"enabled":true,"max_age":7}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "enable kids profile: {kids}");
    let (status, selected) = request(
        &a,
        "member-token-1",
        "POST",
        "/api/auth/profile",
        json!({"profile_id":id}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "select child profile: {selected}");
    // The parent's own scoped lookups seed the per-origin observations the
    // restricted detail lookup requires.
    for title in ["series", "mixed"] {
        let card = parent_cards.get(title).unwrap();
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query.append_pair("card", &card.to_string());
        let (status, _) = request(
            &a,
            "parent-token",
            "GET",
            &format!("/api/meta/series/{title}?{}", query.finish()),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    let mixed_card = parent_cards.get("mixed").unwrap();
    let mut mixed_query = url::form_urlencoded::Serializer::new(String::new());
    mixed_query.append_pair(
        "metadata_origin",
        &mixed_card["metadata_origin"].to_string(),
    );
    mixed_query.append_pair("card", &mixed_card.to_string());
    let (status, details) = request(
        &a,
        "member-token-1",
        "GET",
        &format!("/api/meta/series/mixed?{}", mixed_query.finish()),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        details["episodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["identity"]["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["mixed-safe"]
    );
    for episode in [
        "mixed-adult",
        "mixed-older",
        "episode-one",
    ] {
        assert_eq!(
            request(
                &a,
                "member-token-1",
                "POST",
                "/api/streams",
                json!({"type":"series","id":episode,"series_id":"mixed"})
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    // A conflicting provider certification is resolved to the content rating
    // when the card is projected, so that episode is no longer withheld.
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "POST",
            "/api/streams",
            json!({"type":"series","id":"mixed-conflict","series_id":"mixed"})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, original) = request(
        &a,
        "parent-token",
        "GET",
        "/api/meta/series/mixed",
        Value::Null,
    )
    .await;
    assert_eq!(original["episodes"].as_array().unwrap().len(), 5);
    task.abort();
}

#[tokio::test]
async fn parent_approval_cap_is_idempotent_and_legacy_overflow_remains_revocable() {
    let (a, task) = kids_metadata_fixture().await;
    {
        let mut db = a.db.lock().unwrap();
        let tx = db.transaction().unwrap();
        for n in 0..501 {
            tx.execute(
                "INSERT INTO kids_approvals(profile_id,kind,id) VALUES(1,'movie',?1)",
                [format!("title-{n:03}")],
            )
            .unwrap();
        }
        tx.commit().unwrap();
    }
    let (_, first) = request(
        &a,
        "parent-token",
        "GET",
        "/api/profiles/1/approvals",
        Value::Null,
    )
    .await;
    assert_eq!(first["items"].as_array().unwrap().len(), 500);
    let (_, last) = request(
        &a,
        "parent-token",
        "GET",
        "/api/profiles/1/approvals?offset=500",
        Value::Null,
    )
    .await;
    assert_eq!(last["items"].as_array().unwrap().len(), 1);
    assert_eq!(last["items"][0]["id"], "title-500");
    assert_eq!(
        request(
            &a,
            "parent-token",
            "POST",
            "/api/profiles/1/approvals",
            json!({"id":"title-500","type":"movie","approved":false})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &a,
            "parent-token",
            "POST",
            "/api/profiles/1/approvals",
            // Opaque provider titles now require a scoped selection, so the
            // legacy cap overflow surfaces as an unscoped rejection.
            json!({"id":"new-title","type":"movie","approved":true})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    // Re-granting a legacy unscoped approval is no longer possible without a
    // scoped title selection; only revocation stays available for old rows.
    assert_eq!(
        request(
            &a,
            "parent-token",
            "POST",
            "/api/profiles/1/approvals",
            json!({"id":"title-000","type":"movie","approved":true})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    task.abort();
}

#[tokio::test]
async fn device_profile_management_preserves_pairing_and_household_boundaries() {
    let a = fixture();
    a.db.lock()
        .unwrap()
        .execute("UPDATE auth_sessions SET kind='device' WHERE id='s1'", [])
        .unwrap();
    let (status, created) = request(
        &a,
        "member-token-1",
        "POST",
        "/api/profiles",
        json!({"name":"TV profile","avatar_id":"zayne"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = created["id"].as_str().unwrap();
    let (_, me) = request(&a, "member-token-1", "GET", "/api/auth/me", Value::Null).await;
    assert_eq!(me["can_create_profile"], true);
    assert_eq!(me["can_manage_profiles"], true);
    assert_eq!(me["capabilities"]["manage_profiles"], true);
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "DELETE",
            "/api/profiles/1",
            Value::Null
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (_, foreign) = request(
        &a,
        "member-token-2",
        "POST",
        "/api/profiles",
        json!({"name":"Other household"}),
    )
    .await;
    let foreign_id = foreign["id"].as_str().unwrap();
    for method in ["PATCH", "DELETE"] {
        assert_eq!(
            request(
                &a,
                "member-token-1",
                method,
                &format!("/api/profiles/{foreign_id}"),
                json!({"name":"Forbidden"})
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "GET",
            "/api/auth/accounts",
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "PUT",
            "/api/parent/pin",
            json!({"pin":"7248"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (_, edited) = request(
        &a,
        "member-token-1",
        "PATCH",
        &format!("/api/profiles/{id}"),
        json!({"name":"Renamed","avatar_id":"initial-pink"}),
    )
    .await;
    assert_eq!(edited["id"], created["id"]);
    assert_eq!(edited["name"], "Renamed");
    assert_eq!(edited["avatar_id"], "initial-pink");
    let (status, catalog) = request(
        &a,
        "member-token-1",
        "PATCH",
        &format!("/api/profiles/{id}"),
        json!({"avatar_style":"animal-friends","avatar_choice":12}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{catalog}");
    assert_eq!(catalog["avatar_style"], "animal-friends");
    assert_eq!(catalog["avatar_choice"], 12);
    assert_eq!(catalog["avatar_id"], Value::Null);
    let (_, me) = request(&a, "member-token-1", "GET", "/api/auth/me", Value::Null).await;
    let account = me["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == created["id"])
        .unwrap();
    assert_eq!(account["avatar_url"], catalog["avatar_url"]);
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "POST",
            "/api/auth/profile",
            json!({"profile_id":id})
        )
        .await
        .0,
        StatusCode::OK
    );
    let mut selected = a.clone();
    selected.principal = Some(auth::Principal::Account {
        account_id: 1,
        role: "device".into(),
        profile_id: Some(id.parse().unwrap()),
        session_id: Some("s1".into()),
    });
    selected.own_resource("playback", "removed-device-profile");
    let stale = selected.request_lease();
    let mut other = a.clone();
    other.principal = Some(auth::Principal::Account {
        account_id: 2,
        role: "member".into(),
        profile_id: None,
        session_id: Some("s2".into()),
    });
    other.own_resource("playback", "other-viewer");
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "DELETE",
            &format!("/api/profiles/{id}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(stale.validate(&a.db.lock().unwrap()).is_err());
    assert!(a
        .resource_lease("playback", "removed-device-profile")
        .is_none());
    assert!(a.resource_lease("playback", "other-viewer").is_some());
    let (status, me) = request(&a, "member-token-1", "GET", "/api/auth/me", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert!(me["profile_id"].is_null());
    assert_eq!(me["role"], "device");
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "POST",
            "/api/auth/profile",
            json!({"profile_id":"1"})
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn device_profile_mutations_require_live_parent_grant_and_lease() {
    let a = fixture();
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "PUT",
            "/api/parent/pin",
            json!({"pin":"7248"})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, child) = request(
        &a,
        "member-token-1",
        "POST",
        "/api/profiles",
        json!({"name":"Kids"}),
    )
    .await;
    let id = child["id"].as_str().unwrap();
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "PUT",
            &format!("/api/profiles/{id}/kids"),
            json!({"enabled":true,"max_age":7})
        )
        .await
        .0,
        StatusCode::OK
    );
    a.db.lock()
        .unwrap()
        .execute(
            "UPDATE auth_sessions SET kind='device',profile_id=?1 WHERE id='s1'",
            [id],
        )
        .unwrap();
    for (method, path, body) in [
        (
            "POST",
            "/api/profiles".to_string(),
            json!({"name":"Escape"}),
        ),
        (
            "PATCH",
            "/api/profiles/1".to_string(),
            json!({"name":"Escape"}),
        ),
        ("DELETE", format!("/api/profiles/{id}"), Value::Null),
    ] {
        let (status, body) = request(&a, "member-token-1", method, &path, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error_code"], "parent_required");
    }
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "POST",
            "/api/parent/unlock",
            json!({"pin":"7248"})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (status, created) = request(
        &a,
        "member-token-1",
        "POST",
        "/api/profiles",
        json!({"name":"Parent created"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let target = created["id"].as_str().unwrap();
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "PATCH",
            &format!("/api/profiles/{target}"),
            json!({"name":"Parent edited"})
        )
        .await
        .0,
        StatusCode::OK
    );
    // Capture a valid request before the PIN expires; the handler must recheck it.
    let mut scoped = a.clone();
    scoped.principal = Some(auth::Principal::Account {
        account_id: 1,
        role: "device".into(),
        profile_id: Some(id.parse().unwrap()),
        session_id: Some("s1".into()),
    });
    let lease = scoped.request_lease();
    let scoped = scoped.with_lease(lease);
    a.db.lock()
        .unwrap()
        .execute("UPDATE parent_grants SET expires=0", [])
        .unwrap();
    assert!(
        create_profile(State(scoped.clone()), axum::Json(json!({"name":"Expired"})))
            .await
            .is_err()
    );
    assert!(update_profile(
        State(scoped.clone()),
        Path(target.parse().unwrap()),
        axum::Json(json!({"name":"Expired"}))
    )
    .await
    .is_err());
    assert!(delete_profile_authenticated(
        State(a.clone()),
        Extension(scoped.request_lease()),
        Path(target.parse().unwrap())
    )
    .await
    .is_err());
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "POST",
            "/api/parent/unlock",
            json!({"pin":"7248"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "DELETE",
            &format!("/api/profiles/{target}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    a.db.lock()
        .unwrap()
        .execute("UPDATE auth_sessions SET profile_id=1 WHERE id='s1'", [])
        .unwrap();
    assert!(
        create_profile(State(scoped.clone()), axum::Json(json!({"name":"Stale"})))
            .await
            .is_err()
    );
    assert!(update_profile(
        State(scoped.clone()),
        Path(1),
        axum::Json(json!({"name":"Stale"}))
    )
    .await
    .is_err());
    assert!(delete_profile_authenticated(
        State(a.clone()),
        Extension(scoped.request_lease()),
        Path(id.parse().unwrap())
    )
    .await
    .is_err());
}


#[tokio::test]
async fn device_token_poll_reports_denied_and_expired_codes_on_the_wire() {
    let a = fixture();
    for (user_code, secret, expires) in [
        ("00000011", "denied-device-secret", util::now() + 600),
        ("00000012", "expired-device-secret", util::now() - 1),
    ] {
        a.db.lock().unwrap().execute(
            "INSERT INTO auth_pairings(code_hash,device_hash,device_name,expires) VALUES(?1,?2,'Wire TV',?3)",
            params![auth::hash(user_code), auth::hash(secret), expires],
        ).unwrap();
    }
    let poll = |secret: &'static str| {
        let a = a.clone();
        async move {
            request(
                &a,
                "",
                "POST",
                "/api/auth/device/token",
                json!({"device_code":secret}),
            )
            .await
        }
    };
    let (status, body) = poll("denied-device-secret").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "authorization_pending");
    assert_eq!(body["error_code"], "authorization_pending");
    assert_eq!(
        request(
            &a,
            "member-token-2",
            "POST",
            "/api/auth/device/deny",
            json!({"user_code":"00000011"})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (status, body) = poll("denied-device-secret").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "access_denied");
    assert_eq!(body["error_code"], "access_denied");
    let (status, body) = poll("expired-device-secret").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "expired_token");
    assert_eq!(body["error_code"], "expired_token");
}

/// The parent-PIN hashing budget belongs to one app instance. A saturated
/// budget in one app must not answer 429 for another app in the same process
/// (#68: parallel fixtures used to queue on one process-wide gate and start
/// failing setup calls, which left kids policy disabled).
#[tokio::test]
async fn parent_pin_cpu_budget_is_scoped_to_each_app() {
    let busy = fixture();
    let other = fixture();
    // Hold every permit of the first app's budget: its own PIN work now waits.
    let held = busy
        .pin_cpu_gate
        .clone()
        .acquire_many_owned(kids::PIN_CPU_PERMITS as u32)
        .await
        .expect("PIN budget permits");
    // An exhausted budget refuses further PIN work instead of queueing it.
    let (status, body) = request(
        &busy,
        "member-token-1",
        "PUT",
        "/api/parent/pin",
        json!({"pin":"7248"}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "exhausted PIN budget must refuse: {body}"
    );
    // That budget is the first app's own: an app never refuses another app's
    // PIN work, however busy its fixtures are (#68).
    let (status, body) = request(
        &other,
        "member-token-1",
        "PUT",
        "/api/parent/pin",
        json!({"pin":"7248"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "second app must save its PIN: {body}");
    drop(held);
}
