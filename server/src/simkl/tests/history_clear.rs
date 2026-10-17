use super::*;

#[tokio::test]
async fn clear_history_is_selected_profile_local_only_and_invalidates_continue_watching() {
    let (app, task, provider_writes) = server().await;
    link(&app).await;
    {
        let db = app.db.lock().unwrap();
        db.execute_batch("INSERT INTO profiles(id,name) VALUES(3,'Other'); INSERT INTO profile_owners(profile_id,account_id,created_at) VALUES(3,1,0);
            INSERT INTO progress(profile_id,id,type,name,position,duration,updated_at) VALUES(1,'simkl:movie:10','movie','Movie',20,100,1),(3,'opaque-other','movie','Other',40,100,1);
            INSERT INTO favorites(profile_id,id,type,name) VALUES(1,'favorite','movie','Preserved');
            INSERT INTO continuation_cache VALUES(1,'series','episode','{}',1),(3,'other-series','other-episode','{}',1);").unwrap();
    }
    for (token, profile) in [("member-token-2", 1), ("member-token-1", 3)] {
        assert_eq!(
            request(
                &app,
                token,
                "DELETE",
                &format!("/api/profiles/{profile}/history"),
                json!({})
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    let (_, before) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/bootstrap",
        json!({}),
    )
    .await;
    assert_eq!(
        before["home"]["rows"][0]["cards"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "DELETE",
            "/api/profiles/1/history",
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, history) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/history",
        json!({}),
    )
    .await;
    assert!(history.as_array().unwrap().is_empty());
    let (_, after) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/bootstrap",
        json!({}),
    )
    .await;
    assert!(after["home"]["rows"][0]["cards"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(
        after["home"]["revision"].as_u64().unwrap() > before["home"]["revision"].as_u64().unwrap()
    );
    {
        let db = app.db.lock().unwrap();
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM progress WHERE profile_id=3",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM favorites WHERE profile_id=1",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM continuation_cache WHERE profile_id=1",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM continuation_cache WHERE profile_id=3",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }
    assert_eq!(provider_writes.load(Ordering::SeqCst), 0);
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "DELETE",
            "/api/profiles/1/history",
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    // This local-only action uses the same selected child-profile policy as GET.
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO kids_profiles(profile_id,enabled,max_age,revision) VALUES(1,1,12,1)",
            [],
        )
        .unwrap();
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "DELETE",
            "/api/profiles/1/history",
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(provider_writes.load(Ordering::SeqCst), 0);
    task.abort();
}
