use super::*;

fn scoped_clock(fixture: &Duplicate, position: u64, duration: u64, at: u64, completed: Option<u64>) {
    let db = fixture.app.db.lock().unwrap();
    let raw: String = db.query_row("SELECT value FROM engine_scoped_records WHERE record_kind='playback'", [], |r| r.get(0)).unwrap();
    let mut record: Value = serde_json::from_str(&raw).unwrap();
    record["position_millis"] = json!(position);
    record["duration_millis"] = json!(duration);
    record["updated_at_millis"] = json!(at);
    record["completed_at_millis"] = json!(completed);
    db.execute("UPDATE engine_scoped_records SET value=?1,updated_at_millis=?2 WHERE record_kind='playback'", params![record.to_string(), at]).unwrap();
}

fn continue_card(home: &Value) -> &Value {
    &home["rows"].as_array().unwrap().iter().find(|row| row["id"] == "continue").unwrap()["cards"][0]
}

#[tokio::test]
async fn exact_saved_clock_uses_floor_seconds_and_hour_boundaries() {
    let fixture = duplicate(true).await;
    for (index, (position, expected)) in [
        (1, "Resume at 0:00"), (999, "Resume at 0:00"),
        (1000, "Resume at 0:01"), (59_999, "Resume at 0:59"),
        (60_000, "Resume at 1:00"), (930_999, "Resume at 15:30"),
        (3_599_999, "Resume at 59:59"), (3_600_000, "Resume at 1:00:00"),
        (3_723_999, "Resume at 1:02:03"),
    ].into_iter().enumerate() {
        scoped_clock(&fixture, position, 40_000_000, 400_000 + index as u64, None);
        let action = call(&fixture.app, "POST", "/api/profiles/1/viewing-action", json!({"card":fixture.card})).await;
        assert_eq!(action["label"], expected, "{action}");
        assert_eq!(action["resume_position_millis"], position);
        assert!(action["remaining_label"].is_null());
        assert_eq!(action["progress"], position as f64 / 40_000_000.0);
    }
}

