use super::anime_show_migration::run;
use super::anime_show_tests::stack;
use crate::*;

const SHOW: &str = "tt2560140";

fn insert(app: &App, id: &str, title_id: &str, context: Value, position: f64, updated_at: i64) {
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO progress(profile_id,id,type,name,poster,position,duration,updated_at,context,title_id) VALUES(1,?1,'series',?2,'/poster.jpg',?3,24.0,?4,?5,?6)",
            params![id, format!("Episode {id}"), position, updated_at, context.to_string(), title_id],
        )
        .unwrap();
}

fn played(app: &App, id: &str, release: &str, season: u64, episode: u64, position: f64, at: i64) {
    insert(
        app,
        id,
        release,
        json!({"series_id":release,"season":season,"episode":episode,"engine_provider_id":"library"}),
        position,
        at,
    );
}

fn imported(app: &App, id: &str, release: &str, season: u64, episode: u64, position: f64, at: i64) {
    insert(
        app,
        id,
        release,
        json!({
            "engine_identity":{"id":id,"kind":"series","ids":[],"episode":{"order":"aired","season":season,"episode":episode},"edition":null},
            "engine_parent_identity":{"id":release,"kind":"series","ids":[],"episode":null,"edition":null},
            "engine_provider_id":"simkl",
            "series_id":release,
            "season":season,
            "episode":episode,
            "watched_override":true,
        }),
        position,
        at,
    );
}

fn saved(app: &App, id: &str, added_at: i64) {
    let (namespace, value) = id.split_once(':').unwrap();
    let card = json!({
        "identity":{"id":id,"kind":"series","ids":[{"namespace":namespace,"value":value}],"episode":null,"edition":null},
        "provider_id":"library",
        "title":format!("Title {id}"),
        "poster":"/poster.jpg",
        "badges":[],
    });
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO favorites(profile_id,id,type,name,poster,context,added_at_millis) VALUES(1,?1,'series',?2,'/poster.jpg',?3,?4)",
            params![id, format!("Title {id}"), card.to_string(), added_at],
        )
        .unwrap();
}

fn episodes(app: &App) -> Vec<(String, String)> {
    let db = app.db.lock().unwrap();
    let mut query = db
        .prepare("SELECT id,title_id FROM progress WHERE profile_id=1 ORDER BY id")
        .unwrap();
    let rows = query
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    rows
}

fn saved_titles(app: &App) -> Vec<String> {
    let db = app.db.lock().unwrap();
    let mut query = db
        .prepare("SELECT id FROM favorites WHERE profile_id=1 ORDER BY id")
        .unwrap();
    let rows = query
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    rows
}

fn context_of(app: &App, id: &str) -> Value {
    let raw: String = app
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT context FROM progress WHERE profile_id=1 AND id=?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap();
    serde_json::from_str(&raw).unwrap()
}

