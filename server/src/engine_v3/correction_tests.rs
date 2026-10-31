use crate::{auth_integration_tests::fixture, test_support::request, *};
use viptv_engine::{origin::MetadataOrigin, presentation::Card};

fn origin() -> (MetadataOrigin, Value) {
    let origin = MetadataOrigin {
        provider_id: "stremio".into(),
        connection_id: Some("addon:1".into()),
        incarnation: Some("0123456789abcdef0123456789abcdef".into()),
    };
    let value = serde_json::to_value(&origin).unwrap();
    (origin, value)
}
fn movie(origin: &Value) -> (Value, Value) {
    let identity = json!({"id":"tt1000001","kind":"movie","ids":[{"namespace":"imdb","value":"tt1000001"}],"episode":null,"edition":null});
    let card = json!({"identity":identity,"metadata_origin":origin,"provider_id":"addon:1","title":"Finished film",
        "subtitle":null,"description":null,"poster":null,"landscape":null,"logo":null,"progress":null,"badges":[]});
    let record = json!({"identity":identity,"metadata_origin":origin,"parent_identity":null,"provider_id":"addon:1",
        "title":"Finished film","poster":null,"landscape":null,"position_millis":100_000,"duration_millis":100_000,
        "updated_at_millis":1_000,"completed_at_millis":1_000,"times_watched":1,"presentation":card,"_current_completed":true});
    (card, record)
}
fn episode(origin: &Value) -> (Value, Value) {
    let parent = json!({"id":"tt2000002","kind":"series","ids":[{"namespace":"imdb","value":"tt2000002"}],"episode":null,"edition":null});
    let identity = json!({"id":"tt2000002:1:5","kind":"series","ids":[{"namespace":"imdb","value":"tt2000002"}],
        "episode":{"order":"aired","season":1,"episode":5},"edition":null});
    let card = json!({"identity":identity,"parent_identity":parent,"metadata_origin":origin,"provider_id":"addon:1",
        "title":"Show","subtitle":null,"description":null,"poster":null,"landscape":null,"logo":null,"progress":null,"badges":[]});
    let record = json!({"identity":identity,"metadata_origin":origin,"parent_identity":parent,"provider_id":"addon:1",
        "title":"Show","poster":null,"landscape":null,"position_millis":100_000,"duration_millis":100_000,
        "updated_at_millis":1_000,"completed_at_millis":1_000,"times_watched":1,"presentation":card,"_current_completed":true});
    (card, record)
}
fn seed(app: &App, origin: &MetadataOrigin, card: &Value, record: &Value) {
    let key = origin.key().unwrap();
    let identity = card["identity"].to_string();
    let kind = card["identity"]["kind"].as_str().unwrap();
    let id = card["identity"]["id"].as_str().unwrap();
    let db = app.db.lock().unwrap();
    db.execute("INSERT INTO engine_scoped_records VALUES(1,'favorite','shared',?1,?2,?3,?2,?4,?5,1000,1000,1)",
        params![kind, id, key, identity, card.to_string()]).unwrap();
    db.execute("INSERT INTO engine_scoped_records VALUES(1,'playback','shared',?1,?2,?3,?2,?4,?5,NULL,1000,1)",
        params![kind, id, key, identity, record.to_string()]).unwrap();
}
fn stored(app: &App, id: &str) -> Value {
    let raw: String = app.db.lock().unwrap().query_row(
        "SELECT value FROM engine_scoped_records WHERE profile_id=1 AND record_kind='playback' AND identity_id=?1",
        [id], |r| r.get(0)).unwrap();
    serde_json::from_str(&raw).unwrap()
}
async fn get(app: &App, path: &str) -> Value {
    let (status, value) = request(app, "member-token-1", "GET", path, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{path}: {value}");
    value
}
async fn correct(app: &App, body: Value) -> Value {
    let (status, value) = request(app, "member-token-1", "PUT", "/api/profiles/1/progress/correct", body).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value
}
fn badge(app: &App, card: &Value) -> bool {
    let card: Card = serde_json::from_value(card.clone()).unwrap();
    super::watched::Watched::load(&app.db.lock().unwrap(), 1).unwrap().contains(&card)
}
fn statistics<'a>(library: &'a Value, id: &str) -> &'a Value {
    library["statistics"].as_array().unwrap().iter()
        .find(|s| s["identity"]["id"] == id)
        .unwrap_or_else(|| panic!("{id} has no statistics in {library}"))
}

