use crate::engine_v3::anime_show::{resolve, ReleaseEpisode, TvdbEpisode};
use crate::{
    auth_integration_tests::fixture, test_support::encrypt_fixture_sources,
    test_support::request,
};
use crate::*;
use axum::routing::get;
use viptv_engine::identity::{EpisodeCoordinates, ExternalId, MediaIdentity};

fn release(id: &str, ids: &[(&str, &str)]) -> MediaIdentity {
    MediaIdentity {
        id: id.into(),
        kind: "series".into(),
        ids: ids
            .iter()
            .map(|(namespace, value)| ExternalId {
                namespace: (*namespace).into(),
                value: (*value).into(),
            })
            .collect(),
        episode: None,
        edition: None,
    }
}

fn episode(season: u32, episode: u32) -> EpisodeCoordinates {
    EpisodeCoordinates {
        order: "aired".into(),
        season: Some(season),
        episode,
    }
}

fn id_of(identity: &MediaIdentity, namespace: &str) -> Option<String> {
    identity
        .ids
        .iter()
        .find(|id| id.namespace == namespace)
        .map(|id| id.value.clone())
}

fn videos(prefix: &str, numbers: &[u32]) -> Value {
    Value::Array(
        numbers
            .iter()
            .map(|number| json!({"id":format!("{prefix}:{number}"),"season":1,"episode":number}))
            .collect(),
    )
}

fn tvdb_videos(prefix: &str, seasons: &[(u32, u32)]) -> Value {
    Value::Array(
        seasons
            .iter()
            .flat_map(|(season, count)| {
                (1..=*count).map(move |episode| {
                    json!({"id":format!("{prefix}:{season}:{episode}"),"season":season,"episode":episode,"name":format!("Episode {season}x{episode}"),"released":"2020-01-01T00:00:00.000Z"})
                })
            })
            .collect(),
    )
}

fn kitsu_videos(prefix: &str, count: u64, season: u64, offset: u64) -> Value {
    Value::Array(
        (1..=count)
            .map(|number| {
                json!({"id":format!("{prefix}:{number}"),"season":1,"episode":number,"imdbSeason":season,"imdbEpisode":offset+number,"absoluteEpisode":offset+number})
            })
            .collect(),
    )
}

const ATTACK_ON_TITAN_SEASONS: [(u32, u32); 5] = [(0, 2), (1, 3), (2, 3), (3, 22), (4, 3)];

fn episode_at(body: &Value, season: u64, episode: u64) -> String {
    body["episodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|card| {
            card["identity"]["episode"]["season"] == season
                && card["identity"]["episode"]["episode"] == episode
        })
        .map(|card| card["identity"]["id"].as_str().unwrap().to_owned())
        .unwrap_or_else(|| panic!("no {season}x{episode} in {}", body["episodes"]))
}

