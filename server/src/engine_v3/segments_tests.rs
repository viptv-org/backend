use super::anime_show_tests::stack;
use super::segments::{
    animeskip_segments, answer, parse, parse_skipdb, parse_theintrodb, target, Service, Target,
};
use crate::{auth, ResourceLease};
use axum::{
    extract::{Path, RawQuery},
    http::StatusCode,
    routing::{get, post},
    Router,
};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use viptv_engine::presentation::Card;

struct Source {
    base: String,
    reads: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Source {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn source(rows: Value) -> Source {
    let reads = Arc::new(AtomicUsize::new(0));
    let count = reads.clone();
    let routes = Router::new()
        .route(
            "/v2/skip-times/:mal/:episode",
            get(
                move |Path((mal, episode)): Path<(String, String)>, RawQuery(query): RawQuery| {
                    let count = count.clone();
                    let rows = rows.clone();
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        let query = query.unwrap_or_default();
                        assert!(
                            query.contains("types[]=op")
                                && query.contains("types[]=ed")
                                && query.contains("types[]=recap")
                                && query.contains("episodeLength=0"),
                            "{query}"
                        );
                        let key = format!("{mal}/{episode}");
                        match rows.get(key.as_str()) {
                            Some(results) => (
                                StatusCode::OK,
                                axum::Json(json!({"found":true,"results":results,"message":"Successfully found skip times","statusCode":200})),
                            ),
                            None => (
                                StatusCode::NOT_FOUND,
                                axum::Json(json!({"found":false,"results":[],"message":"No skip times found","statusCode":404})),
                            ),
                        }
                    }
                },
            ),
        )
        .route("/", get(|| async { StatusCode::OK }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    Source {
        base: format!("http://{address}"),
        reads,
        task,
    }
}

async fn fribb(body: Value) -> Source {
    let routes = Router::new()
        .route("/", get(move || async move { (StatusCode::OK, axum::Json(body.clone())) }))
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
    Source {
        base: format!("http://{address}"),
        reads: Arc::new(AtomicUsize::new(0)),
        task,
    }
}

async fn segments_fake(path: &str, body: Value) -> Source {
    let routes = Router::new()
        .route(path, get(move || async move { (StatusCode::OK, axum::Json(body.clone())) }))
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
    Source {
        base: format!("http://{address}"),
        reads: Arc::new(AtomicUsize::new(0)),
        task,
    }
}

fn fixture_rows() -> Value {
    json!({
        "38524/10": [
            {"interval":{"startTime":310.571,"endTime":400.571},"skipType":"op","skipId":"op","episodeLength":1443.984},
            {"interval":{"startTime":1290.0,"endTime":1380.0},"skipType":"ed","skipId":"ed","episodeLength":1443.984}
        ]
    })
}

fn lease() -> ResourceLease {
    ResourceLease {
        policy_revision: 0,
        principal: auth::Principal::Account {
            account_id: 1,
            role: "owner".into(),
            profile_id: Some(1),
            session_id: Some("s1".into()),
        },
        session_id: Some("s1".into()),
    }
}

fn ids() -> Value {
    json!([
        {"namespace":"imdb","value":"tt2560140"},
        {"namespace":"tvdb","value":"267440"},
        {"namespace":"tmdb","value":"1429"},
        {"namespace":"mal","value":"38524"},
        {"namespace":"anilist","value":"104578"}
    ])
}

fn episode_card(season: u32, episode: u32) -> Card {
    serde_json::from_value(json!({
        "identity":{"id":"tt2560140","kind":"series","ids":ids(),"episode":{"order":"aired","season":season,"episode":episode},"edition":null},
        "provider_id":"simkl","title":"Attack on Titan Season 3 Part 2","subtitle":null,"description":null,
        "poster":null,"landscape":null,"logo":null,"progress":null,"badges":[],
        "parent_identity":{"id":"tt2560140","kind":"series","ids":ids(),"episode":null,"edition":null}
    }))
    .unwrap()
}

#[test]
fn skip_types_convert_to_milliseconds_and_ignore_mixed_or_impossible_intervals() {
    let document = json!({"found":true,"results":[
        {"interval":{"startTime":310.571,"endTime":400.571},"skipType":"op"},
        {"interval":{"startTime":1290.0,"endTime":1380.0},"skipType":"ed"},
        {"interval":{"startTime":400.0,"endTime":500.0},"skipType":"mixed-op"},
        {"interval":{"startTime":0.0,"endTime":30.0},"skipType":"recap"},
        {"interval":{"startTime":100.0,"endTime":90.0},"skipType":"ed"},
        {"interval":{"startTime":-5.0,"endTime":10.0},"skipType":"op"},
        {"interval":{"startTime":"1","endTime":10.0},"skipType":"ed"}
    ]});
    assert_eq!(
        parse(&document),
        vec![
            super::segments::Segment {
                kind: "recap",
                start_ms: 0,
                end_ms: 30_000
            },
            super::segments::Segment {
                kind: "intro",
                start_ms: 310_571,
                end_ms: 400_571
            },
            super::segments::Segment {
                kind: "credits",
                start_ms: 1_290_000,
                end_ms: 1_380_000
            },
        ]
    );
    assert!(parse(&json!({"found":false,"results":[]})).is_empty());
    assert!(parse(&json!({"statusCode":500})).is_empty());
}

#[tokio::test]
async fn a_known_episode_returns_its_opening_and_credits_and_an_unknown_one_returns_none() {
    let source = source(fixture_rows()).await;
    let service = Service::fixture(Some(&source.base), None, None, None);
    let target = Target {
        season: None,
        episode: 10,
        mal: Some(38524),
        anilist: None,
        release_episode: Some(10),
        tvdb: None,
        tmdb: None,
        imdb: None,
    };
    assert_eq!(
        serde_json::to_value(service.lookup(&target).await).unwrap(),
        json!([
            {"kind":"intro","start_ms":310571,"end_ms":400571},
            {"kind":"credits","start_ms":1290000,"end_ms":1380000}
        ])
    );
    assert_eq!(source.reads.load(Ordering::SeqCst), 1);
    service.lookup(&target).await;
    assert_eq!(source.reads.load(Ordering::SeqCst), 1);
    let empty = Target {
        season: None,
        episode: 11,
        mal: Some(38524),
        anilist: None,
        release_episode: Some(11),
        tvdb: None,
        tmdb: None,
        imdb: None,
    };
    assert!(service.lookup(&empty).await.is_empty());
    assert_eq!(source.reads.load(Ordering::SeqCst), 2);
    assert!(service
        .lookup(&Target {
            season: None,
            episode: 1,
            mal: Some(0),
            anilist: None,
            release_episode: Some(1),
            tvdb: None,
            tmdb: None,
            imdb: None,
        })
        .await
        .is_empty());
    assert_eq!(source.reads.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn an_upstream_failure_reports_no_segments_and_backs_off() {
    let reads = Arc::new(AtomicUsize::new(0));
    let count = reads.clone();
    let routes = Router::new()
        .route(
            "/v2/skip-times/:mal/:episode",
            get(move || {
                let count = count.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    StatusCode::INTERNAL_SERVER_ERROR
                }
            }),
        )
        .route("/", get(|| async { StatusCode::OK }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    let service = Service::fixture(Some(&format!("http://{address}")), None, None, None);
    let target = Target {
        season: None,
        episode: 10,
        mal: Some(38524),
        anilist: None,
        release_episode: Some(10),
        tvdb: None,
        tmdb: None,
        imdb: None,
    };
    assert!(service.lookup(&target).await.is_empty());
    assert!(service.lookup(&target).await.is_empty());
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    task.abort();
}

#[test]
fn theintrodb_groups_timestamps_mapping_nulls_to_media_bounds() {
    let document = json!({
        "tvdb_id":71663,"type":"tv","season":1,"episode":1,
        "intro":[{"start_ms":null,"end_ms":24716}],
        "credits":[{"start_ms":1320000,"end_ms":null}],
        "recap":[{"start_ms":90000,"end_ms":80000}]
    });
    assert_eq!(
        parse_theintrodb(&document),
        vec![super::segments::Segment {
            kind: "intro",
            start_ms: 0,
            end_ms: 24_716
        }]
    );
}

#[test]
fn skipdb_maps_outro_to_credits_and_ignores_preview() {
    let document = json!({
        "imdb_id":"tt0903747","season":1,"episode":1,
        "segments":{
            "intro":{"start_ms":229500,"end_ms":246500,"confidence":0.75},
            "recap":null,
            "outro":{"start_ms":3434000,"end_ms":3500000,"confidence":0.75},
            "preview":{"start_ms":3500000,"end_ms":3520000,"confidence":0.5}
        }
    });
    assert_eq!(
        parse_skipdb(&document),
        vec![
            super::segments::Segment {
                kind: "intro",
                start_ms: 229_500,
                end_ms: 246_500
            },
            super::segments::Segment {
                kind: "credits",
                start_ms: 3_434_000,
                end_ms: 3_500_000
            },
        ]
    );
}

#[tokio::test]
async fn theintrodb_adapter_serves_segments_from_a_fake_server() {
    let source = segments_fake(
        "/media",
        json!({"tvdb_id":71663,"type":"tv","season":1,"episode":1,"intro":[{"start_ms":1000,"end_ms":90000}],"credits":[{"start_ms":1320000,"end_ms":1380000}]}),
    )
    .await;
    let service = Service::fixture(None, None, Some(&source.base), None);
    let target = Target {
        season: Some(1),
        episode: 1,
        mal: None,
        anilist: None,
        release_episode: None,
        tvdb: Some(71663),
        tmdb: None,
        imdb: None,
    };
    assert_eq!(
        serde_json::to_value(service.lookup(&target).await).unwrap(),
        json!([
            {"kind":"intro","start_ms":1000,"end_ms":90000},
            {"kind":"credits","start_ms":1320000,"end_ms":1380000}
        ])
    );
}

#[tokio::test]
async fn skipdb_adapter_serves_segments_from_a_fake_server() {
    let source = segments_fake(
        "/api/segments",
        json!({
            "segments":{
                "intro":{"start_ms":229500,"end_ms":246500},
                "recap":null,
                "outro":{"start_ms":3434000,"end_ms":3500000},
                "preview":null
            }
        }),
    )
    .await;
    let service = Service::fixture(None, None, None, Some(&source.base));
    let target = Target {
        season: Some(1),
        episode: 1,
        mal: None,
        anilist: None,
        release_episode: None,
        tvdb: None,
        tmdb: None,
        imdb: Some("tt0903747".into()),
    };
    assert_eq!(
        serde_json::to_value(service.lookup(&target).await).unwrap(),
        json!([
            {"kind":"intro","start_ms":229500,"end_ms":246500},
            {"kind":"credits","start_ms":3434000,"end_ms":3500000}
        ])
    );
}

#[tokio::test]
async fn two_sources_merge_in_priority_order_one_segment_per_kind() {
    let theintrodb = segments_fake(
        "/media",
        json!({"tvdb_id":71663,"type":"tv","season":1,"episode":1,"intro":[{"start_ms":1000,"end_ms":90000}]}),
    )
    .await;
    let skipdb = segments_fake(
        "/api/segments",
        json!({
            "segments":{
                "intro":{"start_ms":500,"end_ms":80000},
                "recap":{"start_ms":0,"end_ms":30000},
                "outro":{"start_ms":1300000,"end_ms":1390000},
                "preview":null
            }
        }),
    )
    .await;
    let service = Service::fixture(None, None, Some(&theintrodb.base), Some(&skipdb.base));
    let target = Target {
        season: Some(1),
        episode: 1,
        mal: None,
        anilist: None,
        release_episode: None,
        tvdb: Some(71663),
        tmdb: Some(46260),
        imdb: Some("tt0903747".into()),
    };
    assert_eq!(
        serde_json::to_value(service.lookup(&target).await).unwrap(),
        json!([
            {"kind":"recap","start_ms":0,"end_ms":30000},
            {"kind":"intro","start_ms":1000,"end_ms":90000},
            {"kind":"credits","start_ms":1300000,"end_ms":1390000}
        ])
    );
}

#[tokio::test]
async fn an_anime_episode_asks_for_its_mal_release_episode_and_serves_both_segments() {
    let (mut app, stack_task) = stack().await;
    let source = source(fixture_rows()).await;
    let fribb = fribb(json!([])).await;
    app.segments = Arc::new(Service::fixture(Some(&source.base), None, None, None));
    app.cross_ids = Arc::new(super::cross_ids::Service::fixture(&fribb.base));
    let card = episode_card(3, 22);
    let resolved = target(&app, &lease(), &card).await.unwrap();
    assert_eq!(resolved.mal, Some(38524));
    assert_eq!(resolved.release_episode, Some(10));
    assert_eq!(
        answer(&app, &lease(), Some(&card)).await,
        json!({"segments":[
            {"kind":"intro","start_ms":310571,"end_ms":400571},
            {"kind":"credits","start_ms":1290000,"end_ms":1380000}
        ]})
    );
    assert_eq!(
        answer(&app, &lease(), Some(&episode_card(3, 23))).await,
        json!({"segments":[]})
    );
    assert_eq!(answer(&app, &lease(), None).await, json!({"segments":[]}));
    stack_task.abort();
}

#[tokio::test]
async fn an_episode_card_whose_own_ids_are_opaque_uses_its_parent_show() {
    let (mut app, stack_task) = stack().await;
    let source = source(fixture_rows()).await;
    let fribb = fribb(json!([])).await;
    app.segments = Arc::new(Service::fixture(Some(&source.base), None, None, None));
    app.cross_ids = Arc::new(super::cross_ids::Service::fixture(&fribb.base));
    let mut card = episode_card(3, 22);
    card.identity.ids = vec![viptv_engine::identity::ExternalId {
        namespace: "opaque".into(),
        value: "release-1x10".into(),
    }];
    let resolved = target(&app, &lease(), &card).await.unwrap();
    assert_eq!(resolved.mal, Some(38524));
    assert_eq!(resolved.release_episode, Some(10));
    assert_eq!(
        answer(&app, &lease(), Some(&card)).await,
        json!({"segments":[
            {"kind":"intro","start_ms":310571,"end_ms":400571},
            {"kind":"credits","start_ms":1290000,"end_ms":1380000}
        ]})
    );
    stack_task.abort();
}
async fn animeskip_source(episodes: Value) -> Source {
    let reads = Arc::new(AtomicUsize::new(0));
    let count = reads.clone();
    let routes = Router::new()
        .route(
            "/graphql",
            post(
                move |headers: axum::http::HeaderMap, body: String| {
                    let count = count.clone();
                    let episodes = episodes.clone();
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        assert_eq!(
                            headers
                                .get("x-client-id")
                                .and_then(|value| value.to_str().ok()),
                            Some(super::segments::ANIMESKIP_CLIENT_ID)
                        );
                        let body: Value = serde_json::from_str(&body).unwrap();
                        let query = body["query"].as_str().unwrap();
                        let data = if query.contains("findShowsByExternalId") {
                            assert_eq!(body["variables"]["serviceId"], "104578");
                            json!({"findShowsByExternalId":[{"id":"show-1","deletedAt":null}]})
                        } else {
                            assert!(query.contains("findEpisodesByShowId"), "{query}");
                            assert_eq!(body["variables"]["showId"], "show-1");
                            json!({"findEpisodesByShowId": episodes})
                        };
                        (StatusCode::OK, axum::Json(json!({"data": data})))
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
    Source {
        base: format!("http://{address}/graphql"),
        reads,
        task,
    }
}

fn animeskip_episodes() -> Value {
    json!([
        {"season":"1","number":"9","absoluteNumber":"9","deletedAt":null,"timestamps":[
            {"at":300.0,"type":{"name":"Intro"}}
        ]},
        {"season":"1","number":"10","absoluteNumber":"10","deletedAt":null,"timestamps":[
            {"at":0.0,"type":{"name":"Recap"}},
            {"at":24.5,"type":{"name":"Canon"}},
            {"at":90.0,"type":{"name":"Intro"}},
            {"at":180.0,"type":{"name":"Canon"}},
            {"at":1290.0,"type":{"name":"Credits"}},
            {"at":1350.0,"type":{"name":"Canon"}}
        ]}
    ])
}

fn animeskip_target() -> Target {
    Target {
        season: Some(3),
        episode: 22,
        mal: Some(38524),
        anilist: Some(104578),
        release_episode: Some(10),
        tvdb: None,
        tmdb: None,
        imdb: None,
    }
}

#[test]
fn animeskip_timestamps_end_at_the_next_timestamp_and_ignore_unknown_types() {
    let document = json!({"data":{"findEpisodesByShowId":[
        {"season":"1","number":"10","absoluteNumber":"10","deletedAt":null,"timestamps":[
            {"at":1290.0,"type":{"name":"Credits"}},
            {"at":0.0,"type":{"name":"Recap"}},
            {"at":24.5,"type":{"name":"Canon"}},
            {"at":90.0,"type":{"name":"Intro"}},
            {"at":180.0,"type":{"name":"Canon"}},
            {"at":1350.0,"type":{"name":"Mixed Credits"}},
            {"at":1380.0,"type":{"name":"Canon"}},
            {"at":1400.0,"type":{"name":"Preview"}},
            {"at":-4.0,"type":{"name":"Intro"}}
        ]}
    ]}});
    assert_eq!(
        animeskip_segments(&document, &animeskip_target()),
        Some(vec![
            super::segments::Segment {
                kind: "recap",
                start_ms: 0,
                end_ms: 24_500
            },
            super::segments::Segment {
                kind: "intro",
                start_ms: 90_000,
                end_ms: 180_000
            },
            super::segments::Segment {
                kind: "credits",
                start_ms: 1_290_000,
                end_ms: 1_350_000
            },
        ])
    );
    assert_eq!(
        animeskip_segments(&json!({"data":{"findEpisodesByShowId":[]}}), &animeskip_target()),
        None
    );
    let runaway = json!({"data":{"findEpisodesByShowId":[
        {"season":"1","number":"10","deletedAt":null,"timestamps":[
            {"at":90.0,"type":{"name":"Intro"}},
            {"at":900.0,"type":{"name":"Canon"}}
        ]}
    ]}});
    assert_eq!(animeskip_segments(&runaway, &animeskip_target()), None);
    let deleted = json!({"data":{"findEpisodesByShowId":[
        {"season":"1","number":"10","deletedAt":"2024-01-01T00:00:00Z","timestamps":[
            {"at":90.0,"type":{"name":"Intro"}},
            {"at":180.0,"type":{"name":"Canon"}}
        ]}
    ]}});
    assert_eq!(animeskip_segments(&deleted, &animeskip_target()), None);
}

#[test]
fn animeskip_falls_back_to_the_season_coordinates_when_no_release_episode_is_known() {
    let document = json!({"data":{"findEpisodesByShowId":[
        {"season":"2","number":"1","deletedAt":null,"timestamps":[
            {"at":10.0,"type":{"name":"Intro"}},
            {"at":100.0,"type":{"name":"Canon"}}
        ]},
        {"season":"2","number":"2","deletedAt":null,"timestamps":[
            {"at":20.0,"type":{"name":"Intro"}},
            {"at":110.0,"type":{"name":"Canon"}}
        ]}
    ]}});
    let coordinates = Target {
        season: Some(2),
        episode: 2,
        mal: None,
        anilist: Some(104578),
        release_episode: None,
        tvdb: None,
        tmdb: None,
        imdb: None,
    };
    assert_eq!(
        animeskip_segments(&document, &coordinates),
        Some(vec![super::segments::Segment {
            kind: "intro",
            start_ms: 20_000,
            end_ms: 110_000
        }])
    );
    let missing = Target {
        season: Some(2),
        episode: 3,
        mal: None,
        anilist: Some(104578),
        release_episode: None,
        tvdb: None,
        tmdb: None,
        imdb: None,
    };
    assert_eq!(animeskip_segments(&document, &missing), None);
}

#[tokio::test]
async fn the_animeskip_adapter_asks_the_anilist_show_and_serves_the_episode() {
    let source = animeskip_source(animeskip_episodes()).await;
    let service = Service::fixture(None, Some(&source.base), None, None);
    assert_eq!(
        serde_json::to_value(service.lookup(&animeskip_target()).await).unwrap(),
        json!([
            {"kind":"recap","start_ms":0,"end_ms":24500},
            {"kind":"intro","start_ms":90000,"end_ms":180000},
            {"kind":"credits","start_ms":1290000,"end_ms":1350000}
        ])
    );
    assert_eq!(source.reads.load(Ordering::SeqCst), 2);
    service.lookup(&animeskip_target()).await;
    assert_eq!(source.reads.load(Ordering::SeqCst), 2);
    let unknown = Target {
        season: Some(3),
        episode: 23,
        mal: Some(38524),
        anilist: Some(104578),
        release_episode: Some(11),
        tvdb: None,
        tmdb: None,
        imdb: None,
    };
    assert!(service.lookup(&unknown).await.is_empty());
    assert_eq!(source.reads.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn animeskip_stays_off_unless_enabled_and_an_anilist_id_is_known() {
    let source = animeskip_source(animeskip_episodes()).await;
    let off = Service::fixture(None, None, None, None);
    assert!(off.lookup(&animeskip_target()).await.is_empty());
    let service = Service::fixture(None, Some(&source.base), None, None);
    let unmapped = Target {
        season: Some(3),
        episode: 22,
        mal: Some(38524),
        anilist: None,
        release_episode: Some(10),
        tvdb: None,
        tmdb: None,
        imdb: None,
    };
    assert!(service.lookup(&unmapped).await.is_empty());
    assert_eq!(source.reads.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn animeskip_merges_after_aniskip_and_fills_the_kinds_it_lacks() {
    let aniskip = source(fixture_rows()).await;
    let animeskip = animeskip_source(json!([
        {"season":"1","number":"10","absoluteNumber":"10","deletedAt":null,"timestamps":[
            {"at":0.0,"type":{"name":"Recap"}},
            {"at":20.0,"type":{"name":"Canon"}},
            {"at":95.0,"type":{"name":"Intro"}},
            {"at":185.0,"type":{"name":"Canon"}}
        ]}
    ]))
    .await;
    let service = Service::fixture(Some(&aniskip.base), Some(&animeskip.base), None, None);
    assert_eq!(
        serde_json::to_value(service.lookup(&animeskip_target()).await).unwrap(),
        json!([
            {"kind":"recap","start_ms":0,"end_ms":20000},
            {"kind":"intro","start_ms":310571,"end_ms":400571},
            {"kind":"credits","start_ms":1290000,"end_ms":1380000}
        ])
    );
}

#[tokio::test]
async fn an_anime_episode_asks_animeskip_with_its_anilist_show_and_release_episode() {
    let (mut app, stack_task) = stack().await;
    let source = animeskip_source(animeskip_episodes()).await;
    let fribb = fribb(json!([])).await;
    app.segments = Arc::new(Service::fixture(None, Some(&source.base), None, None));
    app.cross_ids = Arc::new(super::cross_ids::Service::fixture(&fribb.base));
    let card = episode_card(3, 22);
    let resolved = target(&app, &lease(), &card).await.unwrap();
    assert_eq!(resolved.anilist, Some(104578));
    assert_eq!(resolved.release_episode, Some(10));
    assert_eq!(
        answer(&app, &lease(), Some(&card)).await,
        json!({"segments":[
            {"kind":"recap","start_ms":0,"end_ms":24500},
            {"kind":"intro","start_ms":90000,"end_ms":180000},
            {"kind":"credits","start_ms":1290000,"end_ms":1350000}
        ]})
    );
    stack_task.abort();
}