#[tokio::test]
async fn a_correction_overrides_the_engine_playback_record_for_every_read() {
    let app = fixture();
    let (origin, origin_value) = origin();
    let (card, record) = movie(&origin_value);
    seed(&app, &origin, &card, &record);
    assert_eq!(get(&app, "/api/profiles/1/history").await[0]["watched"], json!(true));
    assert!(badge(&app, &card));
    assert_eq!(statistics(&get(&app, "/api/profiles/1/library").await, "tt1000001")["watched"], json!(true));
    let queue = get(&app, "/api/profiles/1/continue").await;
    assert!(queue["entries"].as_array().unwrap().is_empty(), "{queue}");

    let corrected = correct(&app, json!({"id":"tt1000001","type":"movie","name":"Finished film","action":"unwatched"})).await;
    assert_eq!(corrected["position"], json!(0.0));
    assert_eq!(corrected["duration"], json!(100.0));
    assert_eq!(corrected["watched"], json!(false));
    let history = get(&app, "/api/profiles/1/history").await;
    assert_eq!(history[0]["position"], json!(0.0));
    assert_eq!(history[0]["duration"], json!(100.0));
    assert_eq!(history[0]["watched"], json!(false));
    assert_eq!(history[0]["resume_active"], json!(false));
    assert!(get(&app, "/api/profiles/1/continue").await["entries"].as_array().unwrap().is_empty());
    assert!(!badge(&app, &card));
    assert_eq!(statistics(&get(&app, "/api/profiles/1/library").await, "tt1000001")["watched"], json!(false));
    let unwatched = stored(&app, "tt1000001");
    assert_eq!(unwatched["position_millis"], json!(0));
    assert_eq!(unwatched["duration_millis"], json!(100_000));
    assert!(unwatched["completed_at_millis"].is_null());
    assert_eq!(unwatched["_current_completed"], json!(false));
    assert_eq!(unwatched["times_watched"], json!(0));

    let corrected = correct(&app, json!({"id":"tt1000001","type":"movie","name":"Finished film","action":"position","position":30})).await;
    assert_eq!(corrected["position"], json!(30.0));
    assert_eq!(corrected["duration"], json!(100.0));
    assert_eq!(corrected["watched"], json!(false));
    let history = get(&app, "/api/profiles/1/history").await;
    assert_eq!(history[0]["position"], json!(30.0));
    assert_eq!(history[0]["duration"], json!(100.0));
    assert_eq!(history[0]["watched"], json!(false));
    assert_eq!(history[0]["resume_active"], json!(true));
    let queue = get(&app, "/api/profiles/1/continue").await;
    let entries = queue["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["status"], "Resume");
    assert_eq!(entries[0]["record"]["position_millis"], json!(30_000));
    assert_eq!(entries[0]["card"]["progress"], json!(0.3));
    assert!(!badge(&app, &card));
    assert_eq!(statistics(&get(&app, "/api/profiles/1/library").await, "tt1000001")["watched"], json!(false));

    correct(&app, json!({"id":"tt1000001","type":"movie","name":"Finished film","action":"watched"})).await;
    let history = get(&app, "/api/profiles/1/history").await;
    assert_eq!(history[0]["position"], json!(100.0));
    assert_eq!(history[0]["watched"], json!(true));
    assert_eq!(history[0]["resume_active"], json!(false));
    assert!(get(&app, "/api/profiles/1/continue").await["entries"].as_array().unwrap().is_empty());
    assert!(badge(&app, &card));
    let library = get(&app, "/api/profiles/1/library").await;
    assert_eq!(statistics(&library, "tt1000001")["watched"], json!(true));
    assert_eq!(statistics(&library, "tt1000001")["times_watched"], json!(1));
    let watched = stored(&app, "tt1000001");
    assert_eq!(watched["position_millis"], json!(100_000));
    assert!(watched["completed_at_millis"].as_u64().is_some());
    assert_eq!(watched["_current_completed"], json!(true));
}

#[tokio::test]
async fn a_corrected_episode_resumes_from_the_start_instead_of_reading_as_finished() {
    let app = fixture();
    let (origin, origin_value) = origin();
    let (card, record) = episode(&origin_value);
    seed(&app, &origin, &card, &record);
    let queue = get(&app, "/api/profiles/1/continue").await;
    let entries = queue["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["status"], "Pending");
    assert_eq!(entries[0]["card"]["badges"], json!(["Watched"]));

    correct(&app, json!({"id":"tt2000002:1:5","type":"series","name":"Show","series_id":"tt2000002","season":1,"episode":5,"action":"unwatched"})).await;
    let history = get(&app, "/api/profiles/1/history").await;
    assert_eq!(history[0]["position"], json!(0.0));
    assert_eq!(history[0]["duration"], json!(100.0));
    assert_eq!(history[0]["watched"], json!(false));
    let queue = get(&app, "/api/profiles/1/continue").await;
    let entries = queue["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["status"], "Resume");
    assert_eq!(entries[0]["card"]["identity"]["id"], "tt2000002:1:5");
    assert_eq!(entries[0]["record"]["position_millis"], json!(0));
    assert_eq!(entries[0]["card"]["progress"], json!(0.0));
    assert!(entries[0]["card"]["badges"].as_array().unwrap().is_empty());
    let unwatched = stored(&app, "tt2000002:1:5");
    assert!(unwatched["completed_at_millis"].is_null());
    assert_eq!(unwatched["_current_completed"], json!(false));

    correct(&app, json!({"id":"tt2000002:1:5","type":"series","name":"Show","series_id":"tt2000002","season":1,"episode":5,"action":"watched"})).await;
    let queue = get(&app, "/api/profiles/1/continue").await;
    let entries = queue["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["status"], "Pending");
    assert_eq!(entries[0]["card"]["badges"], json!(["Watched"]));
    let history = get(&app, "/api/profiles/1/history").await;
    assert_eq!(history[0]["position"], json!(100.0));
    assert_eq!(history[0]["watched"], json!(true));
}
