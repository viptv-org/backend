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
    let path = "/api/v3/profiles/1/library/membership";
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
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "DELETE",
            "/api/v3/profiles/1/library/movie/simkl:movie:10",
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, empty) = request(&app, "member-token-1", "GET", path, Value::Null).await;
    assert!(empty["identities"].as_array().unwrap().is_empty());
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
