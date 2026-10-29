use super::watched::{Watched, BADGE};
use crate::{auth_integration_tests::fixture, test_support::request, *};
use viptv_engine::presentation::Card;

fn card(kind: &str, id: &str) -> Card {
    serde_json::from_value(json!({"identity":{"id":id,"kind":kind,"ids":[],"episode":null,"edition":null},
        "provider_id":"addon:1","title":id,"subtitle":null,"description":null,"poster":null,"landscape":null,
        "logo":null,"progress":null,"badges":[]})).unwrap()
}
fn progress(db: &Connection, profile: i64, kind: &str, id: &str, title: &str, position: f64, duration: f64, updated: i64, context: Value) {
    db.execute("INSERT INTO progress(profile_id,id,type,name,poster,position,duration,updated_at,context,title_id) VALUES(?1,?2,?3,'Title',NULL,?4,?5,?6,?7,?8)",
        params![profile, id, kind, position, duration, updated, context.to_string(), title]).unwrap();
}
fn verdict(db: &Connection, profile: i64, series: &str, from: &str, status: &str) {
    db.execute("INSERT INTO continuation_cache(profile_id,series_id,from_id,result,updated_at) VALUES(?1,?2,?3,?4,0)",
        params![profile, series, from, json!({"status":status}).to_string()]).unwrap();
}

#[tokio::test]
async fn watched_titles_follow_existing_completion_and_caught_up_facts() {
    let app = fixture();
    let db = app.db.lock().unwrap();
    db.execute("INSERT INTO profiles(id,name,avatar_seed,presentation_complete) VALUES(2,'Other','fixture-two',1)", []).unwrap();
    progress(&db, 1, "movie", "tt0000001", "tt0000001", 96.0, 100.0, 1, json!({}));
    progress(&db, 1, "movie", "tt0000002", "tt0000002", 50.0, 100.0, 2, json!({}));
    progress(&db, 1, "movie", "tt0000003", "tt0000003", 10.0, 100.0, 3, json!({"watched_override":true,"progress_corrected":true}));
    progress(&db, 1, "movie", "tt0000004", "tt0000004", 0.0, 0.0, 4, json!({"stremio_import_watched":true,"imdb_id":"tt0000004"}));
    progress(&db, 1, "movie", "tt0000005", "tt0000005", 99.0, 100.0, 5, json!({"watched_override":false,"progress_corrected":true}));
    progress(&db, 1, "series", "tt0000010:1:1", "tt0000010", 100.0, 100.0, 10, json!({}));
    progress(&db, 1, "series", "tt0000010:1:2", "tt0000010", 100.0, 100.0, 11, json!({}));
    verdict(&db, 1, "tt0000010", "tt0000010:1:2", "caught_up");
    progress(&db, 1, "series", "tt0000011:1:5", "tt0000011", 100.0, 100.0, 12, json!({}));
    verdict(&db, 1, "tt0000011", "tt0000011:1:5", "upcoming");
    progress(&db, 1, "series", "tt0000012:1:5", "tt0000012", 100.0, 100.0, 13, json!({}));
    verdict(&db, 1, "tt0000012", "tt0000012:1:5", "next");
    progress(&db, 1, "series", "tt0000013:1:1", "tt0000013", 100.0, 100.0, 14, json!({"stremio_import_watched":true}));
    progress(&db, 1, "series", "tt0000014:1:9", "tt0000014", 100.0, 100.0, 15, json!({}));
    verdict(&db, 1, "tt0000014", "tt0000014:1:9", "caught_up");
    progress(&db, 1, "series", "tt0000014:1:1", "tt0000014", 30.0, 100.0, 16, json!({}));
    for (remote, local, unwatched) in [("simkl:tv:1", "tt0000020", false), ("simkl:tv:2", "tt0000021", true)] {
        let mut statistics = viptv_engine::library::LibraryStatistics::empty(card("series", local).identity);
        statistics.watched = true;
        statistics.status = Some("completed".into());
        statistics.has_unwatched_aired_episodes = Some(unwatched);
        db.execute("INSERT INTO simkl_import_entries VALUES(1,?1,'series',?2,'series','{}',?3,1)",
            params![remote, local, serde_json::to_string(&statistics).unwrap()]).unwrap();
    }
    progress(&db, 2, "movie", "tt0000030", "tt0000030", 100.0, 100.0, 20, json!({}));

    let watched = Watched::load(&db, 1).unwrap();
    let expect = [
        ("movie", "tt0000001", true), ("movie", "tt0000002", false), ("movie", "tt0000003", true),
        ("movie", "tt0000004", true), ("movie", "tt0000005", false), ("movie", "tt0000030", false),
        ("series", "tt0000010", true), ("series", "tt0000011", true), ("series", "tt0000012", false),
        ("series", "tt0000013", false), ("series", "tt0000014", false), ("series", "tt0000020", true),
        ("series", "tt0000021", false), ("movie", "tt0000010", false), ("movie", "tt9999999", false),
    ];
    for (kind, id, expected) in expect {
        assert_eq!(watched.contains(&card(kind, id)), expected, "{kind} {id}");
    }
    assert!(Watched::load(&db, 2).unwrap().contains(&card("movie", "tt0000030")));

    let mut hinted = card("movie", "tmdb:42");
    hinted.identity.ids = serde_json::from_value(json!([{"namespace":"imdb","value":"tt0000004"}])).unwrap();
    assert!(watched.contains(&hinted));
    let mut episode = card("series", "tt0000010:1:2");
    episode.identity.episode = serde_json::from_value(json!({"order":"aired","season":1,"episode":2})).unwrap();
    assert!(!watched.contains(&episode));

    let mut stale = card("movie", "tt0000002");
    stale.badges = vec!["4K".into(), "watched".into()];
    watched.mark(&mut stale);
    assert_eq!(stale.badges, vec!["4K".to_owned()]);
    let mut done = card("movie", "tt0000001");
    done.badges = vec![BADGE.into()];
    watched.mark(&mut done);
    watched.mark(&mut done);
    assert_eq!(done.badges, vec![BADGE.to_owned()]);
    let mut full = card("movie", "tt0000001");
    full.badges = (0..32).map(|n| format!("b{n}")).collect();
    watched.mark(&mut full);
    assert_eq!(full.badges.len(), 32, "never exceed the card contract's badge bound");
}

