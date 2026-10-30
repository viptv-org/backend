use super::anime_show_tests::stack;
use super::segments::{answer, parse, target, Segment, Service};
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
            Segment {
                kind: "recap",
                start_ms: 0,
                end_ms: 30_000
            },
            Segment {
                kind: "intro",
                start_ms: 310_571,
                end_ms: 400_571
            },
            Segment {
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
    let service = Service::fixture(&source.base);
    assert_eq!(
        serde_json::to_value(service.lookup(38524, 10).await).unwrap(),
        json!([
            {"kind":"intro","start_ms":310571,"end_ms":400571},
            {"kind":"credits","start_ms":1290000,"end_ms":1380000}
        ])
    );
    assert_eq!(source.reads.load(Ordering::SeqCst), 1);
    service.lookup(38524, 10).await;
    assert_eq!(source.reads.load(Ordering::SeqCst), 1);
    assert!(service.lookup(38524, 11).await.is_empty());
    assert_eq!(source.reads.load(Ordering::SeqCst), 2);
    assert!(service.lookup(0, 1).await.is_empty());
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
    let service = Service::fixture(&format!("http://{address}"));
    assert!(service.lookup(38524, 10).await.is_empty());
    assert!(service.lookup(38524, 10).await.is_empty());
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    task.abort();
}

#[tokio::test]
async fn an_anime_episode_asks_for_its_mal_release_episode_and_serves_both_segments() {    let (mut app, stack_task) = stack().await;
    let source = source(fixture_rows()).await;
    app.segments = Arc::new(Service::fixture(&source.base));
    let card = episode_card(3, 22);
    assert_eq!(target(&app, &lease(), &card).await, Some((38524, 10)));
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
    app.segments = Arc::new(Service::fixture(&source.base));
    let mut card = episode_card(3, 22);
    card.identity.ids = vec![viptv_engine::identity::ExternalId {
        namespace: "opaque".into(),
        value: "release-1x10".into(),
    }];
    assert_eq!(target(&app, &lease(), &card).await, Some((38524, 10)));
    assert_eq!(
        answer(&app, &lease(), Some(&card)).await,
        json!({"segments":[
            {"kind":"intro","start_ms":310571,"end_ms":400571},
            {"kind":"credits","start_ms":1290000,"end_ms":1380000}
        ]})
    );
    stack_task.abort();
}
