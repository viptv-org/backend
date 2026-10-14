use super::*;
use crate::{auth_integration_tests::fixture, test_support::request};
use axum::routing::{get, post};
use std::sync::atomic::{AtomicUsize, Ordering};

async fn server() -> (App, tokio::task::JoinHandle<()>, Arc<AtomicUsize>) {
    let writes = Arc::new(AtomicUsize::new(0));
    let count = writes.clone();
    let router = axum::Router::new()
        .route(
            "/oauth2/token",
            post(|axum::Form(body): axum::Form<HashMap<String,String>>| async move {
                assert_eq!(body["grant_type"], "authorization_code");
                assert_eq!(body["client_secret"], "fixture-secret");
                assert!(body["code_verifier"].len()>=43);
                axum::Json(json!({"access_token":"fixture-private-token","refresh_token":"fixture-refresh-token","token_type":"Bearer","expires_in":604800,"scope":"media:read media:write"}))
            }),
        )
        .route(
            "/search/movie",
            get(|headers: axum::http::HeaderMap| async move {
                assert!(!headers.contains_key(header::AUTHORIZATION));
                axum::Json(
                    json!([{"title":"Movie","ids":{"simkl_id":10},"endpoint_type":"movies"}]),
                )
            }),
        )
        .route(
            "/movies/10",
            get(|| async { axum::Json(json!({"title":"Movie","ids":{"simkl":10},"overview":"Full movie detail","runtime":120,"ratings":{"imdb":{"rating":8.4,"votes":2000}},"similar":[{"title":"Related","type":"movie","ids":{"simkl":12}},{"title":"Movie","type":"movie","ids":{"simkl":10}}]})) }),
        )
        .route(
            "/movies/11",
            get(|| async { axum::Json(json!({"title":"Wrong","ids":{"simkl":12}})) }),
        )
        .route(
            "/tv/20",
            get(|| async { axum::Json(json!({"title":"Show","ids":{"simkl":20}})) }),
        )
        .route(
            "/tv/episodes/20",
            get(|| async {
                axum::Json(
                    json!([{ "title":"Pilot", "season":1, "episode":2, "ids":{"simkl_id":30}}]),
                )
            }),
        )
        .route(
            "/sync/history",
            post(
                move |headers: axum::http::HeaderMap, axum::Json(body): axum::Json<Value>| {
                    let count = count.clone();
                    async move {
                        assert_eq!(
                            headers[header::AUTHORIZATION],
                            "Bearer fixture-private-token"
                        );
                        count.fetch_add(1, Ordering::SeqCst);
                        if body["shows"].is_array() {
                            assert_eq!(body["shows"][0]["seasons"][0]["episodes"][0]["number"], 2);
                            axum::Json(json!({"added":{"episodes":1}}))
                        } else {
                            axum::Json(json!({"added":{"movies":1}}))
                        }
                    }
                },
            ),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let mut app = fixture();
    app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
    (app, task, writes)
}

async fn start(app: &App) -> String {
    let (status, body) = request(
        app,
        "member-token-1",
        "POST",
        "/api/v3/profiles/1/simkl/authorize",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.to_string().contains("fixture-secret"));
    let url = url::Url::parse(body["authorization_url"].as_str().unwrap()).unwrap();
    assert_eq!(url.path(), "/oauth2/authorize");
    let fields = url.query_pairs().collect::<HashMap<_, _>>();
    assert_eq!(fields["code_challenge_method"], "S256");
    assert_eq!(fields["scope"], "media:read media:write");
    assert_eq!(fields["code_challenge"].len(), 43);
    url.query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .into_owned()
}

async fn link(app: &App) -> String {
    let state = start(app).await;
    use tower::ServiceExt;
    let response = crate::router(app.clone(), None)
        .oneshot(
            axum::http::Request::builder()
                .uri(format!(
                    "/api/v3/simkl/callback?state={state}&code=fixture-code&iss=https%3A%2F%2Fsimkl.com"
                ))
                .header(header::HOST, "tv.example")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(response.headers()[header::LOCATION], "/?simkl=linked");
    state
}

mod metadata;
mod oauth;

mod revocation;

mod discovery;
mod calendar_browse;

mod mapping;

mod tracking_completion;
mod tracking_fixture;
mod tracking_history;
mod tracking_lifecycle;

mod tokens;

mod token_revocation;

mod history_clear;

mod watchlists;
mod import_continuation;

mod episode_pages;

mod large_episode_pages;
