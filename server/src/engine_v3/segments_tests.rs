use super::anime_show_tests::stack;
use super::segments::{
    answer, parse, parse_skipdb, parse_theintrodb, target, Service, Target,
};
use crate::{auth, ResourceLease};
use axum::{
    extract::{Path, RawQuery},
    http::StatusCode,
    routing::get,
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
        {"namespace":"mal","value":"38524"}
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
    let service = Service::fixture(Some(&source.base), None, None);
    let target = Target {
        season: None,
        episode: 10,
        mal: Some(38524),
        mal_episode: Some(10),
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
        mal_episode: Some(11),
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
            mal_episode: Some(1),
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
    let service = Service::fixture(Some(&format!("http://{address}")), None, None);
    let target = Target {
        season: None,
        episode: 10,
        mal: Some(38524),
        mal_episode: Some(10),
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
    let service = Service::fixture(None, Some(&source.base), None);
    let target = Target {
        season: Some(1),
        episode: 1,
        mal: None,
        mal_episode: None,
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
    let service = Service::fixture(None, None, Some(&source.base));
    let target = Target {
        season: Some(1),
        episode: 1,
        mal: None,
        mal_episode: None,
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
    let service = Service::fixture(None, Some(&theintrodb.base), Some(&skipdb.base));
    let target = Target {
        season: Some(1),
        episode: 1,
        mal: None,
        mal_episode: None,
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
    app.segments = Arc::new(Service::fixture(Some(&source.base), None, None));
    app.cross_ids = Arc::new(super::cross_ids::Service::fixture(&fribb.base));
    let card = episode_card(3, 22);
    let resolved = target(&app, &lease(), &card).await.unwrap();
    assert_eq!(resolved.mal, Some(38524));
    assert_eq!(resolved.mal_episode, Some(10));
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
    app.segments = Arc::new(Service::fixture(Some(&source.base), None, None));
    app.cross_ids = Arc::new(super::cross_ids::Service::fixture(&fribb.base));
    let mut card = episode_card(3, 22);
    card.identity.ids = vec![viptv_engine::identity::ExternalId {
        namespace: "opaque".into(),
        value: "release-1x10".into(),
    }];
    let resolved = target(&app, &lease(), &card).await.unwrap();
    assert_eq!(resolved.mal, Some(38524));
    assert_eq!(resolved.mal_episode, Some(10));
    assert_eq!(
        answer(&app, &lease(), Some(&card)).await,
        json!({"segments":[
            {"kind":"intro","start_ms":310571,"end_ms":400571},
            {"kind":"credits","start_ms":1290000,"end_ms":1380000}
        ]})
    );
    stack_task.abort();
}