use super::tracking_fixture::receipt;
use super::*;
use crate::test_support::encrypt_fixture_sources;
use axum::routing::{get, post};

const SHOW: &str = "tt2560140";

fn videos() -> Value {
    Value::Array(
        [(1u64, 5u64), (2, 3), (3, 22)]
            .into_iter()
            .flat_map(|(season, count)| {
                (1..=count).map(move |episode| {
                    json!({"id":format!("{SHOW}:{season}:{episode}"),"season":season,"episode":episode,"name":format!("Episode {season}x{episode}")})
                })
            })
            .collect(),
    )
}

async fn addon_meta(Path((_, _, id)): Path<(i64, String, String)>) -> axum::Json<Value> {
    axum::Json(match id.trim_end_matches(".json") {
        SHOW => json!({"meta":{"id":SHOW,"type":"series","name":"Attack on Titan","tvdb_id":"267440","moviedb_id":"1429","videos":videos()}}),
        _ => json!({}),
    })
}

async fn simkl_title(Path(id): Path<String>) -> axum::Json<Value> {
    axum::Json(match id.as_str() {
        "5000" => json!({"title":"Attack on Titan Season 3 Part 2","type":"anime","anime_type":"tv","ids":{"simkl":5000,"mal":38524,"imdb":SHOW,"tmdb":1429,"tvdb":267440}}),
        _ => Value::Null,
    })
}

async fn simkl_episodes(Path(id): Path<String>) -> axum::Json<Value> {
    axum::Json(match id.as_str() {
        "5000" => Value::Array(
            (1..=10)
                .map(|number| {
                    json!({"type":"episode","episode":number,"aired":true,"tvdb":{"season":3,"episode":12+number},"ids":{"simkl_id":5000+number}})
                })
                .collect(),
        ),
        _ => json!([]),
    })
}

async fn simkl_search(Query(query): Query<HashMap<String, String>>) -> axum::Json<Value> {
    axum::Json(match query.get("mal").map(String::as_str) {
        Some("38524") => json!([{"ids":{"simkl":5000}}]),
        _ => json!([]),
    })
}

