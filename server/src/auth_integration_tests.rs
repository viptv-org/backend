//! Authorization regression tests, entirely in process with synthetic credentials.
use super::*;
use axum::{
    body::{to_bytes, Body},
    http::Request,
};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

use crate::test_support::request;

fn fixture() -> App {
    let a = crate::test_support::app();
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
async fn profile_lists_are_filtered_and_new_profiles_are_granted_atomically() {
    let a = fixture();
    let (_, first) = request(&a, "member-token-1", "GET", "/api/profiles", Value::Null).await;
    assert_eq!(first.as_array().unwrap().len(), 1);
    let (_, second) = request(&a, "member-token-2", "GET", "/api/profiles", Value::Null).await;
    assert_eq!(second, json!([]));
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
    assert_eq!(second, json!([profile]));
    a.db.lock()
        .unwrap()
        .execute(
            "UPDATE auth_sessions SET profile_id=?1 WHERE account_id=2",
            [id],
        )
        .unwrap();
    for suffix in ["favorites", "progress"] {
        let path = format!("/api/profiles/{id}/{suffix}");
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
    assert_eq!(profiles.as_array().unwrap().len(), 2);
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
        "id":"tt-progress",
        "type":"movie",
        "name":"Progress",
        "position":12.0,
        "duration":120.0
    });
    for (suffix, body) in [("favorites", favorite), ("progress", progress)] {
        let selected_path = format!("/api/profiles/1/{suffix}");
        assert_eq!(
            request(&a, "member-token-1", "PUT", &selected_path, body.clone(),)
                .await
                .0,
            StatusCode::OK
        );
        let (status, selected_items) =
            request(&a, "member-token-1", "GET", &selected_path, Value::Null).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(selected_items.as_array().unwrap().len(), 1);

        let other_owned_path = format!("/api/profiles/2/{suffix}");
        assert_eq!(
            request(&a, "member-token-1", "GET", &other_owned_path, Value::Null,)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            request(&a, "member-token-1", "PUT", &other_owned_path, body,)
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
            json!({"id":"tt-invalid","type":"movie","name":"Bad","position":-1.0,"duration":100.0})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
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
    for path in ["/api/providers", "/api/matches"] {
        for method in ["GET", "POST"] {
            // Unsupported methods retain the existing method-not-allowed contract.
            let status = request(&a, "member-token-1", method, path, json!({}))
                .await
                .0;
            assert!(status == StatusCode::FORBIDDEN || status == StatusCode::METHOD_NOT_ALLOWED);
        }
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
    owned.own_resource("playback", "private-session");
    for (method, path) in [
        ("GET", "/api/streams/private-job"),
        ("GET", "/api/streams/private-job/events"),
        ("POST", "/api/playback/private-session/heartbeat"),
        ("DELETE", "/api/playback/private-session"),
    ] {
        assert_eq!(
            request(&a, "member-token-2", method, path, Value::Null)
                .await
                .0,
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "DELETE",
            "/api/playback/private-session",
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    let (cards, _) = owned.register(
        "addon:1",
        vec![json!({"url":"https://example.com/video.mp4"})],
        "movie",
    );
    let status = request(
        &a,
        "member-token-2",
        "POST",
        "/api/playback",
        json!({"stream_id":cards[0]["id"]}),
    )
    .await
    .0;
    assert_eq!(status, StatusCode::NOT_FOUND);
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
        Arc::new(Job {
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
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "POST",
            "/api/playback",
            json!({"stream_id":cards[0]["id"]})
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
    for path in ["/api/providers", "/api/matches"] {
        assert_eq!(
            request(&a, "member-token-1", "GET", path, Value::Null)
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
async fn revoked_resource_lease_denies_bearerless_media_and_closes_open_sse() {
    use futures::StreamExt;
    let a = fixture();
    a.db.lock()
        .unwrap()
        .execute(
            "UPDATE auth_sessions SET profile_id=1 WHERE account_id=1",
            [],
        )
        .unwrap();
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
    owned.own_resource("job", "revocable-job");
    owned.own_resource("playback", "revocable-playback");
    a.jobs.lock().unwrap().insert(
        "revocable-job".into(),
        Arc::new(Job {
            kind: "movie".into(),
            created: Instant::now(),
            state: Mutex::new(JobState {
                events: vec![json!({"seq":1,"streams":[]})],
                pending: 1,
            }),
            notify: Notify::new(),
        }),
    );
    let response = router(a.clone(), None)
        .oneshot(
            Request::builder()
                .uri("/api/streams/revocable-job/events")
                .header("authorization", "Bearer member-token-1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body().into_data_stream();
    assert!(body
        .next()
        .await
        .unwrap()
        .unwrap()
        .windows(7)
        .any(|w| w == b"streams"));
    a.db.lock()
        .unwrap()
        .execute("DELETE FROM auth_sessions WHERE id='s1'", [])
        .unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(2), body.next())
        .await
        .unwrap()
        .is_none());
    let lease = a.resource_lease("playback", "revocable-playback").unwrap();
    assert!(lease.validate(&a.db.lock().unwrap()).is_err());
    let response = router(a.clone(), None)
        .oneshot(
            Request::builder()
                .uri("/media/revocable-playback/old-capability/index.m3u8")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    // The revoked lease took the stop-and-deny branch, not merely a missing file path.
    assert!(a.resource_lease("playback", "revocable-playback").is_none());
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
            StatusCode::NOT_FOUND
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
            json!({"stream_id":cards[0]["id"]})
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
        ("GET", "/api/live"),
        ("GET", "/api/guide/channel"),
        ("POST", "/api/playback"),
        ("GET", "/api/profiles/1/favorites"),
        ("PUT", "/api/profiles/1/progress"),
    ] {
        let response = router(a.clone(), None)
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
async fn addons_are_account_shared_and_cross_account_mutations_are_isolated() {
    let a = fixture();
    {
        let db = a.db.lock().unwrap();
        for owner in 1..=2 {
            db.execute("INSERT INTO addons(id,account_id,name,manifest_url,manifest) VALUES(?1,?1,?2,'https://example.com/manifest.json',?3)",params![owner,format!("Account {owner}"),json!({"name":format!("Account {owner}"),"catalogs":[{"id":"top","type":"movie"}]}).to_string()]).unwrap();
        }
    }
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
        assert_eq!(list.as_array().unwrap().len(), 1);
        assert_eq!(list[0]["id"], owner);
        assert!(list[0]["priority"].is_null());
    }
    let (status, _) = request(
        &a,
        "member-token-1",
        "PATCH",
        "/api/addons/2",
        json!({"enabled":false}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    request(&a, "member-token-1", "DELETE", "/api/addons/2", Value::Null).await;
    let (_, other) = request(&a, "member-token-2", "GET", "/api/addons", Value::Null).await;
    assert_eq!(other[0]["enabled"], true);
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
            json!({"id":"movie","type":"movie","name":"Test","position":20,"duration":100})
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
            &format!("/api/profiles/{id}/progress"),
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
    assert_eq!(profiles.as_array().unwrap().len(), 1);
    assert_eq!(profiles[0]["is_primary"], true);
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
    assert_eq!(defaults["audio_language"], "en");
    assert_eq!(defaults["autoplay"], true);
    assert_eq!(
        request(
            &a,
            "member-token-2",
            "PUT",
            "/api/profiles/1/preferences",
            json!({"autoplay":false})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, saved) = request(&a,"member-token-1","PUT","/api/profiles/1/preferences",json!({"audio_language":"ja","subtitles_enabled":true,"subtitle_size":"large","quality":"720p","autoplay":false})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["audio_language"], "ja");
    assert_eq!(saved["subtitle_language"], "en");
    let (_, autoplay) = request(
        &a,
        "member-token-1",
        "GET",
        "/api/profiles/1/continue/settings",
        Value::Null,
    )
    .await;
    assert_eq!(autoplay["autoplay"], false);
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "PUT",
            "/api/profiles/1/preferences",
            json!({"quality":"potato"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
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
}

#[tokio::test]
async fn service_health_is_owner_only_bounded_and_credential_free() {
    let a = fixture();
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "GET",
            "/api/service-health",
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    {
        let db = a.db.lock().unwrap();
        db.execute("UPDATE auth_accounts SET role='owner' WHERE id=1", [])
            .unwrap();
        for id in 1..=23 {
            db.execute("INSERT INTO providers(id,name,url,username,password,enabled,max_connections) VALUES(?1,?2,'http://127.0.0.1:9','secret-user','secret-password',0,4)",params![id,format!("Provider {id}")]).unwrap();
        }
    }
    let (status, first) = request(
        &a,
        "member-token-1",
        "GET",
        "/api/service-health",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["providers"]["total"], 23);
    assert_eq!(first["providers"]["items"].as_array().unwrap().len(), 20);
    assert_eq!(first["providers"]["next_offset"], 20);
    assert_eq!(first["providers"]["items"][0]["enabled"], false);
    assert_eq!(
        first["providers"]["items"][0]["last_catalog_at"],
        Value::Null
    );
    assert!(!first.to_string().contains("secret-"));
    assert!(!first.to_string().contains("127.0.0.1"));
    let (_, second) = request(
        &a,
        "member-token-1",
        "GET",
        "/api/service-health?offset=20",
        Value::Null,
    )
    .await;
    assert_eq!(second["providers"]["items"].as_array().unwrap().len(), 3);
    assert_eq!(second["providers"]["next_offset"], Value::Null);
    {
        let db = a.db.lock().unwrap();
        db.execute("UPDATE auth_sessions SET kind='device' WHERE id='s1'", [])
            .unwrap();
        db.execute(
            "INSERT INTO auth_device_profiles(session_id,profile_id) VALUES('s1',1)",
            [],
        )
        .unwrap();
    }
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "GET",
            "/api/service-health",
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn service_health_uses_valid_guide_mappings_and_shared_capacity() {
    let a = fixture();
    let now = util::now();
    {
        let db = a.db.lock().unwrap();
        db.execute("UPDATE auth_accounts SET role='owner' WHERE id=1", [])
            .unwrap();
        db.execute("INSERT INTO providers(id,name,url,username,password,enabled,max_connections) VALUES(1,'Family','http://127.0.0.1:9','user','password',0,4)",[]).unwrap();
        db.execute("INSERT INTO account_pools(id,name,configured_limit,external_reserve) VALUES(1,'Shared',4,1)",[]).unwrap();
        db.execute(
            "INSERT INTO provider_pools(provider_id,pool_id) VALUES(1,1)",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO account_observations(pool_id,reported_limit,limit_at,reported_usage,usage_at,external_estimate) VALUES(1,4,?1,2,?1,2)",[now-120]).unwrap();
        for id in ["family:valid", "family:changed"] {
            db.execute(
                "INSERT INTO family_channels(id,data) VALUES(?1,?2)",
                params![id, json!({"name":id,"enabled":true}).to_string()],
            )
            .unwrap();
            db.execute("INSERT INTO guide_mappings(channel_id,source_id,guide_id,observed_name) VALUES(?1,1,?1,'Verified')",[id]).unwrap();
            db.execute(
                "INSERT INTO guide_channels(source_id,guide_id,name) VALUES(1,?1,?2)",
                params![
                    id,
                    if id == "family:valid" {
                        "Verified"
                    } else {
                        "Changed feed"
                    }
                ],
            )
            .unwrap();
            db.execute("INSERT INTO family_programmes(channel_id,source_id,guide_id,start,end,data) VALUES(?1,1,?1,?2,?3,'{}')",params![id,now-60,now+3600]).unwrap();
        }
        db.execute(
            "INSERT INTO guide_sources(id,name,enabled,updated_at) VALUES(1,'Guide',1,?1)",
            [now - 30],
        )
        .unwrap();
        db.execute("INSERT INTO catalog_runs(id,state,owner_id,policy,created_at,started_at,finished_at) VALUES('finished','completed',1,'{}',?1,?1,?2)",params![now-120,now-60]).unwrap();
        db.execute("INSERT INTO catalog_results(run_id,provider_id,status) VALUES('finished',1,'completed')",[]).unwrap();
    }
    let (status, snapshot) = request(
        &a,
        "member-token-1",
        "GET",
        "/api/service-health",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(snapshot["guides"]["enabled_channels"], 2);
    assert_eq!(snapshot["guides"]["current_channels"], 1);
    assert_eq!(
        snapshot["providers"]["items"][0]["pool"]["estimated_free"],
        2
    );
    assert_eq!(
        snapshot["providers"]["items"][0]["pool"]["confidence"],
        "stale"
    );
    assert_eq!(
        snapshot["providers"]["items"][0]["last_catalog_at"],
        now - 60
    );
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
    let episode = json!({"id":"opaque-episode","type":"series","name":"Series","series_id":"opaque-series","season":2,"episode":3,"position":35,"duration":100,"source_addon_id":"provider:1","source_fingerprint":"stable"});
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
    let mut correction = episode.clone();
    correction["action"] = json!("watched");
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
        "/api/profiles/1/continue/page",
        Value::Null,
    )
    .await;
    assert_eq!(queue["items"][0]["id"], "opaque-episode");
    assert_eq!(queue["items"][0]["position"], 0.0);
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
        assert_eq!(request(&a,"member-token-1","PUT","/api/profiles/1/progress",json!({"id":id,"type":"series","name":series,"series_id":series,"season":1,"episode":1,"position":20,"duration":20})).await.0,StatusCode::OK);
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
        "/api/profiles/1/continue/page",
        Value::Null,
    )
    .await;
    assert_eq!(queue["items"][0]["queue_status"], "pending");
    assert_eq!(request(&a,"member-token-1","PUT","/api/profiles/1/progress",json!({"id":"unknown-duration","type":"series","name":"Show","series_id":"show","position":10,"duration":100})).await.0,StatusCode::OK);
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
    assert_eq!(request(&a,"member-token-1","PUT","/api/profiles/1/progress",json!({"id":"ep2","type":"series","name":"Show","series_id":"show","season":1,"episode":2,"position":5,"duration":100})).await.0,StatusCode::OK);
    let (_, queue) = request(
        &a,
        "member-token-1",
        "GET",
        "/api/profiles/1/continue/page",
        Value::Null,
    )
    .await;
    assert_eq!(queue["items"][0]["id"], "ep2");
}

async fn kids_metadata_fixture() -> (App, tokio::task::JoinHandle<()>) {
    let a = fixture();
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
                meta["videos"] = json!((0..2000).map(|n|json!({"id":format!("large-{n}"),"season":1,"episode":n+1,"title":format!("Episode {n}"),"description":"x".repeat(1024)})).collect::<Vec<_>>());
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
    let service = service.route("/catalog/:kind/:catalog/:extra", get(||async {axum::Json(json!({"metas":[{"id":"safe","type":"movie","name":"Trusted safe","contentRating":"TV-Y"},{"id":"unknown","type":"movie","name":"Unknown"},{"id":"adult","type":"movie","name":"Adult","contentRating":"TV-MA"}]}))}));
    let task = tokio::spawn(async move { axum::serve(listener, service).await.unwrap() });
    {
        let db = a.db.lock().unwrap();
        db.execute("INSERT INTO addons(account_id,name,manifest_url,manifest) VALUES(1,'Fixture',?1,?2)",params![format!("http://{address}/manifest.json"),json!({"id":"fixture","name":"Fixture","resources":["meta","catalog"],"types":["movie","series"],"catalogs":[{"id":"search","type":"movie","extra":[{"name":"search","isRequired":true}]}]}).to_string()]).unwrap();
        let hash = format!("{:x}", Sha256::digest(b"parent-token"));
        db.execute("INSERT INTO auth_sessions(id,account_id,access_hash,refresh_hash,csrf_hash,profile_id,kind,device_name,access_expires,refresh_expires,created_at) VALUES('parent',1,?1,'parent-refresh','unused',1,'browser','parent',?2,?2,0)",params![hash,util::now()+3600]).unwrap();
    }
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
        discover["metas"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["id"].as_str().unwrap())
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
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            &a,
            "parent-token",
            "POST",
            &format!("/api/profiles/{id}/approvals"),
            json!({"id":"unknown","type":"movie","approved":true})
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
            "/api/meta/movie/unknown",
            Value::Null
        )
        .await
        .1["meta"]["name"],
        "Trusted unknown"
    );
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
    assert_eq!(
        request(
            &a,
            "parent-token",
            "POST",
            &format!("/api/profiles/{id}/approvals"),
            json!({"id":"series","type":"series","approved":true})
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
            &format!("/api/streams/{}?after=0", old_job["id"].as_str().unwrap()),
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
async fn kids_policy_and_family_changes_revoke_already_issued_resources() {
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
    let (status, job) = request(
        &a,
        "member-token-1",
        "POST",
        "/api/streams",
        json!({"type":"movie","id":"safe"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
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
    let (_, job) = request(
        &a,
        "member-token-1",
        "POST",
        "/api/streams",
        json!({"type":"movie","id":"safe"}),
    )
    .await;
    let job_id = job["id"].as_str().unwrap();
    {
        let db = a.db.lock().unwrap();
        db.execute("INSERT INTO family_channels(id,data) VALUES('family:kids',?1)",[json!({"id":"family:kids","enabled":true,"country":"US","language":"en","category":"Kids"}).to_string()]).unwrap();
        db.execute("UPDATE family_channels SET data=json_set(data,'$.category','Movies') WHERE id='family:kids'",[]).unwrap();
    }
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
    assert_eq!(
        request(
            &a,
            "member-token-1",
            "GET",
            "/api/guide/family%3Akids",
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
            "/api/playback",
            json!({"channel_id":"iptv:1:1"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
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
    assert_eq!(original["meta"]["videos"].as_array().unwrap().len(), 2000);
    assert_eq!(
        original["meta"]["videos"][1999]["description"]
            .as_str()
            .unwrap()
            .len(),
        1024
    );
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
    let (status, page) = request(
        &a,
        "member-token-1",
        "GET",
        "/api/discover?type=series",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["total"], 1);
    assert!(page["metas"][0]["videos"].is_null());
    let (status, meta) = request(
        &a,
        "member-token-1",
        "GET",
        "/api/meta/series/large",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(meta["meta"]["videos"].as_array().unwrap().len(), 2000);
    assert!(meta["meta"]["videos"][0]["description"].is_null());
    for (episode, expected) in [
        ("large-1999", StatusCode::OK),
        ("large-2000", StatusCode::FORBIDDEN),
    ] {
        assert_eq!(request(&a,"member-token-1","PUT",&format!("/api/profiles/{id}/progress"),json!({"id":episode,"type":"series","series_id":"large","position":1,"duration":100})).await.0,expected);
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
        "/api/parent/search?type=movie&search=Trusted",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["items"].as_array().unwrap().len(), 2);
    assert_eq!(result["items"][0]["id"], "safe");
    assert_eq!(
        request(
            &a,
            "parent-token",
            "GET",
            "/api/parent/search?type=live&search=Trusted",
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
            "/api/parent/search?type=movie&search=Trusted",
            Value::Null
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
    for title in ["series", "mixed"] {
        assert_eq!(
            request(
                &a,
                "parent-token",
                "GET",
                &format!("/api/meta/series/{title}"),
                Value::Null
            )
            .await
            .0,
            StatusCode::OK
        );
    }
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
    request(
        &a,
        "parent-token",
        "PUT",
        &format!("/api/profiles/{id}/kids"),
        json!({"enabled":true,"max_age":7}),
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
    let (status, details) = request(
        &a,
        "member-token-1",
        "GET",
        "/api/meta/series/mixed",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        details["meta"]["videos"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["mixed-safe"]
    );
    for episode in [
        "mixed-adult",
        "mixed-older",
        "mixed-conflict",
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
    let (_, original) = request(
        &a,
        "parent-token",
        "GET",
        "/api/meta/series/mixed",
        Value::Null,
    )
    .await;
    assert_eq!(original["meta"]["videos"].as_array().unwrap().len(), 5);
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
    assert_eq!(first.as_array().unwrap().len(), 500);
    let (_, last) = request(
        &a,
        "parent-token",
        "GET",
        "/api/profiles/1/approvals?offset=500",
        Value::Null,
    )
    .await;
    assert_eq!(last.as_array().unwrap().len(), 1);
    assert_eq!(last[0]["id"], "title-500");
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
            json!({"id":"new-title","type":"movie","approved":true})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
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
        StatusCode::OK
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
        json!({"name":"TV profile","avatar_style":"critters","avatar_choice":2}),
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
    for path in ["/api/providers", "/api/auth/accounts"] {
        assert_eq!(
            request(&a, "member-token-1", "GET", path, Value::Null)
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
        json!({"name":"Renamed","avatar_choice":3}),
    )
    .await;
    assert_eq!(edited["id"], created["id"]);
    assert_eq!(edited["name"], "Renamed");
    assert_eq!(edited["avatar_choice"], 3);
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
