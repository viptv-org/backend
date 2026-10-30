use crate::{auth_integration_tests::fixture, engine_v3::art, test_support::request, *};

fn poster(id: &str) -> String {
    format!("https://images.metahub.space/poster/small/{id}/img")
}
fn background(id: &str) -> String {
    format!("https://images.metahub.space/background/medium/{id}/img")
}
async fn get(app: &App, path: &str) -> Value {
    let (status, value) = request(app, "member-token-1", "GET", path, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value
}
fn card<'a>(page: &'a Value, id: &str) -> &'a Value {
    page["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|card| card["identity"]["id"] == id)
        .unwrap_or_else(|| panic!("{id} missing from {page}"))
}
fn stored_poster(app: &App, table: &str, id: &str) -> Option<String> {
    app.db
        .lock()
        .unwrap()
        .query_row(
            &format!("SELECT poster FROM {table} WHERE profile_id=1 AND id=?1"),
            [id],
            |r| r.get(0),
        )
        .unwrap()
}
fn stored_context(app: &App, table: &str, id: &str) -> Value {
    let raw: String = app
        .db
        .lock()
        .unwrap()
        .query_row(
            &format!("SELECT context FROM {table} WHERE profile_id=1 AND id=?1"),
            [id],
            |r| r.get(0),
        )
        .unwrap();
    serde_json::from_str(&raw).unwrap()
}

#[tokio::test]
async fn an_empty_poster_is_served_and_saved_from_the_title_id() {
    let app = fixture();
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO favorites(profile_id,id,type,name) VALUES(1,'tt31514720','series','Re:Monster')",
            [],
        )
        .unwrap();
    let page = get(&app, "/api/profiles/1/library").await;
    let saved = card(&page, "tt31514720");
    assert_eq!(saved["poster"], poster("tt31514720"));
    assert_eq!(saved["landscape"], background("tt31514720"));
    assert_eq!(stored_poster(&app, "favorites", "tt31514720"), Some(poster("tt31514720")));
}

#[tokio::test]
async fn a_kitsu_row_without_a_mapping_stays_empty() {
    let app = fixture();
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO favorites(profile_id,id,type,name) VALUES(1,'kitsu:47964','series','Re:Monster')",
            [],
        )
        .unwrap();
    let page = get(&app, "/api/profiles/1/library").await;
    let saved = card(&page, "kitsu:47964");
    assert!(saved["poster"].is_null(), "{saved}");
    assert!(saved["landscape"].is_null(), "{saved}");
    art::backfill_all(&app).await.unwrap();
    assert_eq!(stored_poster(&app, "favorites", "kitsu:47964"), None);
    assert_eq!(stored_context(&app, "favorites", "kitsu:47964"), json!({}));
}

#[tokio::test]
async fn the_backfill_is_capped_and_idempotent() {
    let app = fixture();
    {
        let db = app.db.lock().unwrap();
        for n in 1..=3 {
            db.execute(
                "INSERT INTO favorites(profile_id,id,type,name) VALUES(1,?1,'movie','Saved')",
                [format!("tt000000{n}")],
            )
            .unwrap();
        }
        assert_eq!(art::backfill(&db, 1, 2).unwrap(), 2, "one batch stops at its cap");
        assert_eq!(art::backfill(&db, 1, 2).unwrap(), 1);
        assert_eq!(art::backfill(&db, 1, 2).unwrap(), 0, "nothing is left to fill");
    }
    art::backfill_all(&app).await.unwrap();
    for n in 1..=3 {
        let id = format!("tt000000{n}");
        assert_eq!(stored_poster(&app, "favorites", &id), Some(poster(&id)));
    }
}

#[tokio::test]
async fn history_and_continue_watching_cards_get_a_poster_and_backdrop() {
    let app = fixture();
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO progress(profile_id,id,type,name,poster,position,duration,updated_at,context,title_id) \
             VALUES(1,'tt1234567:1:2','series','Saved show',NULL,30,100,5,'{\"season\":1,\"episode\":2,\"series_id\":\"tt1234567\"}','tt1234567')",
            [],
        )
        .unwrap();
    let history = get(&app, "/api/profiles/1/history").await;
    assert_eq!(history[0]["card"]["poster"], poster("tt1234567"));
    assert_eq!(history[0]["card"]["landscape"], background("tt1234567"));
    let continuation = get(&app, "/api/profiles/1/continue").await;
    let entry = &continuation["entries"][0]["card"];
    assert_eq!(entry["poster"], poster("tt1234567"));
    assert_eq!(entry["landscape"], background("tt1234567"));
    assert_eq!(stored_poster(&app, "progress", "tt1234567:1:2"), Some(poster("tt1234567")));
    let context = stored_context(&app, "progress", "tt1234567:1:2");
    assert_eq!(context["background"], background("tt1234567"));
}

#[tokio::test]
async fn a_progress_write_stores_the_art_of_its_title() {
    let app = fixture();
    let record = json!({"record":{
        "identity":{"id":"tt1234567","kind":"series","ids":[],"episode":null,"edition":null},
        "parent_identity":null,"provider_id":"stremio","title":"Saved show","poster":null,"landscape":null,
        "position_millis":30_000,"duration_millis":100_000,"updated_at_millis":1,"completed_at_millis":null}});
    let (status, value) = request(
        &app,
        "member-token-1",
        "PUT",
        "/api/profiles/1/progress",
        record,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(stored_poster(&app, "progress", "tt1234567"), Some(poster("tt1234567")));
    assert_eq!(
        stored_context(&app, "progress", "tt1234567")["background"],
        background("tt1234567")
    );
}