async fn stack() -> (App, tokio::task::JoinHandle<()>, Arc<Mutex<Vec<Value>>>) {
    let (mut app, token_server, _) = server().await;
    link(&app).await;
    token_server.abort();
    let writes = Arc::new(Mutex::new(Vec::<Value>::new()));
    let history = writes.clone();
    let scrobble = writes.clone();
    let routes = Router::new()
        .route("/a/:addon/meta/:kind/:id", get(addon_meta))
        .route("/anime/:id", get(simkl_title))
        .route("/anime/episodes/:id", get(simkl_episodes))
        .route("/search/id", get(simkl_search))
        .route(
            "/sync/history",
            post(
                move |headers: axum::http::HeaderMap, axum::Json(body): axum::Json<Value>| {
                    let history = history.clone();
                    async move {
                        assert_eq!(
                            headers[header::AUTHORIZATION],
                            "Bearer fixture-private-token"
                        );
                        history.lock().unwrap().push(json!({"path":"history","body":body}));
                        axum::Json(json!({"added":{"episodes":1}}))
                    }
                },
            ),
        )
        .route(
            "/scrobble/:action",
            post(
                move |Path(action): Path<String>,
                      headers: axum::http::HeaderMap,
                      axum::Json(body): axum::Json<Value>| {
                    let scrobble = scrobble.clone();
                    async move {
                        assert_eq!(
                            headers[header::AUTHORIZATION],
                            "Bearer fixture-private-token"
                        );
                        scrobble.lock().unwrap().push(json!({"path":action,"body":body.clone()}));
                        let action = if action == "stop" { "scrobble" } else { action.as_str() };
                        axum::Json(json!({"action":action,"show":body["show"],"episode":body["episode"]}))
                    }
                },
            ),
        )
        .fallback(|method: axum::http::Method| async move {
            if method.is_safe() {
                StatusCode::OK
            } else {
                StatusCode::NOT_FOUND
            }
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    app.addons = app.addons.clone().for_account(1);
    app.addons.allow_test_loopback = true;
    app.simkl = Arc::new(crate::simkl::Service::fixture(&format!("http://{address}")));
    for (id, name, manifest) in [
        (
            4i64,
            "Cinemeta",
            json!({"id":"cinemeta","name":"Cinemeta","resources":["meta"],"types":["movie","series"],"idPrefixes":["tt"]}),
        ),
        (
            15i64,
            "AIO Metadata",
            json!({"id":"aio","name":"AIO Metadata","resources":["meta"],"types":["movie","series"],"idPrefixes":["mal"]}),
        ),
    ] {
        app.db
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO addons(id,name,manifest_url,manifest,account_id)VALUES(?1,?2,?3,?4,1)",
                params![
                    id,
                    name,
                    format!("http://{address}/a/{id}/manifest.json"),
                    manifest.to_string()
                ],
            )
            .unwrap();
    }
    encrypt_fixture_sources(&app);
    (app, task, writes)
}

fn episode_identity(id: &str, season: u64, episode: u64) -> Value {
    json!({"id":id,"kind":"series","ids":[],"episode":{"order":"aired","season":season,"episode":episode},"edition":null})
}

fn series_identity(id: &str, ids: Value) -> Value {
    json!({"id":id,"kind":"series","ids":ids,"episode":null,"edition":null})
}

fn cour_episode() -> Value {
    json!({"record":{"identity":episode_identity(&format!("{SHOW}:2:3"),2,3),"provider_id":"stremio","title":"Episode 2x3","poster":null,"landscape":null,"position_millis":95_000,"duration_millis":100_000,"updated_at_millis":0,"completed_at_millis":null},"parent_identity":series_identity("simkl:anime:5000",json!([])),"ended":false})
}

fn show_identity() -> Value {
    series_identity(
        SHOW,
        json!([
            {"namespace":"imdb","value":SHOW},
            {"namespace":"tvdb","value":"267440"},
            {"namespace":"tmdb","value":"1429"},
            {"namespace":"mal","value":"38524"}
        ]),
    )
}

#[tokio::test]
async fn an_episode_outside_the_opened_simkl_cour_writes_path_a_history() {
    let (app, task, writes) = stack().await;
    let (status, result) = request(
        &app,
        "member-token-1",
        "PUT",
        "/api/profiles/1/progress",
        cour_episode(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    receipt(&app, &format!("{SHOW}:2:3"), "done").await;
    assert_eq!(writes.lock().unwrap().len(), 1);
    let history = writes.lock().unwrap()[0]["body"].clone();
    assert!(history["anime"].is_null());
    let show = &history["shows"][0];
    assert_eq!(show["ids"]["tvdb"], "267440");
    assert_eq!(show["ids"]["tmdb"], "1429");
    assert_eq!(show["use_tvdb_anime_seasons"], true);
    assert_eq!(show["seasons"][0]["number"], 2);
    assert_eq!(show["seasons"][0]["episodes"][0]["number"], 3);
    task.abort();
}

#[tokio::test]
async fn an_episode_reduced_to_the_show_identity_still_writes_path_a_history() {
    let (app, task, writes) = stack().await;
    let mut body = cour_episode();
    body["parent_identity"] = show_identity();
    let (status, result) = request(&app, "member-token-1", "PUT", "/api/profiles/1/progress", body).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    receipt(&app, &format!("{SHOW}:2:3"), "done").await;
    assert_eq!(writes.lock().unwrap().len(), 1);
    let history = writes.lock().unwrap()[0]["body"].clone();
    let show = &history["shows"][0];
    assert_eq!(show["ids"]["tvdb"], "267440");
    assert_eq!(show["use_tvdb_anime_seasons"], true);
    assert_eq!(show["seasons"][0]["number"], 2);
    assert_eq!(show["seasons"][0]["episodes"][0]["number"], 3);
    task.abort();
}

#[tokio::test]
async fn an_episode_outside_the_opened_simkl_cour_scrobbles_path_a() {
    let (app, task, writes) = stack().await;
    let mut event = json!({"attempt_id":"cour","sequence":1,"identity":episode_identity(&format!("{SHOW}:2:3"),2,3),"parent_identity":series_identity("simkl:anime:5000",json!([])),"event":"start","position_millis":0,"duration_millis":100_000,"ended":false});
    let (status, response) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/profiles/1/playback-events",
        event.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["action"], "start");
    event["sequence"] = json!(2);
    event["event"] = json!("stop");
    event["position_millis"] = json!(95_000);
    let (status, response) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/profiles/1/playback-events",
        event,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["state"], "recorded");
    assert_eq!(response["action"], "stop");
    assert_eq!(writes.lock().unwrap().len(), 2);
    let scrobble = writes.lock().unwrap()[1].clone();
    assert_eq!(scrobble["path"], "stop");
    assert_eq!(scrobble["body"]["show"]["ids"]["tvdb"], "267440");
    assert_eq!(scrobble["body"]["episode"]["season"], 2);
    assert_eq!(scrobble["body"]["episode"]["number"], 3);
    task.abort();
}

#[tokio::test]
async fn a_show_identity_episode_without_an_anime_mapping_stays_local() {
    let (app, task, writes) = stack().await;
    let mut body = cour_episode();
    body["record"]["identity"] = episode_identity("tt0944947:1:1", 1, 1);
    body["parent_identity"] = series_identity(
        "tt0944947",
        json!([
            {"namespace":"imdb","value":"tt0944947"},
            {"namespace":"tvdb","value":"121361"}
        ]),
    );
    let (status, result) = request(&app, "member-token-1", "PUT", "/api/profiles/1/progress", body).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(app.simkl.tracking.pending_count(), 0);
    assert!(writes.lock().unwrap().is_empty());
    task.abort();
}
