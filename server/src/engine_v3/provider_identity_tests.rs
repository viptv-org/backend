use crate::*;
use crate::{auth_integration_tests::fixture, test_support::request};
use axum::routing::get;
use std::sync::Mutex;

type Asked = Arc<Mutex<Vec<i64>>>;

fn naruto() -> Value {
    json!({"meta":{"id":"mal:20","type":"series","name":"Naruto","imdb_id":"tt0409591","_tmdbId":"46260","_tvdbId":"78857","_malId":"20","_kitsuId":"11","_anilistId":"20","_anidbId":"2392",
        "videos":[{"id":"kitsu:11:1","season":1,"episode":1,"title":"Enter: Naruto Uzumaki!","released":"2002-10-03T00:00:00Z"},
                  {"id":"kitsu:11:2","season":1,"episode":2,"title":"My Name is Konohamaru!","released":"2002-10-10T00:00:00Z"}]}})
}

async fn stack() -> (App, tokio::task::JoinHandle<()>, Asked) {
    let asked: Asked = Default::default();
    let routes = Router::new().route(
        "/a/:addon/meta/:kind/:id",
        get({
            let asked = asked.clone();
            move |Path((addon, _, id)): Path<(i64, String, String)>| {
                let asked = asked.clone();
                async move {
                    asked.lock().unwrap().push(addon);
                    let id = id.trim_end_matches(".json");
                    axum::Json(match (addon, id) {
                        (15, "mal:20") => naruto(),
                        (15, "mal:21") => json!({"meta":{"id":"kitsu:11","type":"series","name":"Naruto","_malId":"21",
                            "videos":[{"id":"kitsu:11:1","season":1,"episode":1,"title":"Enter: Naruto Uzumaki!"}]}}),
                        (15, "mal:22") => json!({"meta":{"id":"mal:99","type":"series","name":"Another title","_malId":"99","videos":[]}}),
                        (15, _) => json!({}),
                        (4, _) => json!({"meta":{"id":id,"type":"series","name":"Cinemeta title","videos":[]}}),
                        _ => json!({}),
                    })
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    let mut app = fixture();
    app.addons.allow_test_loopback = true;
    for (id, name, manifest) in [
        (
            4i64,
            "Cinemeta",
            json!({"id":"cinemeta","name":"Cinemeta","resources":["meta"],"types":["movie","series"]}),
        ),
        (
            15i64,
            "AIO Metadata",
            json!({"id":"aio","name":"AIO Metadata","resources":["meta"],"types":["movie","series"],"idPrefixes":["tt"]}),
        ),
    ] {
        app.db.lock().unwrap().execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id)VALUES(?1,?2,?3,?4,1)",params![id,name,format!("http://{address}/a/{id}/manifest.json"),manifest.to_string()]).unwrap();
    }
    crate::test_support::encrypt_fixture_sources(&app);
    (app, task, asked)
}

fn served(asked: &Asked) -> Vec<i64> {
    asked.lock().unwrap().clone()
}

#[tokio::test]
async fn an_aiometadata_title_with_kitsu_episodes_and_underscore_cross_ids_is_the_same_title() {
    let (app, task, asked) = stack().await;
    let (status, value) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/meta/series/mal:20?metadata_provider_id=stremio&metadata_addon_id=15",
        Value::Null,
    )
    .await;
    task.abort();
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(served(&asked), vec![15]);
    let item = &value["item"];
    assert_eq!(item["identity"]["id"], "mal:20");
    assert_eq!(item["metadata_origin"]["connection_id"], "addon:15");
    let ids = item["identity"]["ids"].as_array().unwrap();
    for (namespace, id) in [
        ("imdb", "tt0409591"),
        ("tmdb", "46260"),
        ("tvdb", "78857"),
        ("mal", "20"),
        ("kitsu", "11"),
        ("anilist", "20"),
        ("anidb", "2392"),
    ] {
        assert!(
            ids.iter()
                .any(|value| value["namespace"] == namespace && value["value"] == id),
            "{namespace}:{id} missing from {ids:?}"
        );
    }
    let episodes = value["episodes"].as_array().unwrap();
    assert_eq!(episodes.len(), 2);
    assert_eq!(episodes[0]["identity"]["id"], "kitsu:11:1");
    assert_eq!(episodes[1]["identity"]["id"], "kitsu:11:2");
    assert_eq!(episodes[0]["parent_identity"]["id"], "mal:20");
    assert_eq!(
        episodes[0]["parent_identity"]["ids"],
        item["identity"]["ids"]
    );
}

#[tokio::test]
async fn an_explicit_addon_is_asked_for_a_title_its_manifest_does_not_advertise() {
    let (app, task, asked) = stack().await;
    let (status, value) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/meta/series/mal:20?provider_id=stremio&addon_id=15",
        Value::Null,
    )
    .await;
    task.abort();
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(served(&asked), vec![15]);
    assert_eq!(value["item"]["metadata_origin"]["connection_id"], "addon:15");
    assert_eq!(value["item"]["title"], "Naruto");
}

#[tokio::test]
async fn a_provider_document_that_asserts_the_requested_id_in_another_namespace_is_the_same_title() {
    let (app, task, asked) = stack().await;
    let (status, value) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/meta/series/mal:21?provider_id=stremio&addon_id=15",
        Value::Null,
    )
    .await;
    task.abort();
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(served(&asked), vec![15]);
    assert_eq!(value["item"]["identity"]["id"], "mal:21");
    assert_eq!(value["episodes"][0]["identity"]["id"], "kitsu:11:1");
}

#[tokio::test]
async fn a_different_title_in_the_same_namespace_is_rejected() {
    let (app, task, _) = stack().await;
    let (status, value) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/meta/series/mal:22?provider_id=stremio&addon_id=15",
        Value::Null,
    )
    .await;
    task.abort();
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{value}");
    assert_eq!(value["error"], "Provider returned a different title");
}

#[tokio::test]
async fn an_explicit_addon_that_cannot_answer_reports_it_instead_of_using_another_addon() {
    let (app, task, asked) = stack().await;
    let (status, value) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/meta/series/tt1111111?provider_id=stremio&addon_id=15",
        Value::Null,
    )
    .await;
    task.abort();
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{value}");
    assert_eq!(
        value["error"],
        "The selected metadata add-on did not return this title"
    );
    assert_eq!(served(&asked), vec![15]);
}