#[tokio::test]
async fn home_preview_uses_saved_clock_with_unknown_or_mismatched_metadata_runtime() {
    for runtime in [None, Some(2)] {
        let fixture = duplicate_with_runtime(true, runtime).await;
        scoped_clock(&fixture, 3_723_999, 8_000_000, 400_000, None);
        let page = call(&fixture.app, "GET", "/api/profiles/1/continue", Value::Null).await;
        assert_eq!(page["entries"][0]["status"], "Resume");
        assert_eq!(page["entries"][0]["record"]["position_millis"], 3_723_999);
        assert_eq!(page["entries"][0]["card"]["subtitle"], "S1 E2");
        let home = call(&fixture.app, "GET", "/api/profiles/1/home", Value::Null).await;
        let card = continue_card(&home);
        assert_eq!(card["subtitle"], "S1 E2");
        assert_eq!(card["view"]["preview"]["caption"], "Resume at 1:02:03");
        assert_eq!(card["view"]["preview"]["resume_position_millis"], 3_723_999);
        assert_eq!(card["view"]["preview"]["progress"], 3_723_999.0 / 8_000_000.0);
        let action = call(&fixture.app, "POST", "/api/profiles/1/viewing-action", json!({"card":card})).await;
        assert_eq!(action["label"], card["view"]["preview"]["caption"]);
        assert_eq!(action["resume_position_millis"], 3_723_999);
        assert_eq!(action["card"]["identity"], card["identity"]);
        assert_eq!(fixture.app.db.lock().unwrap().query_row("SELECT count(*) FROM progress", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    }
}

#[tokio::test]
async fn fresh_action_overrides_stale_home_clock_without_changing_history() {
    let fixture = duplicate(true).await;
    let home = call(&fixture.app, "GET", "/api/profiles/1/home", Value::Null).await;
    let stale = continue_card(&home);
    assert_eq!(stale["view"]["preview"]["resume_position_millis"], 30_000);
    scoped_clock(&fixture, 930_123, 3_600_000, 400_000, None);
    let action = call(&fixture.app, "POST", "/api/profiles/1/viewing-action", json!({"card":stale})).await;
    assert_eq!(action["resume_position_millis"], 930_123);
    assert_eq!(action["label"], "Resume at 15:30");
    assert_eq!(action["card"]["identity"], stale["identity"]);
    let page = call(&fixture.app, "GET", "/api/profiles/1/continue", Value::Null).await;
    assert_eq!(page["entries"][0]["record"]["position_millis"], 930_123);
    assert_eq!(page["entries"][0]["record"]["duration_millis"], 3_600_000);
}

#[tokio::test]
async fn restart_zero_and_absent_progress_keep_play_copy_and_null_clock() {
    let fixture = duplicate(true).await;
    let action = call(&fixture.app, "POST", "/api/profiles/1/viewing-action", json!({"card":fixture.card,"restart":true})).await;
    assert_eq!(action["label"], "Play S1 E2");
    assert_eq!(action["resume"], false);
    assert!(action.get("resume_position_millis").is_some());
    assert!(action["resume_position_millis"].is_null());
    for absent in [false, true] {
        {
            let db = fixture.app.db.lock().unwrap();
            db.execute("DELETE FROM engine_scoped_records WHERE record_kind='playback'", []).unwrap();
            if absent { db.execute("DELETE FROM progress", []).unwrap(); }
            else { db.execute("UPDATE progress SET position=0", []).unwrap(); }
        }
        let action = call(&fixture.app, "POST", "/api/profiles/1/viewing-action", json!({"card":fixture.card})).await;
        assert_eq!(action["label"], "Play S1 E2", "{action}");
        assert!(action.get("resume_position_millis").is_some());
        assert!(action["resume_position_millis"].is_null());
        assert!(action["remaining_label"].is_null());
    }
}

#[tokio::test]
async fn active_rewatch_keeps_saved_clock_and_completion_facts() {
    let fixture = duplicate(true).await;
    scoped_clock(&fixture, 30_000, 100_000, 400_000, Some(1000));
    let page = call(&fixture.app, "GET", "/api/profiles/1/continue", Value::Null).await;
    assert_eq!(page["entries"][0]["status"], "Resume");
    assert_eq!(page["entries"][0]["record"]["completed_at_millis"], 1000);
    let home = call(&fixture.app, "GET", "/api/profiles/1/home", Value::Null).await;
    assert_eq!(continue_card(&home)["view"]["preview"]["resume_position_millis"], 30_000);
    assert_eq!(continue_card(&home)["view"]["preview"]["caption"], "Resume at 0:30");
}

#[tokio::test]
async fn next_episode_never_reuses_the_completed_preceding_clock() {
    let fixture = duplicate(true).await;
    let mut previous = fixture.card.identity.clone();
    previous.id = "simkl:tv:20:1:1".into(); previous.episode.as_mut().unwrap().episode = 1;
    let context = json!({"engine_identity":previous,"engine_parent_identity":fixture.card.parent_identity,
        "engine_provider_id":"simkl","series_id":"simkl:tv:20","season":1,"episode":1,"watched_override":true});
    {
        let db = fixture.app.db.lock().unwrap();
        db.execute("DELETE FROM engine_scoped_records WHERE record_kind='playback'", []).unwrap();
        db.execute("UPDATE progress SET id=?1,position=100,duration=100,context=?2", params![previous.id, context.to_string()]).unwrap();
    }
    let next = call(&fixture.app, "POST", "/api/profiles/1/continue/next", json!({"identity":previous,"parent_identity":fixture.card.parent_identity})).await;
    assert_eq!(next["status"], "Next");
    let home = call(&fixture.app, "GET", "/api/profiles/1/home", Value::Null).await;
    let card = continue_card(&home);
    assert_eq!(card["view"]["preview"]["caption"], "Play S1 E2");
    assert!(card["view"]["preview"].get("resume_position_millis").is_some());
    assert!(card["view"]["preview"]["resume_position_millis"].is_null());
    let action = call(&fixture.app, "POST", "/api/profiles/1/viewing-action", json!({"card":card})).await;
    assert_eq!(action["label"], "Play S1 E2");
    assert_eq!(action["resume"], false);
    assert!(action["resume_position_millis"].is_null());
}

#[tokio::test]
async fn continuation_subtitles_keep_typed_special_and_absolute_coordinates() {
    for (season, order, caption) in [(Some(0), "aired", "S0 E2"), (None, "absolute", "E2")] {
        let fixture = duplicate(true).await;
        let mut identity = fixture.card.identity.clone();
        identity.id = "simkl:tv:20:0:2".into();
        identity.episode.as_mut().unwrap().season = season;
        identity.episode.as_mut().unwrap().order = order.into();
        let context = json!({"engine_identity":identity,"engine_parent_identity":fixture.card.parent_identity,
            "engine_provider_id":"simkl","series_id":"simkl:tv:20","season":season,"episode":2});
        fixture.app.db.lock().unwrap().execute("UPDATE progress SET id=?1,context=?2", params![identity.id, context.to_string()]).unwrap();
        let page = call(&fixture.app, "GET", "/api/profiles/1/continue", Value::Null).await;
        let entry = page["entries"].as_array().unwrap().iter().find(|entry| entry["card"]["identity"]["episode"]["order"] == order
            && entry["card"]["identity"]["episode"]["season"] == json!(season)).unwrap();
        assert_eq!(entry["status"], "Resume");
        assert_eq!(entry["card"]["subtitle"], caption);
    }
}

#[tokio::test]
async fn movie_resume_keeps_progress_without_remaining_subtitle() {
    let app = fixture();
    let identity = json!({"id":"owned-movie","kind":"movie","ids":[],"episode":null,"edition":null});
    call(&app, "PUT", "/api/profiles/1/progress", json!({"record":{"identity":identity,
        "parent_identity":null,"provider_id":"stremio","title":"Owned movie","poster":null,"landscape":null,
        "position_millis":90_123,"duration_millis":2_000_000,"updated_at_millis":1,"completed_at_millis":null}})).await;
    let page = call(&app, "GET", "/api/profiles/1/continue", Value::Null).await;
    assert_eq!(page["entries"][0]["status"], "Resume");
    assert!(page["entries"][0]["card"]["subtitle"].is_null());
    let home = call(&app, "GET", "/api/profiles/1/home", Value::Null).await;
    let card = continue_card(&home);
    assert!(card["subtitle"].is_null());
    assert_eq!(card["view"]["preview"]["caption"], "Resume at 1:30");
    assert_eq!(card["view"]["preview"]["resume_position_millis"], 90_123);
    assert_eq!(card["view"]["preview"]["progress"], 90_123.0 / 2_000_000.0);
}
