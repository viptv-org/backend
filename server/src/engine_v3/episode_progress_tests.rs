//! Details episode windows carry the selected profile's own watch state.
use crate::{auth_integration_tests::fixture, test_support::request, *};

const SERIES: &str = "tt38262097";

/// One metadata add-on (id 101) serving a six-episode season of `SERIES`.
async fn stack() -> (App, tokio::task::JoinHandle<()>) {
    let videos = (1..=6)
        .map(|episode| json!({"id":format!("{SERIES}:1:{episode}"),"season":1,"episode":episode,"title":format!("Episode {episode}")}))
        .collect::<Vec<_>>();
    let router = Router::new().route(
        &format!("/meta/series/{SERIES}.json"),
        get(move || {
            let videos = videos.clone();
            async move {
                axum::Json(json!({"meta":{"id":SERIES,"type":"series","name":"Level 999 no Murabito","videos":videos}}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let mut app = fixture();
    app.addons.allow_test_loopback = true;
    app.db.lock().unwrap().execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id) VALUES(101,'Metadata',?1,?2,1)",params![format!("http://{address}/manifest.json"),json!({"id":"fixture","name":"Metadata","version":"1","resources":["meta"],"types":["series"]}).to_string()]).unwrap();
    crate::test_support::encrypt_fixture_sources(&app);
    (app, task)
}

async fn episodes(app: &App, path: &str) -> Vec<Value> {
    let (status, detail) = request(app, "member-token-1", "GET", path, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    detail["episodes"].as_array().unwrap().clone()
}

fn episode(episodes: &[Value], number: u64) -> &Value {
    episodes
        .iter()
        .find(|card| card["identity"]["episode"]["episode"] == number)
        .unwrap_or_else(|| panic!("episode {number} missing"))
}

fn watched(card: &Value) -> bool {
    card["badges"]
        .as_array()
        .is_some_and(|badges| badges.iter().any(|badge| badge == "Watched"))
}

/// The owner's live rows: E1/E2 imported from Stremio as completion only
/// (no clock, no date), E3 part-way through, E4 finished by normal playback.
fn seed_owner_rows(app: &App) {
    let db = app.db.lock().unwrap();
    for number in [1, 2] {
        db.execute("INSERT INTO progress(profile_id,id,type,name,poster,position,duration,updated_at,context,title_id) VALUES(1,?1,'series','Level 999 no Murabito',NULL,0,0,0,?2,?3)",
            params![format!("{SERIES}:1:{number}"),json!({"episode":number,"imdb_id":SERIES,"season":1,"series_id":SERIES,"stremio_completion_only":true,"stremio_import_watched":true,"stremio_watch_date_unknown":true,"watched_override":true}).to_string(),SERIES]).unwrap();
    }
    db.execute("INSERT INTO progress(profile_id,id,type,name,poster,position,duration,updated_at,context,title_id) VALUES(1,?1,'series','Level 999 no Murabito',NULL,934.737,1430.015,1792484717,?2,?3)",
        params![format!("{SERIES}:1:3"),json!({"episode":3,"imdb_id":SERIES,"season":1,"series_id":SERIES}).to_string(),SERIES]).unwrap();
    db.execute("INSERT INTO progress(profile_id,id,type,name,poster,position,duration,updated_at,context,title_id) VALUES(1,?1,'series','Level 999 no Murabito',NULL,1400,1430,1792484000,?2,?3)",
        params![format!("{SERIES}:1:4"),json!({"episode":4,"imdb_id":SERIES,"season":1,"series_id":SERIES}).to_string(),SERIES]).unwrap();
}

#[tokio::test]
async fn details_episodes_show_imported_completion_partial_progress_and_finished_playback() {
    let (app, task) = stack().await;
    seed_owner_rows(&app);
    for path in [
        format!("/api/meta/series/{SERIES}"),
        format!("/api/meta/series/{SERIES}?provider_id=stremio&addon_id=101"),
        format!("/api/meta/series/{SERIES}?offset=0&limit=96"),
    ] {
        let list = episodes(&app, &path).await;
        for number in [1, 2, 4] {
            let card = episode(&list, number);
            assert!(watched(card), "{path}: E{number} must be watched: {card}");
            assert!(card["progress"].is_null(), "{path}: completed E{number} has no partial bar: {card}");
        }
        let partial = episode(&list, 3);
        assert!(!watched(partial), "{path}: E3 is in progress: {partial}");
        let fraction = partial["progress"].as_f64().unwrap_or_else(|| panic!("{path}: E3 progress: {partial}"));
        assert!((fraction - 934.737 / 1430.015).abs() < 1e-6, "{path}: {fraction}");
        for number in [5, 6] {
            let card = episode(&list, number);
            assert!(!watched(card) && card["progress"].is_null(), "{path}: E{number} is unseen: {card}");
        }
    }
    task.abort();
}

#[tokio::test]
async fn details_episode_state_is_private_to_the_selected_profile() {
    let (app, task) = stack().await;
    seed_owner_rows(&app);
    {
        let db = app.db.lock().unwrap();
        db.execute("INSERT INTO profiles(id,name,avatar_seed,presentation_complete) VALUES(2,'Other','fixture-two',1)", []).unwrap();
        db.execute("INSERT INTO profile_owners(profile_id,account_id,created_at) VALUES(2,1,0)", []).unwrap();
        db.execute("INSERT INTO auth_profiles VALUES(1,2)", []).unwrap();
        db.execute("UPDATE auth_sessions SET profile_id=2 WHERE account_id=1", []).unwrap();
    }
    let list = episodes(&app, &format!("/api/meta/series/{SERIES}")).await;
    assert_eq!(list.len(), 6);
    for card in &list {
        assert!(!watched(card) && card["progress"].is_null(), "another profile's state leaked: {card}");
    }
    task.abort();
}

#[tokio::test]
async fn details_episodes_reflect_origin_scoped_playback_records() {
    let (app, task) = stack().await;
    let path = format!("/api/meta/series/{SERIES}?provider_id=stremio&addon_id=101");
    let list = episodes(&app, &path).await;
    for (number, position, ended) in [(5, 40_000, false), (6, 10_000, true)] {
        let card = episode(&list, number).clone();
        assert!(!card["metadata_origin"].is_null(), "{card}");
        let record = json!({"identity":card["identity"],"metadata_origin":card["metadata_origin"],"parent_identity":card["parent_identity"],"provider_id":card["provider_id"],"title":card["title"],"poster":null,"landscape":null,"position_millis":position,"duration_millis":100_000,"updated_at_millis":0,"presentation":card});
        let (status, body) = request(&app, "member-token-1", "PUT", "/api/profiles/1/progress",
            json!({"record":record,"parent_identity":card["parent_identity"],"ended":ended})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let scoped: i64 = app.db.lock().unwrap().query_row("SELECT count(*) FROM engine_scoped_records WHERE profile_id=1 AND record_kind='playback'", [], |r| r.get(0)).unwrap();
    assert_eq!(scoped, 2, "playback went through the origin-scoped store");
    let list = episodes(&app, &path).await;
    let partial = episode(&list, 5);
    assert_eq!(partial["progress"], 0.4, "{partial}");
    assert!(!watched(partial), "{partial}");
    let finished = episode(&list, 6);
    assert!(watched(finished) && finished["progress"].is_null(), "{finished}");
    task.abort();
}