fn seasons(body: &Value) -> Vec<u64> {
    body["seasons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|season| season["season"].as_u64().unwrap())
        .collect()
}

fn meta_of(addon: i64, id: &str) -> Value {
    match (addon, id.trim_end_matches(".json")) {
        (4, "tt0409591") => json!({"meta":{"id":"tt0409591","type":"series","name":"Naruto","tvdb":"78857","moviedb_id":"46260",
            "videos":tvdb_videos("tt0409591", &[(1,2),(2,2),(3,2)])}}),
        (4, "tt2560140") => json!({"meta":{"id":"tt2560140","type":"series","name":"Attack on Titan","tvdb_id":"267440","moviedb_id":"1429",
            "videos":tvdb_videos("tt2560140", &ATTACK_ON_TITAN_SEASONS)}}),
        (15, "mal:20") => json!({"meta":{"id":"mal:20","type":"series","name":"Naruto","imdb_id":"tt0409591","_tmdbId":"46260","_tvdbId":"78857","_malId":"20","_kitsuId":"11",
            "videos":videos("kitsu:11", &[1,2,3])}}),
        (15, "mal:16498") => json!({"meta":{"id":"mal:16498","type":"series","name":"Attack on Titan","imdb_id":"tt2560140","_tvdbId":"267440","_tmdbId":"1429","_malId":"16498",
            "videos":videos("kitsu:16498", &[1,2,3])}}),
        (15, "mal:38524") => json!({"meta":{"id":"mal:38524","type":"series","name":"Attack on Titan Season 3 Part 2","imdb_id":"tt2560140","_tvdbId":"267440","_malId":"38524",
            "videos":videos("kitsu:38524", &[1,2,3,4,5,6,7,8,9,10])}}),
        (15, "mal:21") => json!({"meta":{"id":"mal:21","type":"series","name":"One Piece","imdb_id":"tt0388629","_tvdbId":"81797","_malId":"21",
            "videos":videos("kitsu:21", &[1,2,3,199,200])}}),
        (15, "mal:31") => json!({"meta":{"id":"mal:31","type":"series","name":"Coordinates missing","_tvdbId":"999","_malId":"31",
            "videos":[{"id":"mal:31:1","season":1,"episode":1}]}}),
        (15, "mal:9") => json!({"meta":{"id":"mal:9","type":"series","name":"Unmapped","_malId":"9",
            "videos":[{"id":"mal:9:1","season":1,"episode":1}]}}),
        (16, "kitsu:11") => json!({"meta":{"id":"kitsu:11","type":"series","name":"Naruto","imdb_id":"tt0409591","kitsu_id":"11"}}),
        (16, "kitsu:41982") => json!({"meta":{"id":"kitsu:41982","type":"series","name":"Attack on Titan Season 3 Part 2","imdb_id":"tt2560140","kitsu_id":"41982",
            "videos":kitsu_videos("kitsu:41982", 10, 3, 12)}}),
        (16, "kitsu:9") => json!({"meta":{"id":"kitsu:9","type":"series","name":"Unmapped Kitsu","kitsu_id":"9"}}),
        _ => json!({}),
    }
}

async fn addons_meta(Path((addon, _, id)): Path<(i64, String, String)>) -> axum::Json<Value> {
    axum::Json(meta_of(addon, &id))
}

async fn simkl_search(Query(query): Query<HashMap<String, String>>) -> axum::Json<Value> {
    let id = match (
        query.get("mal").map(String::as_str),
        query.get("kitsu").map(String::as_str),
    ) {
        (Some("38524"), _) => Some(5000),
        (Some("16498"), _) => Some(5001),
        (Some("21"), _) => Some(402),
        (Some("31"), _) => Some(403),
        (Some("20"), _) | (_, Some("11")) => Some(401),
        _ => None,
    };
    axum::Json(match id {
        Some(id) => json!([{"ids":{"simkl":id}}]),
        None => json!([]),
    })
}

async fn simkl_title(Path(id): Path<String>) -> axum::Json<Value> {
    axum::Json(match id.as_str() {
        "401" => json!({"title":"Naruto","ids":{"simkl":401,"mal":20,"kitsu":11,"imdb":"tt0409591","tmdb":46260,"tvdb":78857}}),
        "402" => json!({"title":"One Piece","ids":{"simkl":402,"mal":21,"imdb":"tt0388629","tvdb":81797}}),
        "403" => json!({"title":"Coordinates missing","ids":{"simkl":403,"mal":31,"tvdb":999}}),
        "5000" => json!({"title":"Attack on Titan Season 3 Part 2","type":"anime","anime_type":"tv","ids":{"simkl":5000,"mal":38524,"imdb":"tt2560140","tmdb":1429,"tvdb":267440}}),
        "5001" => json!({"title":"Attack on Titan","type":"anime","anime_type":"tv","ids":{"simkl":5001,"mal":16498,"imdb":"tt2560140","tmdb":1429,"tvdb":267440}}),
        _ => Value::Null,
    })
}

async fn simkl_episodes(Path(id): Path<String>) -> axum::Json<Value> {
    let rows = match id.as_str() {
        "401" => (1..=3)
            .map(|n| json!({"type":"episode","episode":n,"aired":true,"tvdb":{"season":1,"episode":n},"ids":{"simkl_id":4100+n}}))
            .collect::<Vec<_>>(),
        "402" => [(1, 1, 1), (2, 1, 2), (3, 1, 3), (199, 10, 4), (200, 10, 5)]
            .into_iter()
            .map(|(number, season, episode)| {
                json!({"type":"episode","episode":number,"aired":true,"tvdb":{"season":season,"episode":episode},"ids":{"simkl_id":4200+number}})
            })
            .collect::<Vec<_>>(),
        "403" => (1..=2)
            .map(|n| json!({"type":"episode","episode":n,"aired":true,"ids":{"simkl_id":4300+n}}))
            .collect::<Vec<_>>(),
        "5000" => (1..=10)
            .map(|n| json!({"type":"episode","episode":n,"aired":true,"tvdb":{"season":3,"episode":12+n},"ids":{"simkl_id":5000+n}}))
            .collect::<Vec<_>>(),
        "5001" => (1..=3)
            .map(|n| json!({"type":"episode","episode":n,"aired":true,"tvdb":{"season":1,"episode":n},"ids":{"simkl_id":5100+n}}))
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    axum::Json(Value::Array(rows))
}

pub(super) async fn stack() -> (App, tokio::task::JoinHandle<()>) {
    let routes = Router::new()
        .route("/a/:addon/meta/:kind/:id", get(addons_meta))
        .route("/search/id", get(simkl_search))
        .route("/anime/:id", get(simkl_title))
        .route("/anime/episodes/:id", get(simkl_episodes))
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
    let mut app = fixture();
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
        (
            16i64,
            "Anime Kitsu",
            json!({"id":"kitsu","name":"Anime Kitsu","resources":["meta"],"types":["movie","series"],"idPrefixes":["kitsu"]}),
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
    (app, task)
}

#[tokio::test]
async fn naruto_from_an_anime_catalog_resolves_to_its_tvdb_and_tmdb_show() {
    let (app, task) = stack().await;
    for (id, ids) in [
        ("mal:20", &[("mal", "20")][..]),
        ("kitsu:11", &[("kitsu", "11")][..]),
    ] {
        let show = resolve(&app, &release(id, ids)).await.unwrap();
        assert_eq!(show.identity().id, "tt0409591", "{id}");
        assert_eq!(id_of(show.identity(), "tvdb"), Some("78857".into()), "{id}");
        assert_eq!(id_of(show.identity(), "tmdb"), Some("46260".into()), "{id}");
        assert_eq!(
            show.tvdb_episode(&episode(1, 2)),
            Some(TvdbEpisode {
                season: 1,
                episode: 2
            }),
            "{id}"
        );
    }
    task.abort();
}

#[tokio::test]
async fn an_attack_on_titan_cour_lands_on_its_show_and_tvdb_season() {
    let (app, task) = stack().await;
    let cour = resolve(&app, &release("mal:38524", &[("mal", "38524")]))
        .await
        .unwrap();
    assert_eq!(cour.identity().id, "tt2560140");
    assert_eq!(id_of(cour.identity(), "tvdb"), Some("267440".into()));
    assert_eq!(
        cour.tvdb_episode(&episode(1, 1)),
        Some(TvdbEpisode {
            season: 3,
            episode: 13
        })
    );
    assert_eq!(
        cour.tvdb_episode(&episode(1, 10)),
        Some(TvdbEpisode {
            season: 3,
            episode: 22
        })
    );
    assert_eq!(
        cour.release_episode(TvdbEpisode {
            season: 3,
            episode: 22
        }),
        Some(ReleaseEpisode {
            season: Some(1),
            episode: 10
        })
    );
    let from_simkl = resolve(&app, &release("simkl:anime:5000", &[("simkl", "5000")]))
        .await
        .unwrap();
    assert_eq!(id_of(from_simkl.identity(), "tvdb"), Some("267440".into()));
    assert_eq!(
        from_simkl.tvdb_episode(&episode(3, 13)),
        Some(TvdbEpisode {
            season: 3,
            episode: 13
        })
    );
    assert_eq!(
        from_simkl.release_episode(TvdbEpisode {
            season: 3,
            episode: 13
        }),
        Some(ReleaseEpisode {
            season: Some(3),
            episode: 13
        })
    );
    task.abort();
}

#[tokio::test]
async fn absolute_numbering_maps_to_tvdb_coordinates_in_both_directions() {
    let (app, task) = stack().await;
    let show = resolve(&app, &release("mal:21", &[("mal", "21")]))
        .await
        .unwrap();
    assert_eq!(id_of(show.identity(), "tvdb"), Some("81797".into()));
    assert_eq!(
        show.tvdb_episode(&episode(1, 199)),
        Some(TvdbEpisode {
            season: 10,
            episode: 4
        })
    );
    assert_eq!(
        show.release_episode(TvdbEpisode {
            season: 10,
            episode: 4
        }),
        Some(ReleaseEpisode {
            season: Some(1),
            episode: 199
        })
    );
    assert_eq!(
        show.release_episode(TvdbEpisode {
            season: 1,
            episode: 3
        }),
        Some(ReleaseEpisode {
            season: Some(1),
            episode: 3
        })
    );
    assert_eq!(show.tvdb_episode(&episode(1, 100)), None);
    task.abort();
}

#[tokio::test]
async fn a_release_without_a_tvdb_or_tmdb_mapping_keeps_its_identity() {
    let (app, task) = stack().await;
    assert!(resolve(&app, &release("mal:9", &[("mal", "9")]))
        .await
        .is_none());
    assert!(resolve(&app, &release("tt0903747", &[("imdb", "tt0903747")]))
        .await
        .is_none());
    assert!(resolve(&app, &release("kitsu:9", &[("kitsu", "9")]))
        .await
        .is_none());
    task.abort();
}

#[tokio::test]
async fn a_kitsu_release_document_maps_its_episodes_to_the_show_without_simkl() {
    let (app, task) = stack().await;
    let show = resolve(&app, &release("kitsu:41982", &[("kitsu", "41982")]))
        .await
        .unwrap();
    assert_eq!(show.identity().id, "tt2560140");
    assert_eq!(id_of(show.identity(), "tvdb"), Some("267440".into()));
    assert_eq!(
        show.tvdb_episode(&episode(1, 1)),
        Some(TvdbEpisode {
            season: 3,
            episode: 13
        })
    );
    assert_eq!(
        show.tvdb_episode(&episode(1, 10)),
        Some(TvdbEpisode {
            season: 3,
            episode: 22
        })
    );
    assert_eq!(show.tvdb_episode(&episode(1, 11)), None);
    assert_eq!(
        show.release_episode(TvdbEpisode {
            season: 3,
            episode: 22
        }),
        Some(ReleaseEpisode {
            season: Some(1),
            episode: 10
        })
    );
    task.abort();
}

#[tokio::test]
async fn a_simkl_episode_list_without_tvdb_coordinates_still_resolves_the_show() {
    let (app, task) = stack().await;
    let show = resolve(&app, &release("mal:31", &[("mal", "31")]))
        .await
        .unwrap();
    assert_eq!(id_of(show.identity(), "tvdb"), Some("999".into()));
    assert_eq!(show.tvdb_episode(&episode(1, 1)), None);
    assert_eq!(
        show.release_episode(TvdbEpisode {
            season: 1,
            episode: 1
        }),
        None
    );
    task.abort();
}

async fn detail(app: &App, path: &str) -> Value {
    let (status, body) = request(app, "member-token-1", "GET", path, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{path}: {body}");
    body
}

#[tokio::test]
async fn details_from_cinemeta_lists_the_show_seasons() {
    let (app, task) = stack().await;
    let body = detail(&app, "/api/meta/series/tt2560140").await;
    assert_eq!(body["item"]["identity"]["id"], "tt2560140");
    assert_eq!(seasons(&body), vec![0, 1, 2, 3, 4]);
    assert_eq!(body["episodes"][0]["identity"]["id"], "tt2560140:0:1");
    assert_eq!(body["episodes"][0]["identity"]["episode"]["season"], 0);
    task.abort();
}

#[tokio::test]
async fn details_from_an_anime_catalog_release_lists_the_show_seasons() {
    let (app, task) = stack().await;
    let body = detail(
        &app,
        "/api/meta/series/mal:38524?provider_id=stremio&addon_id=15",
    )
    .await;
    assert_eq!(body["item"]["identity"]["id"], "mal:38524");
    assert_eq!(body["item"]["title"], "Attack on Titan");
    assert_eq!(seasons(&body), vec![0, 1, 2, 3, 4]);
    assert_eq!(body["episodes"][0]["identity"]["id"], "tt2560140:0:1");
    assert_eq!(body["episodes"][0]["identity"]["episode"]["season"], 0);
    assert_eq!(body["episodes"][0]["parent_identity"]["id"], "mal:38524");
    assert_eq!(episode_at(&body, 3, 13), "kitsu:38524:1");
    let ids = body["item"]["identity"]["ids"].as_array().unwrap();
    assert!(ids.contains(&json!({"namespace":"imdb","value":"tt2560140"})), "{ids:?}");
    assert!(ids.contains(&json!({"namespace":"tvdb","value":"267440"})), "{ids:?}");
    task.abort();
}

#[tokio::test]
async fn details_from_anime_kitsu_lists_the_tvdb_shows_seasons() {
    let (app, task) = stack().await;
    let body = detail(
        &app,
        "/api/meta/series/kitsu:11?provider_id=stremio&addon_id=16",
    )
    .await;
    assert_eq!(body["item"]["identity"]["id"], "kitsu:11");
    assert_eq!(body["item"]["title"], "Naruto");
    assert_eq!(seasons(&body), vec![1, 2, 3]);
    assert_eq!(body["episodes"][0]["identity"]["id"], "tt0409591:1:1");
    task.abort();
}

#[tokio::test]
async fn details_from_simkl_lists_the_show_seasons() {
    let (app, task) = stack().await;
    let body = detail(&app, "/api/meta/series/simkl:anime:5000").await;
    assert_eq!(body["item"]["identity"]["id"], "simkl:anime:5000");
    assert_eq!(body["item"]["title"], "Attack on Titan");
    assert_eq!(seasons(&body), vec![0, 1, 2, 3, 4]);
    assert_eq!(body["episodes"][0]["identity"]["id"], "tt2560140:0:1");
    assert_eq!(episode_at(&body, 3, 13), "simkl:anime:5000:3:13");
    let ids = body["item"]["identity"]["ids"].as_array().unwrap();
    assert!(ids.contains(&json!({"namespace":"simkl","value":"5000"})), "{ids:?}");
    assert!(ids.contains(&json!({"namespace":"imdb","value":"tt2560140"})), "{ids:?}");
    task.abort();
}

#[tokio::test]
async fn a_release_without_a_show_document_keeps_its_own_episode_list_with_tvdb_coordinates() {
    let (app, task) = stack().await;
    let body = detail(
        &app,
        "/api/meta/series/mal:31?provider_id=stremio&addon_id=15",
    )
    .await;
    assert_eq!(body["item"]["identity"]["id"], "mal:31");
    assert_eq!(body["episodes"][0]["identity"]["id"], "mal:31:1");
    assert_eq!(body["episodes"][0]["identity"]["episode"]["season"], 1);
    task.abort();
}

#[tokio::test]
async fn a_simkl_anime_release_resolves_to_the_show_for_write_time_canonicalisation() {
    let (app, task) = stack().await;
    let show = crate::engine_v3::anime_show::canonical_show(
        &app,
        &release("simkl:anime:5000", &[("simkl", "5000")]),
    )
    .await
    .unwrap();
    assert_eq!(show.id, "tt2560140");
    assert_eq!(id_of(&show, "tvdb"), Some("267440".into()));
    let (show, tvdb) = crate::engine_v3::anime_show::canonical_episode(
        &app,
        &release("mal:38524", &[("mal", "38524")]),
        &episode(3, 13),
    )
    .await
    .unwrap();
    assert_eq!(show.id, "tt2560140");
    assert_eq!(
        tvdb,
        TvdbEpisode {
            season: 3,
            episode: 13
        }
    );
    task.abort();
}

fn parent(id: &str, namespace: &str, value: &str) -> Value {
    json!({"id":id,"kind":"series","ids":[{"namespace":namespace,"value":value}],"episode":null,"edition":null})
}

fn episode_write(id: &str, season: u64, episode: u64, parent: &Value) -> Value {
    json!({
        "record": {
            "identity": {"id":id,"kind":"series","ids":[],"episode":{"order":"aired","season":season,"episode":episode},"edition":null},
            "provider_id": "stremio",
            "title": "Attack on Titan",
            "poster": "/poster.jpg",
            "landscape": null,
            "position_millis": 60_000,
            "duration_millis": 1_440_000,
            "updated_at_millis": 0,
            "completed_at_millis": null,
            "parent_identity": parent,
            "metadata_origin": null
        },
        "parent_identity": parent,
        "ended": false,
    })
}

fn progress_row(app: &App, id: &str) -> (String, String, Value) {
    app.db
        .lock()
        .unwrap()
        .query_row(
            "SELECT id,title_id,context FROM progress WHERE profile_id=1 AND id=?1",
            params![id],
            |r| {
                let context: String = r.get(2)?;
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    serde_json::from_str(&context).unwrap_or(Value::Null),
                ))
            },
        )
        .unwrap()
}

#[tokio::test]
async fn a_release_episode_write_is_stored_under_the_show_and_its_tvdb_coordinates() {
    let (app, task) = stack().await;
    let parent = parent("mal:38524", "mal", "38524");
    let (status, _) = request(
        &app,
        "member-token-1",
        "PUT",
        "/api/profiles/1/progress",
        episode_write("kitsu:38524:1", 3, 13, &parent),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (id, title_id, context) = progress_row(&app, "tt2560140:3:13");
    assert_eq!(id, "tt2560140:3:13");
    assert_eq!(title_id, "tt2560140");
    assert_eq!(context["season"], 3);
    assert_eq!(context["episode"], 13);
    assert_eq!(context["series_id"], "tt2560140");
    assert_eq!(context["engine_identity"]["id"], "tt2560140:3:13");
    assert_eq!(context["engine_parent_identity"]["id"], "tt2560140");
    let ids = context["engine_parent_identity"]["ids"].as_array().unwrap();
    assert!(ids.contains(&json!({"namespace":"imdb","value":"tt2560140"})), "{ids:?}");
    assert!(ids.contains(&json!({"namespace":"tvdb","value":"267440"})), "{ids:?}");
    let rows: i64 = app
        .db
        .lock()
        .unwrap()
        .query_row("SELECT count(*) FROM progress WHERE profile_id=1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 1);
    task.abort();
}

#[tokio::test]
async fn a_kitsu_release_episode_write_is_stored_under_the_show() {
    let (app, task) = stack().await;
    let parent = parent("kitsu:41982", "kitsu", "41982");
    let (status, _) = request(
        &app,
        "member-token-1",
        "PUT",
        "/api/profiles/1/progress",
        episode_write("kitsu:41982:1", 1, 1, &parent),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (id, title_id, context) = progress_row(&app, "tt2560140:3:13");
    assert_eq!(id, "tt2560140:3:13");
    assert_eq!(title_id, "tt2560140");
    assert_eq!(context["series_id"], "tt2560140");
    assert_eq!(context["season"], 3);
    assert_eq!(context["episode"], 13);
    task.abort();
}

#[tokio::test]
async fn a_simkl_release_episode_write_is_stored_under_the_show() {
    let (app, task) = stack().await;
    let parent = parent("simkl:anime:5000", "simkl", "5000");
    let (status, _) = request(
        &app,
        "member-token-1",
        "PUT",
        "/api/profiles/1/progress",
        episode_write("simkl:anime:5000:3:14", 3, 14, &parent),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, title_id, context) = progress_row(&app, "tt2560140:3:14");
    assert_eq!(title_id, "tt2560140");
    assert_eq!(context["series_id"], "tt2560140");
    assert_eq!(context["season"], 3);
    assert_eq!(context["episode"], 14);
    task.abort();
}

#[tokio::test]
async fn a_saved_release_favorite_is_stored_under_the_show() {
    let (app, task) = stack().await;
    let card = json!({
        "identity": parent("mal:38524", "mal", "38524"),
        "provider_id": "stremio",
        "title": "Attack on Titan",
        "subtitle": null,
        "description": null,
        "poster": null,
        "landscape": null,
        "logo": null,
        "progress": null,
        "badges": [],
        "parent_identity": null,
        "added_at_millis": null,
    });
    let (status, _) = request(
        &app,
        "member-token-1",
        "PUT",
        "/api/profiles/1/library",
        json!({"card": card}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let saved: Vec<String> = {
        let db = app.db.lock().unwrap();
        let mut query = db
            .prepare("SELECT id FROM favorites WHERE profile_id=1 ORDER BY id")
            .unwrap();
        query
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(saved, vec!["tt2560140"]);
    task.abort();
}