fn snapshot(app: &App) -> Vec<(String, String, String, f64, i64)> {
    let db = app.db.lock().unwrap();
    let mut query = db
        .prepare("SELECT id,title_id,context,position,updated_at FROM progress WHERE profile_id=1 ORDER BY id")
        .unwrap();
    query
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn show_document() -> Value {
    json!({"videos":[
        {"id":"tt2560140:1:1","season":1,"episode":1,"title":"To You, in 2000 Years"},
        {"id":"tt2560140:3:13","season":3,"episode":13,"title":"The Town Where It Began"},
        {"id":"tt2560140:3:14","season":3,"episode":14,"title":"The Thunder Spear"},
        {"id":"tt2560140:3:15","season":3,"episode":15,"title":"Descent"}
    ]})
}

#[tokio::test]
async fn three_cours_become_one_saved_title_one_queue_entry_and_keep_every_episode() {
    let (app, task) = stack().await;
    for (index, release) in ["mal:16498", "mal:38524", "simkl:anime:5000"].iter().enumerate() {
        saved(&app, release, index as i64 + 1);
    }
    played(&app, "mal:16498:1:1", "mal:16498", 1, 1, 24.0, 100);
    played(&app, "mal:38524:1:1", "mal:38524", 1, 1, 12.0, 200);
    imported(&app, "simkl:anime:5000:3:14", "simkl:anime:5000", 3, 14, 24.0, 300);

    run(&app).await.unwrap();

    assert_eq!(saved_titles(&app), vec![SHOW]);
    assert_eq!(
        episodes(&app),
        vec![
            ("tt2560140:1:1".into(), SHOW.into()),
            ("tt2560140:3:13".into(), SHOW.into()),
            ("tt2560140:3:14".into(), SHOW.into()),
        ]
    );
    let first = context_of(&app, "tt2560140:1:1");
    assert_eq!(first["season"], 1);
    assert_eq!(first["episode"], 1);
    assert_eq!(first["series_id"], SHOW);
    assert_eq!(first["engine_identity"]["episode"]["season"], 1);
    assert_eq!(first["engine_parent_identity"]["id"], SHOW);
    let ids = first["engine_identity"]["ids"].as_array().unwrap();
    for id in [
        json!({"namespace":"imdb","value":"tt2560140"}),
        json!({"namespace":"tvdb","value":"267440"}),
        json!({"namespace":"tmdb","value":"1429"}),
        json!({"namespace":"mal","value":"16498"}),
    ] {
        assert!(ids.contains(&id), "{id} missing from {ids:?}");
    }

    let queue = {
        let db = app.db.lock().unwrap();
        crate::continuation::queue(&db, 1, 0, 50).unwrap()
    };
    let items = queue["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{queue}");
    assert_eq!(items[0]["queue_title_id"], SHOW);
    assert_eq!(items[0]["id"], "tt2560140:3:14");
    let next = crate::continuation::resolve(&items[0], &show_document(), 0);
    assert_eq!(next["status"], "next", "{next}");
    assert_eq!(next["item"]["id"], "tt2560140:3:15");
    task.abort();
}

#[tokio::test]
async fn two_releases_of_one_episode_keep_the_newest_state() {
    let (app, task) = stack().await;
    played(&app, "simkl:anime:5000:3:13", "simkl:anime:5000", 3, 13, 6.0, 100);
    played(&app, "mal:38524:1:1", "mal:38524", 1, 1, 20.0, 200);

    run(&app).await.unwrap();

    assert_eq!(
        episodes(&app),
        vec![("tt2560140:3:13".into(), SHOW.into())]
    );
    let position: f64 = app
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT position FROM progress WHERE profile_id=1 AND id='tt2560140:3:13'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(position, 20.0);
    assert_eq!(context_of(&app, "tt2560140:3:13")["season"], 3);
    assert_eq!(context_of(&app, "tt2560140:3:13")["episode"], 13);
    task.abort();
}

#[tokio::test]
async fn running_the_migration_again_changes_nothing() {
    let (app, task) = stack().await;
    saved(&app, "mal:16498", 1);
    saved(&app, "mal:38524", 2);
    played(&app, "mal:16498:1:1", "mal:16498", 1, 1, 24.0, 100);
    played(&app, "mal:38524:1:1", "mal:38524", 1, 1, 12.0, 200);
    imported(&app, "simkl:anime:5000:3:14", "simkl:anime:5000", 3, 14, 3.0, 300);

    run(&app).await.unwrap();
    let before = snapshot(&app);
    run(&app).await.unwrap();

    assert_eq!(snapshot(&app), before);
    assert_eq!(saved_titles(&app), vec![SHOW]);
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM favorites WHERE profile_id=1", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    task.abort();
}

#[tokio::test]
async fn an_episode_keyed_only_by_its_id_still_finds_its_show() {
    let (app, task) = stack().await;
    insert(
        &app,
        "mal:16498:1:2",
        "",
        json!({"season":1,"episode":2,"engine_provider_id":"library"}),
        24.0,
        100,
    );

    run(&app).await.unwrap();

    assert_eq!(
        episodes(&app),
        vec![("tt2560140:1:2".into(), SHOW.into())]
    );
    task.abort();
}

#[tokio::test]
async fn a_release_without_a_tvdb_or_tmdb_mapping_stays_as_it_is() {
    let (app, task) = stack().await;
    saved(&app, "mal:9", 1);
    played(&app, "mal:9:1:1", "mal:9", 1, 1, 5.0, 100);

    run(&app).await.unwrap();

    assert_eq!(saved_titles(&app), vec!["mal:9"]);
    assert_eq!(
        episodes(&app),
        vec![("mal:9:1:1".into(), "mal:9".into())]
    );
    task.abort();
}

#[tokio::test]
async fn a_title_hidden_from_continue_watching_stays_hidden_under_the_show() {
    let (app, task) = stack().await;
    {
        let db = app.db.lock().unwrap();
        db.execute(
            "INSERT INTO queue_hidden(profile_id,type,title_id) VALUES(1,'series','mal:16498')",
            [],
        )
        .unwrap();
        db.execute(
            "UPDATE queue_hidden_times SET hidden_at=1234 WHERE profile_id=1",
            [],
        )
        .unwrap();
    }

    run(&app).await.unwrap();

    let db = app.db.lock().unwrap();
    let hidden: Vec<String> = {
        let mut query = db
            .prepare("SELECT title_id FROM queue_hidden WHERE profile_id=1")
            .unwrap();
        query
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(hidden, vec![SHOW]);
    let times: Vec<(String, i64)> = {
        let mut query = db
            .prepare("SELECT title_id,hidden_at FROM queue_hidden_times WHERE profile_id=1")
            .unwrap();
        query
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(times, vec![(SHOW.to_owned(), 1234)]);
    task.abort();
}