#[tokio::test]
async fn catalog_home_search_and_library_cards_carry_the_profile_watched_badge() {
    let metas = json!({"metas":[
        {"id":"tt1000001","type":"movie","name":"Finished film"},
        {"id":"tt1000002","type":"movie","name":"Half film"},
        {"id":"tt1000003","type":"movie","name":"Unseen film"}]});
    let mock = Router::new().fallback(move || { let metas = metas.clone(); async move { axum::Json(metas) } });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let mut app = fixture();
    app.addons.allow_test_loopback = true;
    let manifest = json!({"id":"fixture","name":"Fixture","resources":["catalog","meta"],"types":["movie"],
        "catalogs":[{"id":"top","name":"Popular","type":"movie","extra":[{"name":"search"},{"name":"skip"}]}]});
    {
        let db = app.db.lock().unwrap();
        db.execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id) VALUES(1,'Fixture',?1,?2,1)",
            params![format!("http://{address}/manifest.json"), manifest.to_string()]).unwrap();
        progress(&db, 1, "movie", "tt1000001", "tt1000001", 100.0, 100.0, 1, json!({}));
        progress(&db, 1, "movie", "tt1000002", "tt1000002", 40.0, 100.0, 2, json!({}));
        progress(&db, 1, "movie", "tt1000004", "tt1000004", 40.0, 100.0, 3, json!({"stremio_import_watched":true}));
    }
    crate::test_support::encrypt_fixture_sources(&app);
    let (status, body) = request(&app, "member-token-1", "PUT", "/api/profiles/1/integrations",
        json!({"revision":0,"browsing_plugin":"stremio","assignments":[{"connection_id":"addon:1","enabled":["Metadata","Discovery","Search"]}]})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let badges = |cards: &Value, id: &str| -> Vec<String> {
        let card = cards.as_array().unwrap().iter().find(|c| c["identity"]["id"] == id)
            .unwrap_or_else(|| panic!("{id} missing from {cards}"));
        serde_json::from_value(card["badges"].clone()).unwrap()
    };
    let watched = vec![BADGE.to_owned()];

    let (status, page) = request(&app, "member-token-1", "GET", "/api/discover?type=movie&addon_id=1&catalog=top", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(badges(&page["cards"], "tt1000001"), watched);
    assert!(badges(&page["cards"], "tt1000002").is_empty());
    assert!(badges(&page["cards"], "tt1000003").is_empty());

    let (status, home) = request(&app, "member-token-1", "GET", "/api/profiles/1/home", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{home}");
    let row = home["rows"].as_array().unwrap().iter().find(|r| r["id"] == "addon:1:movie:top").expect("catalog row");
    assert_eq!(badges(&row["cards"], "tt1000001"), watched);
    assert!(badges(&row["cards"], "tt1000003").is_empty());
    let resume = home["rows"].as_array().unwrap().iter().find(|r| r["id"] == "continue").expect("continue row");
    assert!(badges(&resume["cards"], "tt1000004").is_empty(), "an in-progress rewatch is not drawn as watched");

    let (status, search) = request(&app, "member-token-1", "GET", "/api/search?query=film", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{search}");
    let cards = &search["rows"][0]["cards"];
    assert_eq!(badges(cards, "tt1000001"), watched);
    assert!(badges(cards, "tt1000002").is_empty());

    for id in ["tt1000001", "tt1000003"] {
        let mut saved = serde_json::to_value(card("movie", id)).unwrap();
        saved["badges"] = json!([BADGE]);
        let (status, body) = request(&app, "member-token-1", "PUT", "/api/profiles/1/library", json!({"card":saved})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let (status, library) = request(&app, "member-token-1", "GET", "/api/profiles/1/library", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{library}");
    assert_eq!(badges(&library["cards"], "tt1000001"), watched);
    assert!(badges(&library["cards"], "tt1000003").is_empty());

    let (status, body) = request(&app, "member-token-1", "PUT", "/api/profiles/1/progress/correct",
        json!({"id":"tt1000001","type":"movie","name":"Finished film","action":"unwatched"})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, page) = request(&app, "member-token-1", "GET", "/api/discover?type=movie&addon_id=1&catalog=top", Value::Null).await;
    assert!(badges(&page["cards"], "tt1000001").is_empty());
    server.abort();
}
