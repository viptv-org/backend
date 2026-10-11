//! Contracts that outlived the removed legacy runtime: preserved history IDs,
//! archived preference data and database fairness without a media engine.
use crate::test_support::request;
use serde_json::{json, Value};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn database_waiters_do_not_starve_runtime_after_engine_removal() {
    let app = crate::auth_integration_tests::fixture();
    let db = app.db.clone();
    let (ready, wait) = std::sync::mpsc::sync_channel(1);
    let holder = std::thread::spawn(move || {
        let _guard = db.lock().unwrap();
        ready.send(()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(250));
    });
    wait.recv().unwrap();
    let mut requests = Vec::new();
    for _ in 0..4 {
        let app = app.clone();
        requests.push(tokio::spawn(async move {
            request(&app, "member-token-1", "GET", "/api/profiles", Value::Null)
                .await
                .0
        }));
    }
    tokio::time::timeout(
        std::time::Duration::from_millis(100),
        tokio::time::sleep(std::time::Duration::from_millis(10)),
    )
    .await
    .unwrap();
    for result in requests {
        assert_eq!(result.await.unwrap(), axum::http::StatusCode::OK);
    }
    holder.join().unwrap();
}

#[tokio::test]
async fn favorite_aliases_and_retired_composite_history_remain_exact_without_runtime_remapping() {
    let app = crate::auth_integration_tests::fixture();
    app.db.lock().unwrap().execute_batch("CREATE TABLE family_aliases(live_id TEXT PRIMARY KEY,channel_id TEXT);
        INSERT INTO family_aliases VALUES('iptv:1:1','family:old');
        INSERT INTO favorites(profile_id,id,type,name) VALUES(1,'iptv:1:1','live','Raw'),(1,'family:old','live','Historical');").unwrap();
    let (status, rows) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/favorites",
        Value::Null,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let mut ids: Vec<_> = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect();
    ids.sort();
    assert_eq!(ids, vec!["family:old", "iptv:1:1"]);
    let (status, _) = request(
        &app,
        "member-token-1",
        "DELETE",
        "/api/profiles/1/favorites/live/iptv%3A1%3A1",
        Value::Null,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row("SELECT id FROM favorites WHERE profile_id=1", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "family:old"
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/v2/iptv/live/family%3Aold/source",
            json!({})
        )
        .await
        .1["error_code"],
        "source_not_found"
    );
}

#[tokio::test]
async fn historical_maximum_quality_is_archived_but_never_returned_or_updated_as_active_policy() {
    let app = crate::auth_integration_tests::fixture();
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO playback_preferences VALUES(1,?1)",
            [
                json!({"quality":"480p","audio_language":"en","subtitle_language":"en"})
                    .to_string(),
            ],
        )
        .unwrap();
    let (_, read) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/profiles/1/preferences",
        Value::Null,
    )
    .await;
    assert!(read.get("quality").is_none());
    let (status, saved) = request(
        &app,
        "member-token-1",
        "PUT",
        "/api/profiles/1/preferences",
        json!({"audio_language":"ja"}),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(saved.get("quality").is_none());
    let stored: String = app
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT value FROM playback_preferences WHERE profile_id=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let stored: Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(stored["quality"], "480p");
    assert_eq!(stored["audio_language"], "ja");
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/profiles/1/preferences",
            json!({"quality":"1080p"})
        )
        .await
        .1["error_code"],
        "client_update_required"
    );
}
