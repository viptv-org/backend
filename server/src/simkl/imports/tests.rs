use super::*;
#[tokio::test]
async fn remote_membership_preserves_local_identity_added_date_and_removal() {
    let app = crate::auth_integration_tests::fixture();
    let db = app.db.lock().unwrap();
    let entry=super::super::watchlists::projection::entries(&json!({"movies":[{"status":"completed","user_rating":8,"movie":{"title":"Remote movie","ids":{"simkl":10,"imdb":"tt1234567"}}}]})).unwrap();
    db.execute("INSERT INTO favorites(profile_id,id,type,name,context,added_at_millis) VALUES(1,'tt1234567','movie','My title','{}',123)",[]).unwrap();
    assert_eq!(membership::apply(&db, 1, 1, &entry, 1000).unwrap(), 0);
    let local: (String, u64) = db
        .query_row(
            "SELECT name,added_at_millis FROM favorites WHERE profile_id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(local, ("My title".into(), 123));
    db.execute("DELETE FROM favorites WHERE profile_id=1", [])
        .unwrap();
    assert_eq!(membership::apply(&db, 1, 2, &entry, 2000).unwrap(), 0);
    let count: u64 = db
        .query_row(
            "SELECT COUNT(*) FROM favorites WHERE profile_id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    let stats: String = db
        .query_row(
            "SELECT statistics FROM simkl_import_entries WHERE profile_id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let stats: Value = serde_json::from_str(&stats).unwrap();
    assert_eq!(stats["provenance"][0]["imported_at_millis"], 1000);
    assert_eq!(stats["identity"]["id"], "tt1234567");
}
#[tokio::test]
async fn import_never_overwrites_local_progress_or_exports_and_only_advances_unchanged_receipts() {
    let app = crate::auth_integration_tests::fixture();
    let db = app.db.lock().unwrap();
    let card =
        viptv_engine::simkl::card(&json!({"title":"Movie","ids":{"simkl":10}}), "movie").unwrap();
    assert!(progress::apply(&db, 1, &card, None, 50.0, 100.0, 100000, false, 1).unwrap());
    assert!(!progress::apply(&db, 1, &card, None, 20.0, 100.0, 90000, false, 1).unwrap());
    assert!(progress::apply(&db, 1, &card, None, 60.0, 100.0, 110000, false, 1).unwrap());
    assert!(!progress::apply(&db, 1, &card, None, 65.0, 100.0, 115000, false, 2).unwrap());
    db.execute("UPDATE progress SET position=70 WHERE profile_id=1", [])
        .unwrap();
    assert!(!progress::apply(&db, 1, &card, None, 80.0, 100.0, 120000, false, 1).unwrap());
    let position: f64 = db
        .query_row(
            "SELECT position FROM progress WHERE profile_id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(position, 70.0);
    let tracking: u64 = db
        .query_row("SELECT COUNT(*) FROM simkl_tracking_receipts", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(tracking, 0);
}
#[tokio::test]
async fn original_episode_import_does_not_invent_missing_watched_rows_dates_or_anime_seasons() {
    let app = crate::auth_integration_tests::fixture();
    let db = app.db.lock().unwrap();
    let items = json!({"shows":[{"status":"completed","watched_episodes_count":100,"show":{"title":"Show","ids":{"simkl":20}},"seasons":[{"number":1,"episodes":[{"number":2,"watched_at":"2020-01-01T00:00:00Z","runtime":24},{"number":3}]}]}],"anime":[{"show":{"title":"Anime","ids":{"simkl":30}},"seasons":[{"number":1,"episodes":[{"number":4,"watched_at":"2020-01-01T00:00:00Z"}]}]}]});
    let entries = super::super::watchlists::projection::entries(&items).unwrap();
    let counts = history::original(&db, 1, 1, &items, &entries).unwrap();
    assert_eq!(counts.inserted, 2);
    assert_eq!(counts.unresolved, 1);
    let context: String = db
        .query_row(
            "SELECT context FROM progress WHERE id='simkl:anime:30:anidb:4'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let context: Value = serde_json::from_str(&context).unwrap();
    assert!(context["engine_identity"]["episode"]["season"].is_null());
    assert_eq!(context["engine_identity"]["episode"]["order"], "anidb");
}

#[tokio::test]
async fn cheap_membership_contains_verified_aliases_and_alias_removal_targets_one_saved_row() {
    use crate::test_support::request;
    let app = crate::auth_integration_tests::fixture();
    {
        let db = app.db.lock().unwrap();
        db.execute("INSERT INTO favorites(profile_id,id,type,name,context) VALUES(1,'tt1234567','movie','Local','{}')",[]).unwrap();
        let entries = super::super::watchlists::projection::entries(
            &json!({"movies":[{"movie":{"title":"Remote","ids":{"simkl":10,"imdb":"tt1234567"}}}]}),
        )
        .unwrap();
        membership::apply(&db, 1, 1, &entries, 1000).unwrap();
    }
    let path = "/api/profiles/1/library/membership";
    let (status, first) = request(&app, "member-token-1", "GET", path, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["complete"], true);
    let ids = first["identities"].as_array().unwrap();
    assert_eq!(ids.len(), 2);
    assert!(ids.iter().any(|v| v["id"] == "tt1234567"));
    assert!(ids.iter().any(|v| v["id"] == "simkl:movie:10"));
    assert_eq!(
        request(&app, "member-token-1", "GET", path, Value::Null)
            .await
            .1,
        first
    );
    assert_eq!(
        request(&app, "member-token-2", "GET", path, Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let alias_card = viptv_engine::simkl::card(
        &json!({"title":"Remote","ids":{"simkl":10,"imdb":"tt1234567"}}),
        "movie",
    )
    .unwrap();
    let (status, saved) = request(
        &app,
        "member-token-1",
        "PUT",
        "/api/profiles/1/library",
        json!({"card":alias_card}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["saved"], true);
    assert_eq!(saved["library_membership"]["complete"], true);
    assert_eq!(
        saved["library_membership"]["identities"],
        first["identities"]
    );
    assert!(
        saved["library_membership"]["revision"].as_u64().unwrap()
            > first["revision"].as_u64().unwrap()
    );
    assert_eq!(
        request(&app, "member-token-1", "GET", path, Value::Null)
            .await
            .1,
        saved["library_membership"]
    );
    let (status, removed) = request(
        &app,
        "member-token-1",
        "DELETE",
        "/api/profiles/1/library/movie/simkl:movie:10",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(removed["saved"], false);
    assert_eq!(removed["library_membership"]["complete"], true);
    assert!(removed["library_membership"]["identities"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(
        removed["library_membership"]["revision"].as_u64().unwrap()
            > saved["library_membership"]["revision"].as_u64().unwrap()
    );
    let (_, empty) = request(&app, "member-token-1", "GET", path, Value::Null).await;
    assert_eq!(empty, removed["library_membership"]);
    let (status, repeated) = request(
        &app,
        "member-token-1",
        "DELETE",
        "/api/profiles/1/library/movie/simkl:movie:10",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(repeated["library_membership"], empty);
}

#[tokio::test]
async fn deleted_or_cleared_history_stays_removed_and_unchanged_imports_keep_revision_stable() {
    let app = crate::auth_integration_tests::fixture();
    let db = app.db.lock().unwrap();
    let entries = super::super::watchlists::projection::entries(
        &json!({"movies":[{"status":"completed","movie":{"title":"Remote","ids":{"simkl":10}}}]}),
    )
    .unwrap();
    membership::apply(&db, 1, 1, &entries, 1000).unwrap();
    let first: u64 = db
        .query_row(
            "SELECT revision FROM engine_library_revisions WHERE profile_id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    membership::apply(&db, 1, 1, &entries, 2000).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT revision FROM engine_library_revisions WHERE profile_id=1",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        first
    );
    let card = &entries[0].card;
    assert!(progress::apply(&db, 1, card, None, 50.0, 100.0, 100000, false, 1).unwrap());
    db.execute("DELETE FROM progress WHERE profile_id=1", [])
        .unwrap();
    assert!(!progress::apply(&db, 1, card, None, 70.0, 100.0, 200000, false, 1).unwrap());
    db.execute(
        "INSERT INTO simkl_import_history_floors VALUES(1,500000)",
        [],
    )
    .unwrap();
    let mut other = card.clone();
    other.identity.id = "simkl:movie:11".into();
    assert!(!progress::apply(&db, 1, &other, None, 70.0, 100.0, 400000, false, 1).unwrap());
}

#[tokio::test]
async fn origin_bearing_simkl_save_and_unsave_keep_verified_import_alias_canonical() {
    use crate::test_support::request;
    let routes = axum::Router::new().route(
        "/movies/10",
        axum::routing::get(|| async {
            axum::Json(json!({"title":"Remote","ids":{"simkl":10,"imdb":"tt1234567"}}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    let mut app = crate::auth_integration_tests::fixture();
    app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
    let entries = super::super::watchlists::projection::entries(
        &json!({"movies":[{"movie":{"title":"Remote","ids":{"simkl":10,"imdb":"tt1234567"}}}]}),
    )
    .unwrap();
    {
        let db = app.db.lock().unwrap();
        db.execute("INSERT INTO favorites(profile_id,id,type,name,context)VALUES(1,'tt1234567','movie','Local','{}')",[]).unwrap();
        membership::apply(&db, 1, 1, &entries, 1000).unwrap();
    }
    let mut card = viptv_engine::simkl::card(
        &json!({"title":"Remote","ids":{"simkl":10,"imdb":"tt1234567"}}),
        "movie",
    )
    .unwrap();
    card.metadata_origin = Some(viptv_engine::origin::MetadataOrigin {
        provider_id: "simkl".into(),
        connection_id: None,
        incarnation: None,
    });
    let (status, saved) = request(
        &app,
        "member-token-1",
        "PUT",
        "/api/profiles/1/library",
        json!({"card":card}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let (status, library) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/library",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{library}");
    assert_eq!(library["cards"].as_array().unwrap().len(), 1);
    assert_eq!(library["cards"][0]["identity"]["id"], "simkl:movie:10");
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair(
            "metadata_origin",
            &serde_json::to_string(card.metadata_origin.as_ref().unwrap()).unwrap(),
        )
        .append_pair("card", &serde_json::to_string(&card).unwrap())
        .finish();
    let path = format!("/api/profiles/1/library/movie/simkl:movie:10?{query}");
    let (status, removed) = request(&app, "member-token-1", "DELETE", &path, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{removed}");
    assert!(removed["library_membership"]["identities"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(removed["library_membership"]
        .get("scoped_identities")
        .is_none_or(|value| value.as_array().unwrap().is_empty()));
    {
        let db = app.db.lock().unwrap();
        membership::apply(&db, 1, 1, &entries, 2000).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT count(*)FROM favorites WHERE profile_id=1",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0,
            "An explicit canonical removal survives later import refresh"
        );
    }
    let (status, resaved) = request(
        &app,
        "member-token-1",
        "PUT",
        "/api/profiles/1/library",
        json!({"card":card}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resaved}");
    assert!(
        resaved["library_membership"]["identities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|identity| identity["id"] == "simkl:movie:10"),
        "Known import aliases remain complete when only the scoped favorite exists"
    );
    let (status, _) = request(&app, "member-token-1", "DELETE", &path, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    // Public alias removal also works when only the verified canonical legacy backing exists.
    app.db.lock().unwrap().execute("INSERT INTO favorites(profile_id,id,type,name,context)VALUES(1,'tt1234567','movie','Local again','{}')",[]).unwrap();
    let (status, removed) = request(&app, "member-token-1", "DELETE", &path, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{removed}");
    assert!(removed["library_membership"]["identities"]
        .as_array()
        .unwrap()
        .is_empty());
    server.abort();
}

#[tokio::test]
async fn modern_simkl_alias_removal_never_erases_ambiguous_legacy_namespace_collision() {
    use crate::test_support::request;
    let app = crate::auth_integration_tests::fixture();
    let entries = super::super::watchlists::projection::entries(
        &json!({"movies":[{"movie":{"title":"Remote","ids":{"simkl":10,"imdb":"tt1234567"}}}]}),
    )
    .unwrap();
    {
        let db = app.db.lock().unwrap();
        db.execute("INSERT INTO favorites(profile_id,id,type,name,context)VALUES(1,'simkl:movie:10','movie','Unrelated old provider title','{}')",[]).unwrap();
        membership::apply(&db, 1, 1, &entries, 1000).unwrap();
    }
    let mut card = viptv_engine::simkl::card(
        &json!({"title":"Remote","ids":{"simkl":10,"imdb":"tt1234567"}}),
        "movie",
    )
    .unwrap();
    card.metadata_origin = Some(viptv_engine::origin::MetadataOrigin {
        provider_id: "simkl".into(),
        connection_id: None,
        incarnation: None,
    });
    let q = url::form_urlencoded::Serializer::new(String::new())
        .append_pair(
            "metadata_origin",
            &serde_json::to_string(card.metadata_origin.as_ref().unwrap()).unwrap(),
        )
        .append_pair("card", &serde_json::to_string(&card).unwrap())
        .finish();
    let (status, removed) = request(
        &app,
        "member-token-1",
        "DELETE",
        &format!("/api/profiles/1/library/movie/simkl:movie:10?{q}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{removed}");
    assert!(
        removed["library_membership"]["identities"]
            .as_array()
            .unwrap()
            .is_empty(),
        "The explicitly removed modern alias is no longer saved"
    );
    assert_eq!(
        removed["library_membership"]["legacy_identities"][0]["id"],
        "simkl:movie:10"
    );
    let db = app.db.lock().unwrap();
    assert_eq!(
        db.query_row(
            "SELECT name FROM favorites WHERE profile_id=1 AND id='simkl:movie:10'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "Unrelated old provider title"
    );
}
