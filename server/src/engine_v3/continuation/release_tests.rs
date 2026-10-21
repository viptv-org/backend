//! Continue Watching promotes a caught-up series once per new release and
//! counts later releases without moving it again.
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

/// A metadata add-on whose episode release times the test controls.
async fn show(releases: Vec<(u32, u64)>) -> Show {
    let mut app = fixture();
    app.addons.allow_test_loopback = true;
    let releases = Arc::new(Mutex::new(releases));
    let calls = Arc::new(AtomicUsize::new(0));
    let (served, counted) = (releases.clone(), calls.clone());
    let routes = Router::new().route(
        "/meta/series/tt100.json",
        axum::routing::get(move || {
            let videos = served
                .lock()
                .unwrap()
                .iter()
                .map(|(number, at)| json!({"id":format!("tt100:1:{number}"),"title":format!("Chapter {number}"),"season":1,"episode":number,"released":iso(*at),"thumbnail":format!("https://img.invalid/{number}.jpg")}))
                .collect::<Vec<_>>();
            counted.fetch_add(1, Ordering::SeqCst);
            async move {
                axum::Json(json!({"meta":{"id":"tt100","type":"series","name":"Fixture Show","videos":videos}}))
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

/// Saves playback of an episode (finished when `ended`) or a movie, then
/// places that activity at `at` on the queue's clock.
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

/// What a client resolves when playback of `from` ends; it also records the
/// release facts Continue Watching reads.
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

    // Caught up: nothing new, the film watched later stays first.
    let entries = queue(app).await;
    assert_eq!(ids(&entries), ["tt200", "tt100:1:1"]);
    let caught_up = show_entry(&entries);
    assert!(
        caught_up["card"]["badges"].as_array().unwrap().is_empty(),
        "{caught_up}"
    );

    // Episode 2 is released: the series moves to the front with a badge and
    // opens episode 2, titled by the series with the episode as caption.
    show.releases.lock().unwrap()[1].1 = start - 2 * DAY;
    resolve(app, 1).await;
    let entries = queue(app).await;
    assert_eq!(ids(&entries), ["tt100:1:2", "tt200"]);
    let new = show_entry(&entries);
    assert_eq!(new["status"], "Next");
    assert_eq!(new["card"]["badges"], json!(["New episode"]));
    assert_eq!(new["card"]["title"], "Fixture Show");
    assert_eq!(new["card"]["subtitle"], "S1 E2 · Chapter 2");
    assert_eq!(new["card"]["parent_identity"]["id"], "tt100");
    assert!(new["card"]["progress"].is_null());
    assert_eq!(new["previous_identity"]["id"], "tt100:1:1");
    assert_eq!(new["record"]["identity"]["id"], "tt100:1:1");

    // Another title is watched after that release, then episode 3 is released
    // while episode 2 is still unwatched: the count grows, the position does not.
    let other = json!({"id":"tt300","kind":"movie","ids":[],"episode":null,"edition":null});
    watch(app, other, None, false, start - DAY).await;
    show.releases.lock().unwrap()[2].1 = start - HOUR;
    resolve(app, 1).await;
    let entries = queue(app).await;
    assert_eq!(ids(&entries), ["tt300", "tt100:1:2", "tt200"]);
    assert_eq!(
        show_entry(&entries)["card"]["badges"],
        json!(["+2 new episodes"])
    );

    // Home carries the same card, so every client renders the badge.
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
    assert_eq!(row["cards"][1]["badges"], json!(["+2 new episodes"]));

    // Removing it hides it until an episode is released after the removal.
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
        json!(["+3 new episodes"])
    );
    // The next Home load un-hides it for good, so playing it keeps it listed.
    request(
        app,
        "member-token-1",
        "GET",
        "/api/profiles/1/home",
        Value::Null,
    )
    .await;
    let hidden: i64 = app
        .db
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM queue_hidden", [], |r| r.get(0))
        .unwrap();
    assert_eq!(hidden, 0);

    // Watching episode 2 leaves a backlog that was already out: no badge.
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
    // Episode 2 had aired before episode 1 was finished: behind, not new.
    watch(app, episode(1), Some(series()), true, start - 10 * DAY).await;
    resolve(app, 1).await;
    let entries = queue(app).await;
    assert!(show_entry(&entries)["card"]["badges"]
        .as_array()
        .unwrap()
        .is_empty());
    // An unfinished episode is resumed, never promoted.
    watch(app, episode(2), Some(series()), false, start - 2 * DAY).await;
    let entries = queue(app).await;
    assert_eq!(show_entry(&entries)["status"], "Resume");
    assert!(show_entry(&entries)["card"]["badges"]
        .as_array()
        .unwrap()
        .is_empty());
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
        if badge != json!([]) {
            break;
        }
    }
    assert_eq!(badge, json!(["New episode"]));
    assert!(revision() > before);
    assert_eq!(show.calls.load(Ordering::SeqCst), 1);
    // A fresh document is not fetched again by the next Home load.
    request(
        app,
        "member-token-1",
        "GET",
        "/api/profiles/1/home",
        Value::Null,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(show.calls.load(Ordering::SeqCst), 1);
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
        json!(["New episode"])
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
    // A Stremio-imported episode: watched by flag, with a partial position.
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
        if badge != json!([]) {
            break;
        }
    }
    assert_eq!(badge, json!(["New episode"]));
}

#[tokio::test]
async fn the_refresh_records_next_episode_verdicts_for_imported_history_without_preparing_targets() {
    let start = now();
    // Imported, finished on the newest episode: caught up, so the series is Watched.
    let show = show(vec![(1, start - 30 * DAY), (2, start + 7 * DAY)]).await;
    let app = &show.app;
    app.db.lock().unwrap().execute("INSERT INTO progress(profile_id,id,type,name,poster,position,duration,updated_at,context,title_id) VALUES(1,'tt100:1:1','series','Fixture Show',NULL,5,100,?1,?2,'tt100')",params![((start - 10 * DAY) / 1000) as i64,json!({"season":1,"episode":1,"series_id":"tt100","stremio_import_watched":true}).to_string()]).unwrap();
    request(app, "member-token-1", "GET", "/api/profiles/1/home", Value::Null).await;
    let mut verdict = None;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        verdict = app
            .db
            .lock()
            .unwrap()
            .query_row("SELECT json_extract(result,'$.status'),updated_at FROM continuation_cache WHERE profile_id=1 AND series_id='tt100' AND from_id='tt100:1:1'", [], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .optional()
            .unwrap();
        if verdict.is_some() {
            break;
        }
    }
    // A background verdict is a fact, not a prepared target.
    assert_eq!(verdict, Some(("upcoming".to_owned(), 0)));
    let card: viptv_engine::presentation::Card = serde_json::from_value(json!({"identity":series(),"provider_id":"addon:1","title":"Fixture Show","subtitle":null,"description":null,"poster":null,"landscape":null,"logo":null,"progress":null,"badges":[]})).unwrap();
    let watched = super::super::watched::Watched::load(&app.db.lock().unwrap(), 1).unwrap();
    assert!(watched.contains(&card));
    let entries = queue(app).await;
    assert_eq!(show_entry(&entries)["status"], "Pending");
    // Episode 2 airs: the next check now says `next`, so the series is not Watched.
    show.releases.lock().unwrap()[1].1 = start - HOUR;
    app.addons.forget_responses();
    app.db.lock().unwrap().execute("UPDATE continuation_releases SET checked_at=0", []).unwrap();
    request(app, "member-token-1", "GET", "/api/profiles/1/home", Value::Null).await;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let status: String = app.db.lock().unwrap().query_row("SELECT json_extract(result,'$.status') FROM continuation_cache WHERE series_id='tt100'", [], |r| r.get(0)).unwrap();
        if status == "next" {
            break;
        }
    }
    let watched = super::super::watched::Watched::load(&app.db.lock().unwrap(), 1).unwrap();
    assert!(!watched.contains(&card));
    assert_eq!(show_entry(&queue(app).await)["card"]["badges"], json!(["New episode"]));
}
