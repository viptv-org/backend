use super::*;
use crate::{auth_integration_tests::fixture, test_support::request};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Mutex,
};

const DAY: u64 = 86_400_000;
const HOUR: u64 = 3_600_000;

fn now() -> u64 {
    util::now_millis() as u64
}
fn iso(millis: u64) -> String {
    chrono::DateTime::from_timestamp_millis(millis as i64)
        .unwrap()
        .to_rfc3339()
}
fn episode(number: u32) -> Value {
    json!({"id":format!("tt100:1:{number}"),"kind":"series","ids":[],"episode":{"order":"aired","season":1,"episode":number},"edition":null})
}
fn series() -> Value {
    json!({"id":"tt100","kind":"series","ids":[],"episode":null,"edition":null})
}

struct Show {
    app: App,
    releases: Arc<Mutex<Vec<(u32, u64)>>>,
    calls: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Show {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn show(releases: Vec<(u32, u64)>) -> Show {
    let mut app = fixture();
    app.addons.allow_test_loopback = true;
    let releases = Arc::new(Mutex::new(releases));
    let calls = Arc::new(AtomicUsize::new(0));
    let (served, counted) = (releases.clone(), calls.clone());
    let routes = Router::new().route(
        "/meta/series/:file",
        axum::routing::get(move |axum::extract::Path(file): axum::extract::Path<String>| {
            let id = file.trim_end_matches(".json").to_owned();
            let videos = served
                .lock()
                .unwrap()
                .iter()
                .map(|(number, at)| json!({"id":format!("{id}:1:{number}"),"title":format!("Chapter {number}"),"season":1,"episode":number,"released":iso(*at),"thumbnail":format!("https://img.invalid/{number}.jpg")}))
                .collect::<Vec<_>>();
            counted.fetch_add(1, Ordering::SeqCst);
            async move {
                axum::Json(json!({"meta":{"id":id,"type":"series","name":"Fixture Show","background":format!("https://img.invalid/{id}/backdrop.jpg"),"videos":videos}}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    app.db.lock().unwrap().execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id) VALUES(1,'Metadata',?1,?2,1)",params![format!("http://{address}/manifest.json"),json!({"id":"metadata","name":"Metadata","version":"1","resources":["meta"],"types":["series"]}).to_string()]).unwrap();
    crate::test_support::encrypt_fixture_sources(&app);
    Show {
        app,
        releases,
        calls,
        task,
    }
}

async fn watch(app: &App, identity: Value, parent: Option<Value>, ended: bool, at: u64) {
    let finished = if ended { 100000 } else { 30000 };
    let (status, value) = request(app, "member-token-1", "PUT", "/api/profiles/1/progress", json!({"ended":ended,"record":{"identity":identity,"parent_identity":parent,"provider_id":"stremio","title":if parent.is_some(){"Fixture Show"}else{"Fixture Film"},"poster":null,"landscape":null,"position_millis":finished,"duration_millis":100000,"updated_at_millis":1,"completed_at_millis":null}})).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    app.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE progress SET updated_at=?1 WHERE id=?2",
            params![(at / 1000) as i64, identity["id"].as_str().unwrap()],
        )
        .unwrap();
}

async fn resolve(app: &App, from: u32) {
    app.addons.forget_responses();
    let (status, next) = request(
        app,
        "member-token-1",
        "POST",
        "/api/profiles/1/continue/next",
        json!({"identity":episode(from),"parent_identity":series()}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{next}");
}

async fn queue(app: &App) -> Vec<Value> {
    let (status, page) = request(
        app,
        "member-token-1",
        "GET",
        "/api/profiles/1/continue",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    page["entries"].as_array().unwrap().clone()
}
fn ids(entries: &[Value]) -> Vec<String> {
    entries
        .iter()
        .map(|e| e["card"]["identity"]["id"].as_str().unwrap().to_owned())
        .collect()
}
fn show_entry(entries: &[Value]) -> Value {
    entries
        .iter()
        .find(|e| {
            e["record"]["identity"]["id"]
                .as_str()
                .unwrap()
                .starts_with("tt100")
        })
        .cloned()
        .unwrap()
}

#[tokio::test]
async fn a_release_promotes_a_caught_up_series_once_and_later_releases_only_count() {
    let start = now();
    let show = show(vec![
        (1, start - 30 * DAY),
        (2, start + 7 * DAY),
        (3, start + 14 * DAY),
        (4, start + 21 * DAY),
    ])
    .await;
    let app = &show.app;
    watch(app, episode(1), Some(series()), true, start - 10 * DAY).await;
    let film = json!({"id":"tt200","kind":"movie","ids":[],"episode":null,"edition":null});
    watch(app, film, None, false, start - 5 * DAY).await;
    resolve(app, 1).await;

    let entries = queue(app).await;
    assert_eq!(ids(&entries), ["tt200", "tt100:1:1"]);
    let caught_up = show_entry(&entries);
    assert_eq!(caught_up["card"]["badges"], json!(["Watched"]), "{caught_up}");

    show.releases.lock().unwrap()[1].1 = start - 2 * DAY;
    resolve(app, 1).await;
    let entries = queue(app).await;
    assert_eq!(ids(&entries), ["tt100:1:2", "tt200"]);
    let new = show_entry(&entries);
    assert_eq!(new["status"], "Next");
    assert_eq!(new["card"]["badges"], json!(["+1"]));
    assert_eq!(new["card"]["title"], "Fixture Show");
    assert_eq!(new["card"]["subtitle"], "S1 E2 · Chapter 2");
    assert_eq!(new["card"]["parent_identity"]["id"], "tt100");
    assert!(new["card"]["progress"].is_null());
    assert_eq!(new["previous_identity"]["id"], "tt100:1:1");
    assert_eq!(new["record"]["identity"]["id"], "tt100:1:1");

    let other = json!({"id":"tt300","kind":"movie","ids":[],"episode":null,"edition":null});
    watch(app, other, None, false, start - DAY).await;
    show.releases.lock().unwrap()[2].1 = start - HOUR;
    resolve(app, 1).await;
    let entries = queue(app).await;
    assert_eq!(ids(&entries), ["tt300", "tt100:1:2", "tt200"]);
    assert_eq!(
        show_entry(&entries)["card"]["badges"],
        json!(["+2"])
    );

    let (status, home) = request(
        app,
        "member-token-1",
        "GET",
        "/api/profiles/1/home",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{home}");
    let row = home["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "continue")
        .unwrap();
    assert_eq!(row["cards"][1]["badges"], json!(["+2"]));

    let (status, receipt) = request(
        app,
        "member-token-1",
        "PUT",
        "/api/profiles/1/continue/visibility",
        json!({"identity":episode(1),"parent_identity":series(),"hidden":true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    assert_eq!(ids(&queue(app).await), ["tt300", "tt200"]);
    app.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE queue_hidden_times SET hidden_at=?1 WHERE title_id='tt100'",
            [((start - 30 * 60_000) / 1000) as i64],
        )
        .unwrap();
    assert_eq!(ids(&queue(app).await), ["tt300", "tt200"]);
    show.releases.lock().unwrap()[3].1 = start - 10 * 60_000;
    resolve(app, 1).await;
    let entries = queue(app).await;
    assert_eq!(ids(&entries), ["tt100:1:2", "tt300", "tt200"]);
    assert_eq!(
        show_entry(&entries)["card"]["badges"],
        json!(["+3"])
    );
    request(
        app,
        "member-token-1",
        "GET",
        "/api/profiles/1/home",
        Value::Null,
    )
    .await;
    let hidden = || {
        app.db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM queue_hidden", [], |r| r.get::<_, i64>(0))
            .unwrap()
    };
    until(|| hidden() == 0).await;
    assert_eq!(hidden(), 0);

    watch(app, episode(2), Some(series()), true, now()).await;
    resolve(app, 2).await;
    let entries = queue(app).await;
    assert_eq!(entries[0]["record"]["identity"]["id"], "tt100:1:2");
    assert!(
        entries[0]["card"]["badges"].as_array().unwrap().is_empty(),
        "{}",
        entries[0]
    );
}

#[tokio::test]
async fn a_backlog_and_an_in_progress_episode_are_never_promoted() {
    let start = now();
    let show = show(vec![
        (1, start - 30 * DAY),
        (2, start - 20 * DAY),
        (3, start - HOUR),
    ])
    .await;
    let app = &show.app;
    watch(app, episode(1), Some(series()), true, start - 10 * DAY).await;
    resolve(app, 1).await;
    let entries = queue(app).await;
    assert!(show_entry(&entries)["card"]["badges"]
        .as_array()
        .unwrap()
        .is_empty());
    watch(app, episode(2), Some(series()), false, start - 2 * DAY).await;
    let entries = queue(app).await;
    assert_eq!(show_entry(&entries)["status"], "Resume");
    assert!(show_entry(&entries)["card"]["badges"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn credits_count_as_finished_and_the_newest_finished_episode_is_drawn_watched() {
    let start = now();
    let show = show(vec![
        (1, start - 30 * DAY),
        (2, start - 20 * DAY),
        (3, start + 7 * DAY),
    ])
    .await;
    let app = &show.app;
    let (status, value) = request(app, "member-token-1", "PUT", "/api/profiles/1/progress", json!({"ended":false,"record":{"identity":episode(1),"parent_identity":series(),"provider_id":"stremio","title":"Fixture Show","poster":null,"landscape":null,"position_millis":1_382_000,"duration_millis":1_440_000,"updated_at_millis":1,"completed_at_millis":null}})).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    resolve(app, 1).await;
    let next = show_entry(&queue(app).await);
    assert_eq!(next["status"], "Next", "{next}");
    assert_eq!(next["card"]["identity"]["id"], "tt100:1:2");
    assert!(next["card"]["badges"].as_array().unwrap().is_empty(), "{next}");

    watch(app, episode(2), Some(series()), true, now() + 2000).await;
    resolve(app, 2).await;
    let finished = show_entry(&queue(app).await);
    assert_eq!(finished["status"], "Upcoming", "{finished}");
    assert_eq!(finished["card"]["identity"]["id"], "tt100:1:2");
    assert_eq!(finished["card"]["badges"], json!(["Watched"]));
    let (status, home) = request(app, "member-token-1", "GET", "/api/profiles/1/home", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{home}");
    let row = home["rows"].as_array().unwrap().iter().find(|row| row["id"] == "continue").unwrap();
    assert_eq!(row["cards"][0]["identity"]["id"], "tt100:1:2");
    assert_eq!(row["cards"][0]["badges"], json!(["Watched"]));

    watch(app, episode(1), Some(series()), false, now() + 4000).await;
    let resumed = show_entry(&queue(app).await);
    assert_eq!(resumed["status"], "Resume", "{resumed}");
    assert!(resumed["card"]["badges"].as_array().unwrap().is_empty(), "{resumed}");
}

#[tokio::test]
async fn an_opened_episode_before_a_watched_one_leaves_the_series_at_the_watched_one() {
    let start = now();
    let show = show(vec![
        (1, start - 30 * DAY),
        (2, start - 20 * DAY),
        (3, start - 10 * DAY),
        (4, start + 7 * DAY),
    ])
    .await;
    let app = &show.app;
    for number in [1, 3] {
        app.db.lock().unwrap().execute("INSERT INTO progress(profile_id,id,type,name,poster,position,duration,updated_at,context,title_id) VALUES(1,?1,'series','Fixture Show',NULL,0,0,0,?2,'tt100')",params![format!("tt100:1:{number}"),json!({"season":1,"episode":number,"series_id":"tt100","watched_override":true,"stremio_import_watched":true,"stremio_completion_only":true,"stremio_watch_date_unknown":true}).to_string()]).unwrap();
    }
    let opened = |position: f64| {
        app.db.lock().unwrap().execute("INSERT INTO progress(profile_id,id,type,name,poster,position,duration,updated_at,context,title_id) VALUES(1,'tt100:1:2','series','Fixture Show',NULL,?1,1420.053,?2,?3,'tt100') ON CONFLICT(profile_id,type,id) DO UPDATE SET position=excluded.position",params![position,((start - 2 * DAY) / 1000) as i64,json!({"season":1,"episode":2,"series_id":"tt100"}).to_string()]).unwrap();
    };
    opened(0.135);
    let entry = show_entry(&queue(app).await);
    assert_eq!(entry["record"]["identity"]["id"], "tt100:1:3", "{entry}");
    assert_eq!(entry["card"]["identity"]["id"], "tt100:1:3");
    assert_ne!(entry["status"], "Resume");
    assert_eq!(entry["card"]["badges"], json!(["Watched"]));

    opened(600.0);
    let entry = show_entry(&queue(app).await);
    assert_eq!(entry["status"], "Resume", "{entry}");
    assert_eq!(entry["card"]["identity"]["id"], "tt100:1:2");
    assert!(entry["card"]["badges"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn home_refreshes_stale_release_facts_in_the_background_and_bumps_the_revision() {
    let start = now();
    let show = show(vec![(1, start - 30 * DAY), (2, start - DAY)]).await;
    let app = &show.app;
    watch(app, episode(1), Some(series()), true, start - 10 * DAY).await;
    let revision = || {
        app.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT revision FROM engine_library_revisions WHERE profile_id=1",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
    };
    let before = revision();
    let (status, home) = request(
        app,
        "member-token-1",
        "GET",
        "/api/profiles/1/home",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{home}");
    let mut badge = Value::Null;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        badge = show_entry(&queue(app).await)["card"]["badges"].clone();
        if badge == json!(["+1"]) {
            break;
        }
    }
    assert_eq!(badge, json!(["+1"]));
    assert!(revision() > before);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let calls = show.calls.load(Ordering::SeqCst);
    assert!((1..=2).contains(&calls), "{calls}");
    request(
        app,
        "member-token-1",
        "GET",
        "/api/profiles/1/home",
        Value::Null,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(show.calls.load(Ordering::SeqCst), calls);
}

#[tokio::test]
async fn a_removed_series_stays_removed_through_replays_without_a_new_release() {
    let start = now();
    let show = show(vec![(1, start - 30 * DAY), (2, start - 2 * DAY)]).await;
    let app = &show.app;
    watch(app, episode(1), Some(series()), true, start - 10 * DAY).await;
    resolve(app, 1).await;
    assert_eq!(
        show_entry(&queue(app).await)["card"]["badges"],
        json!(["+1"])
    );
    let (status, _) = request(
        app,
        "member-token-1",
        "PUT",
        "/api/profiles/1/continue/visibility",
        json!({"identity":episode(1),"parent_identity":series(),"hidden":true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    request(
        app,
        "member-token-1",
        "GET",
        "/api/profiles/1/home",
        Value::Null,
    )
    .await;
    assert!(queue(app).await.is_empty());
    watch(app, episode(1), Some(series()), true, now()).await;
    assert!(queue(app).await.is_empty());
}

#[tokio::test]
async fn imported_watched_history_is_a_finished_episode_for_the_refresh() {
    let start = now();
    let show = show(vec![(1, start - 30 * DAY), (2, start - DAY)]).await;
    let app = &show.app;
    app.db.lock().unwrap().execute("INSERT INTO progress(profile_id,id,type,name,poster,position,duration,updated_at,context,title_id) VALUES(1,'tt100:1:1','series','Fixture Show',NULL,5,100,?1,?2,'tt100')",params![((start - 10 * DAY) / 1000) as i64,json!({"season":1,"episode":1,"series_id":"tt100","stremio_import_watched":true}).to_string()]).unwrap();
    assert_eq!(show_entry(&queue(app).await)["status"], "Pending");
    request(
        app,
        "member-token-1",
        "GET",
        "/api/profiles/1/home",
        Value::Null,
    )
    .await;
    let mut badge = Value::Null;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        badge = show_entry(&queue(app).await)["card"]["badges"].clone();
        if badge == json!(["+1"]) {
            break;
        }
    }
    assert_eq!(badge, json!(["+1"]));
}

fn imported(app: &App, series: &str, number: u32, position: f64, duration: f64, at: u64) {
    app.db.lock().unwrap().execute("INSERT INTO progress(profile_id,id,type,name,poster,position,duration,updated_at,context,title_id) VALUES(1,?1,'series','Fixture Show',NULL,?2,?3,?4,?5,?6)",params![format!("{series}:1:{number}"),position,duration,(at / 1000) as i64,json!({"season":1,"episode":number,"series_id":series,"stremio_import_watched":true}).to_string(),series]).unwrap();
}

fn import(app: &App, series: &str, at: u64) {
    imported(app, series, 1, 5.0, 100.0, at);
}
fn verdict(app: &App, series: &str) -> Option<(String, &'static str)> {
    super::releases::verdicts_db(&app.db.lock().unwrap(), 1)
        .unwrap()
        .remove(series)
}
async fn until(mut done: impl FnMut() -> bool) {
    for _ in 0..200 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("background refresh did not finish");
}

#[tokio::test]
async fn imported_history_gets_a_durable_next_episode_verdict_for_the_watched_badge() {
    let start = now();
    let show = show(vec![(1, start - 30 * DAY), (2, start + 7 * DAY)]).await;
    let app = &show.app;
    import(app, "tt100", start - 10 * DAY);
    request(app, "member-token-1", "GET", "/api/profiles/1/home", Value::Null).await;
    super::releases::wait(app, 1).await;
    assert_eq!(verdict(app, "tt100"), Some(("tt100:1:1".to_owned(), "upcoming")));
    let cached: i64 = app.db.lock().unwrap().query_row("SELECT COUNT(*) FROM continuation_cache", [], |r| r.get(0)).unwrap();
    assert_eq!(cached, 0);
    assert_eq!(show_entry(&queue(app).await)["status"], "Pending");
    let card: viptv_engine::presentation::Card = serde_json::from_value(json!({"identity":series(),"provider_id":"addon:1","title":"Fixture Show","subtitle":null,"description":null,"poster":null,"landscape":null,"logo":null,"progress":null,"badges":[]})).unwrap();
    let watched = || super::super::watched::Watched::load(&app.db.lock().unwrap(), 1).unwrap();
    assert!(watched().contains(&card));
    let next_check: i64 = app.db.lock().unwrap().query_row("SELECT next_check_at FROM continuation_releases WHERE series_id='tt100'", [], |r| r.get(0)).unwrap();
    assert_eq!(next_check, ((start + 7 * DAY) / 1000) as i64 + 5 * 60);
    let calls = show.calls.load(Ordering::SeqCst);
    {
        let db = app.db.lock().unwrap();
        let raw: String = db.query_row("SELECT document FROM continuation_releases WHERE series_id='tt100'", [], |r| r.get(0)).unwrap();
        let mut document: Value = serde_json::from_str(&raw).unwrap();
        document["episodes"][1]["released_at_millis"] = json!(start - HOUR);
        db.execute("UPDATE continuation_releases SET document=?1 WHERE series_id='tt100'", [document.to_string()]).unwrap();
    }
    assert_eq!(verdict(app, "tt100").unwrap().1, "next");
    assert!(!watched().contains(&card));
    assert_eq!(show_entry(&queue(app).await)["card"]["badges"], json!(["+1"]));
    app.db.lock().unwrap().execute("UPDATE continuation_releases SET next_check_at=0", []).unwrap();
    request(app, "member-token-1", "GET", "/api/profiles/1/home", Value::Null).await;
    super::releases::wait(app, 1).await;
    assert!(show.calls.load(Ordering::SeqCst) > calls);
}

#[tokio::test]
async fn one_run_refreshes_every_finished_series_and_never_evicts_their_verdicts() {
    let start = now();
    let show = show(vec![(1, start - 30 * DAY)]).await;
    let app = &show.app;
    for n in 0..60 {
        import(app, &format!("tt{}", 5000 + n), start - (n as u64 + 1) * HOUR);
    }
    request(app, "member-token-1", "GET", "/api/profiles/1/home", Value::Null).await;
    let checked = || -> i64 {
        app.db.lock().unwrap().query_row("SELECT COUNT(*) FROM continuation_releases WHERE document IS NOT NULL", [], |r| r.get(0)).unwrap()
    };
    for _ in 0..600 {
        if checked() == 60 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(checked(), 60);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let calls = show.calls.load(Ordering::SeqCst);
    assert!((60..=72).contains(&calls), "{calls}");
    let verdicts = super::releases::verdicts_db(&app.db.lock().unwrap(), 1).unwrap();
    assert_eq!(verdicts.len(), 60);
    assert!(verdicts.values().all(|(_, status)| *status == "caught_up"));
    let soonest: i64 = app.db.lock().unwrap().query_row("SELECT MIN(next_check_at) FROM continuation_releases", [], |r| r.get(0)).unwrap();
    assert!(soonest >= (start / 1000) as i64 + 24 * 3600 - 60, "{soonest}");
    request(app, "member-token-1", "GET", "/api/profiles/1/home", Value::Null).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(show.calls.load(Ordering::SeqCst), calls);
}

fn restart(app: &App) {
    app.verified_cards.lock().unwrap().clear();
}

#[tokio::test]
async fn an_imported_finished_series_card_keeps_the_episode_still_after_a_restart() {
    let start = now();
    let show = show(vec![(1, start - 30 * DAY), (2, start + 7 * DAY)]).await;
    let app = &show.app;
    import(app, "tt100", start - 10 * DAY);
    request(app, "member-token-1", "GET", "/api/profiles/1/home", Value::Null).await;
    until(|| verdict(app, "tt100").is_some()).await;
    restart(app);
    let entry = show_entry(&queue(app).await);
    assert_eq!(entry["card"]["provider_id"], "addon:1", "{entry}");
    assert_eq!(entry["card"]["landscape"], "https://img.invalid/1.jpg", "{entry}");
    {
        let db = app.db.lock().unwrap();
        let raw: String = db.query_row("SELECT document FROM continuation_releases WHERE series_id='tt100'", [], |r| r.get(0)).unwrap();
        let mut document: Value = serde_json::from_str(&raw).unwrap();
        document["episodes"][0]["card"]["landscape"] = Value::Null;
        db.execute("UPDATE continuation_releases SET document=?1 WHERE series_id='tt100'", [document.to_string()]).unwrap();
    }
    let entry = show_entry(&queue(app).await);
    assert_eq!(entry["card"]["landscape"], "https://img.invalid/tt100/backdrop.jpg", "{entry}");
}

#[tokio::test]
async fn imported_titles_without_release_facts_use_their_my_list_backdrop() {
    let app = fixture();
    let start = now();
    let card = |kind: &str, id: &str| json!({"identity":{"id":id,"kind":kind,"ids":[],"episode":null,"edition":null},"provider_id":"library","title":"Saved","subtitle":null,"description":"Filled by the import","poster":null,"landscape":format!("https://img.invalid/{id}/backdrop.jpg"),"logo":null,"progress":null,"badges":[]});
    {
        let db = app.db.lock().unwrap();
        db.execute("INSERT INTO progress(profile_id,id,type,name,poster,position,duration,updated_at,context,title_id) VALUES(1,'tt100:1:2','series','Fixture Show',NULL,30,100,?1,?2,'tt100')",params![(start / 1000) as i64 - 60,json!({"season":1,"episode":2,"series_id":"tt100"}).to_string()]).unwrap();
        db.execute("INSERT INTO progress(profile_id,id,type,name,poster,position,duration,updated_at,context,title_id) VALUES(1,'tt200','movie','Fixture Film',NULL,30,100,?1,'{}','tt200')",params![(start / 1000) as i64 - 120]).unwrap();
        for (kind, id) in [("series", "tt100"), ("movie", "tt200")] {
            db.execute("INSERT INTO favorites(profile_id,id,type,name,poster,context) VALUES(1,?1,?2,'Saved',NULL,?3)", params![id, kind, card(kind, id).to_string()]).unwrap();
        }
    }
    let entries = queue(&app).await;
    assert_eq!(ids(&entries), ["tt100:1:2", "tt200"]);
    assert_eq!(entries[0]["status"], "Resume");
    assert_eq!(entries[0]["card"]["landscape"], "https://img.invalid/tt100/backdrop.jpg");
    assert_eq!(entries[1]["card"]["landscape"], "https://img.invalid/tt200/backdrop.jpg");
}

#[tokio::test]
async fn an_imported_series_simkl_cannot_map_still_gets_release_facts_on_a_simkl_profile() {
    let start = now();
    let mut show = show(vec![(1, start - 30 * DAY), (2, start - DAY)]).await;
    show.app.simkl = Arc::new(crate::simkl::Service::fixture("http://127.0.0.1:9"));
    let app = &show.app;
    let (status, integrations) =
        request(app, "member-token-1", "GET", "/api/profiles/1/integrations", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{integrations}");
    assert_eq!(integrations["configuration"]["browsing_plugin"], "simkl");
    import(app, "tt100", start - 10 * DAY);
    request(app, "member-token-1", "GET", "/api/profiles/1/home", Value::Null).await;
    until(|| verdict(app, "tt100").is_some()).await;
    assert_eq!(verdict(app, "tt100"), Some(("tt100:1:1".to_owned(), "next")));
    let failures: i64 = app.db.lock().unwrap().query_row("SELECT failures FROM continuation_releases WHERE series_id='tt100'", [], |r| r.get(0)).unwrap();
    assert_eq!(failures, 0);
    assert_eq!(show_entry(&queue(app).await)["card"]["badges"], json!(["+1"]));
}

#[tokio::test]
async fn a_series_simkl_maps_backs_off_while_simkl_fails_instead_of_using_the_add_ons() {
    let start = now();
    let mut show = show(vec![(1, start - 30 * DAY), (2, start - DAY)]).await;
    show.app.simkl = Arc::new(crate::simkl::Service::fixture("http://127.0.0.1:9"));
    let app = &show.app;
    import(app, "tt100", start - 10 * DAY);
    app.db.lock().unwrap().execute("INSERT INTO simkl_import_entries(profile_id,remote_id,remote_kind,local_id,local_kind,card,statistics,link_revision) VALUES(1,'simkl:tv:20','series','tt100','series','{}','{}',0)", []).unwrap();
    request(app, "member-token-1", "GET", "/api/profiles/1/home", Value::Null).await;
    let row = || -> Option<(Option<String>, i64)> {
        app.db.lock().unwrap().query_row("SELECT document,failures FROM continuation_releases WHERE series_id='tt100'", [], |r| Ok((r.get(0)?, r.get(1)?))).ok()
    };
    until(|| row().is_some_and(|(_, failures)| failures > 0)).await;
    assert_eq!(row(), Some((None, 1)));
    assert_eq!(verdict(app, "tt100"), None);
}

async fn next(app: &App, from: u32) -> Value {
    app.addons.forget_responses();
    let (status, next) = request(
        app,
        "member-token-1",
        "POST",
        "/api/profiles/1/continue/next",
        json!({"identity":episode(from),"parent_identity":series()}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{next}");
    next
}

#[tokio::test]
async fn next_episode_of_an_imported_series_simkl_cannot_map_comes_from_the_add_ons() {
    let start = now();
    let mut show = show(vec![(1, start - 30 * DAY), (2, start - DAY)]).await;
    show.app.simkl = Arc::new(crate::simkl::Service::fixture("http://127.0.0.1:9"));
    let app = &show.app;
    let (status, integrations) =
        request(app, "member-token-1", "GET", "/api/profiles/1/integrations", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{integrations}");
    assert_eq!(integrations["configuration"]["browsing_plugin"], "simkl");
    watch(app, episode(1), Some(series()), true, start - 10 * DAY).await;
    let resolved = next(app, 1).await;
    assert_eq!(resolved["status"], "Next", "{resolved}");
    assert_eq!(resolved["item"]["identity"]["id"], "tt100:1:2");
    assert_eq!(resolved["item"]["parent_identity"]["id"], "tt100");
    assert_eq!(show.calls.load(Ordering::SeqCst), 1);
    assert_eq!(show_entry(&queue(app).await)["card"]["badges"], json!(["+1"]));
}

#[tokio::test]
async fn next_episode_of_a_series_simkl_maps_still_comes_from_simkl() {
    let start = now();
    let mut show = show(vec![(1, start - 30 * DAY), (2, start - DAY)]).await;
    let routes = Router::new()
        .route("/tv/20", axum::routing::get(|| async { axum::Json(json!({"title":"Fixture Show","ids":{"simkl":20,"imdb":"tt100"}})) }))
        .route("/tv/episodes/20", axum::routing::get(|| async { axum::Json(json!([
            {"title":"Chapter 1","season":1,"episode":1,"aired":true,"ids":{"simkl":30}},
            {"title":"Chapter 2","season":1,"episode":2,"aired":false,"date":"2100-01-01","ids":{"simkl":31}}
        ])) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let simkl = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    show.app.simkl = Arc::new(crate::simkl::Service::fixture(&format!("http://{address}")));
    let app = &show.app;
    app.db.lock().unwrap().execute("INSERT INTO simkl_import_entries(profile_id,remote_id,remote_kind,local_id,local_kind,card,statistics,link_revision) VALUES(1,'simkl:tv:20','series','tt100','series','{}','{}',0)", []).unwrap();
    watch(app, episode(1), Some(series()), true, start - 10 * DAY).await;
    let resolved = next(app, 1).await;
    assert_eq!(resolved["status"], "Upcoming", "{resolved}");
    assert_eq!(show.calls.load(Ordering::SeqCst), 0);
    simkl.abort();
}

#[tokio::test]
async fn next_episode_on_a_stremio_profile_comes_from_its_add_ons() {
    let start = now();
    let show = show(vec![(1, start - 30 * DAY), (2, start - DAY)]).await;
    let app = &show.app;
    let (status, integrations) =
        request(app, "member-token-1", "GET", "/api/profiles/1/integrations", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{integrations}");
    assert_eq!(integrations["configuration"]["browsing_plugin"], "stremio");
    watch(app, episode(1), Some(series()), true, start - 10 * DAY).await;
    let resolved = next(app, 1).await;
    assert_eq!(resolved["status"], "Next", "{resolved}");
    assert_eq!(resolved["item"]["identity"]["id"], "tt100:1:2");
    assert_eq!(show.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_imported_finish_advances_to_the_next_released_episode() {
    let start = now();
    let show = show(vec![
        (1, start - 30 * DAY),
        (2, start - 20 * DAY),
        (3, start + 7 * DAY),
    ])
    .await;
    let app = &show.app;
    imported(app, "tt100", 1, 0.001, 1420.01, start - 10 * DAY);
    let film = json!({"id":"tt200","kind":"movie","ids":[],"episode":null,"edition":null});
    watch(app, film, None, false, start - 12 * DAY).await;
    let entry = show_entry(&queue(app).await);
    assert_eq!(entry["card"]["identity"]["id"], "tt100:1:1", "{entry}");
    assert_eq!(entry["card"]["badges"], json!(["Watched"]), "{entry}");
    request(
        app,
        "member-token-1",
        "GET",
        "/api/profiles/1/home",
        Value::Null,
    )
    .await;
    until(|| verdict(app, "tt100").is_some()).await;
    let entries = queue(app).await;
    assert_eq!(ids(&entries), ["tt100:1:2", "tt200"], "{entries:?}");
    let next = show_entry(&entries);
    assert_eq!(next["status"], "Next", "{next}");
    assert_eq!(next["card"]["identity"]["id"], "tt100:1:2", "{next}");
    assert_eq!(next["card"]["title"], "Fixture Show", "{next}");
    assert_eq!(next["card"]["subtitle"], "S1 E2 · Chapter 2", "{next}");
    assert!(next["card"]["badges"].as_array().unwrap().is_empty(), "{next}");
    assert!(next["card"]["progress"].is_null(), "{next}");
    assert_eq!(next["record"]["identity"]["id"], "tt100:1:1", "{next}");
    assert_eq!(next["previous_identity"]["id"], "tt100:1:1", "{next}");
    let (status, bootstrap) =
        request(app, "member-token-1", "GET", "/api/bootstrap", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{bootstrap}");
    let entries = bootstrap["continuation"]["entries"].as_array().unwrap();
    assert_eq!(ids(entries), ["tt100:1:2", "tt200"], "{bootstrap}");
}

#[tokio::test]
async fn an_imported_finish_with_no_released_next_episode_stays_caught_up() {
    let start = now();
    let show = show(vec![(1, start - 30 * DAY), (2, start + 7 * DAY)]).await;
    let app = &show.app;
    imported(app, "tt100", 1, 0.001, 1420.01, start - 10 * DAY);
    request(
        app,
        "member-token-1",
        "GET",
        "/api/profiles/1/home",
        Value::Null,
    )
    .await;
    until(|| verdict(app, "tt100").is_some()).await;
    assert_eq!(
        verdict(app, "tt100"),
        Some(("tt100:1:1".to_owned(), "upcoming"))
    );
    let entry = show_entry(&queue(app).await);
    assert_eq!(entry["card"]["identity"]["id"], "tt100:1:1", "{entry}");
    assert_eq!(entry["card"]["badges"], json!(["Watched"]), "{entry}");
    assert_eq!(entry["status"], "Pending", "{entry}");
    assert!(entry["card"]["subtitle"].is_null(), "{entry}");
}

#[tokio::test]
async fn a_prepared_target_projects_an_imported_row_the_release_document_cannot() {
    let start = now();
    let show = show(vec![
        (1, start - 30 * DAY),
        (2, start - 20 * DAY),
        (3, start + 7 * DAY),
    ])
    .await;
    let app = &show.app;
    imported(app, "tt100", 1, 0.001, 1420.01, start - 10 * DAY);
    app.addons.forget_responses();
    let (status, prepared) = request(
        app,
        "member-token-1",
        "POST",
        "/api/profiles/1/continue/next",
        json!({"identity":episode(1),"parent_identity":series(),"metadata_provider_id":"stremio","metadata_addon_id":1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{prepared}");
    assert_eq!(prepared["status"], "Next", "{prepared}");
    let cached: String = app
        .db
        .lock()
        .unwrap()
        .query_row("SELECT result FROM continuation_cache WHERE profile_id=1 AND series_id='tt100'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&cached).unwrap()["metadata_origin"],
        "addon:1"
    );
    app.db
        .lock()
        .unwrap()
        .execute("DELETE FROM continuation_releases WHERE series_id='tt100'", [])
        .unwrap();
    let entry = show_entry(&queue(app).await);
    assert_eq!(entry["status"], "Next", "{entry}");
    assert_eq!(entry["card"]["identity"]["id"], "tt100:1:2", "{entry}");
    assert_eq!(entry["card"]["subtitle"], "S1 E2 · Chapter 2", "{entry}");
    assert_eq!(entry["record"]["identity"]["id"], "tt100:1:1", "{entry}");
}

#[tokio::test]
async fn the_up_next_entry_carries_the_documents_origin() {
    let start = now();
    let show = show(vec![(1, start - 30 * DAY), (2, start - DAY)]).await;
    let app = &show.app;
    watch(app, episode(1), Some(series()), true, start - 10 * DAY).await;
    resolve(app, 1).await;
    let entry = show_entry(&queue(app).await);
    assert_eq!(entry["status"], "Next", "{entry}");
    assert_eq!(entry["card"]["identity"]["id"], "tt100:1:2", "{entry}");
    assert_eq!(entry["card"]["metadata_origin"]["provider_id"], "stremio", "{entry}");
    assert_eq!(entry["card"]["metadata_origin"]["connection_id"], "addon:1", "{entry}");
    assert_eq!(entry["card"]["provider_id"], "addon:1", "{entry}");
}

#[tokio::test]
async fn an_entry_simkl_identifies_carries_the_simkl_origin() {
    let start = now();
    let mut show = show(vec![(1, start - 30 * DAY), (2, start - DAY)]).await;
    show.app.simkl = Arc::new(crate::simkl::Service::fixture("http://127.0.0.1:9"));
    let app = &show.app;
    {
        let db = app.db.lock().unwrap();
        db.execute_batch(
            "INSERT INTO simkl_links(profile_id,account_id,revision,credential) VALUES(1,1,1,'{}');
             INSERT INTO simkl_import_entries(profile_id,remote_id,remote_kind,local_id,local_kind,card,statistics,link_revision) VALUES(1,'simkl:tv:20','series','tt100','series','{}','{}',1);",
        ).unwrap();
        db.execute(
            "INSERT INTO progress(profile_id,id,type,name,poster,position,duration,updated_at,context,title_id) VALUES(1,'tt100:1:2','series','Fixture Show',NULL,30,100,?1,'{\"season\":1,\"episode\":2,\"series_id\":\"tt100\"}','tt100')",
            params![(start / 1000) as i64],
        ).unwrap();
    }
    let entries = queue(app).await;
    let entry = entries
        .iter()
        .find(|entry| entry["card"]["identity"]["id"] == "tt100:1:2")
        .unwrap_or_else(|| panic!("no resumed entry: {entries:?}"));
    assert_eq!(entry["status"], "Resume", "{entry}");
    assert_eq!(entry["card"]["metadata_origin"]["provider_id"], "simkl", "{entry}");
    assert!(entry["card"]["metadata_origin"]["connection_id"].is_null(), "{entry}");
    assert_eq!(entry["card"]["provider_id"], "simkl", "{entry}");
}

#[tokio::test]
async fn a_caught_up_entry_carries_the_next_episode_and_its_release_date() {
    let start = now();
    let aired = start + 7 * DAY;
    let show = show(vec![(1, start - 30 * DAY), (2, aired)]).await;
    let app = &show.app;
    imported(app, "tt100", 1, 0.001, 1420.01, start - 10 * DAY);
    request(app, "member-token-1", "GET", "/api/profiles/1/home", Value::Null).await;
    until(|| verdict(app, "tt100").is_some()).await;
    let entry = show_entry(&queue(app).await);
    assert_eq!(entry["status"], "Pending", "{entry}");
    assert_eq!(entry["card"]["identity"]["id"], "tt100:1:1", "{entry}");
    assert_eq!(entry["card"]["badges"], json!(["Watched"]), "{entry}");
    assert_eq!(entry["next_episode"]["identity"]["id"], "tt100:1:2", "{entry}");
    assert_eq!(
        entry["next_episode"]["identity"]["episode"],
        json!({"order":"aired","season":1,"episode":2}),
        "{entry}"
    );
    assert_eq!(entry["next_episode"]["released_at_millis"], aired, "{entry}");
    let (status, bootstrap) =
        request(app, "member-token-1", "GET", "/api/bootstrap", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{bootstrap}");
    let entry = show_entry(bootstrap["continuation"]["entries"].as_array().unwrap());
    assert_eq!(entry["next_episode"]["identity"]["id"], "tt100:1:2", "{bootstrap}");
    assert_eq!(entry["next_episode"]["released_at_millis"], aired, "{bootstrap}");
}

#[tokio::test]
async fn a_finished_or_up_next_entry_announces_no_next_episode() {
    let start = now();
    let last = show(vec![(1, start - 30 * DAY)]).await;
    let app = &last.app;
    watch(app, episode(1), Some(series()), true, start - 10 * DAY).await;
    resolve(app, 1).await;
    let entry = show_entry(&queue(app).await);
    assert_eq!(entry["status"], "CaughtUp", "{entry}");
    assert!(entry["next_episode"].is_null(), "{entry}");

    let released = show(vec![(1, start - 30 * DAY), (2, start - DAY)]).await;
    let app = &released.app;
    watch(app, episode(1), Some(series()), true, start - 10 * DAY).await;
    resolve(app, 1).await;
    let entry = show_entry(&queue(app).await);
    assert_eq!(entry["status"], "Next", "{entry}");
    assert_eq!(entry["card"]["identity"]["id"], "tt100:1:2", "{entry}");
    assert!(entry["next_episode"].is_null(), "{entry}");
}
