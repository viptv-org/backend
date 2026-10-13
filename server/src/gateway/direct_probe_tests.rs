use super::*;
use axum::{routing::get, Router};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

#[test]
fn secure_redirects_reject_downgrades_private_targets_and_control_values() {
    let source = Url::parse("https://public.example/media").unwrap();
    assert!(next(&source, "http://public.example/movie", false).is_none());
    assert!(next(&source, "https://127.0.0.1/movie", false).is_none());
    assert!(next(&source, "https://user:private@public.example/movie", false).is_none());
    assert!(next(&source, "/movie\nprivate", false).is_none());
    assert_eq!(next(&source, "/movie", false).unwrap().scheme(), "https");
}

#[tokio::test]
async fn real_307_keeps_declared_headers_only_on_the_original_recipient() {
    let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/media", target.local_addr().unwrap());
    let requests = Arc::new(AtomicUsize::new(0));
    let counted = requests.clone();
    let recipient = tokio::spawn(async move {
        axum::serve(
            target,
            Router::new().route(
                "/media",
                get(move |headers: axum::http::HeaderMap| {
                    let counted = counted.clone();
                    async move {
                        for name in [
                            "authorization",
                            "cookie",
                            "x-api-key",
                            "referer",
                            "user-agent",
                        ] {
                            assert!(headers.get(name).is_none(), "{name}");
                        }
                        assert_eq!(headers["range"], "bytes=0-0");
                        assert_eq!(headers["accept-encoding"], "identity");
                        counted.fetch_add(1, Ordering::SeqCst);
                        "x"
                    }
                }),
            ),
        )
        .await
        .ok()
        .unwrap()
    });
    let origin = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let initial = format!("http://{}/initial", origin.local_addr().unwrap());
    let final_url = url.clone();
    let owner = tokio::spawn(async move {
        axum::serve(
            origin,
            Router::new()
                .route(
                    "/initial",
                    get(|headers: axum::http::HeaderMap| async move {
                        assert_eq!(headers["authorization"], "Bearer source-private");
                        assert_eq!(headers["x-api-key"], "source-private");
                        (
                            axum::http::StatusCode::TEMPORARY_REDIRECT,
                            [("location", "/next")],
                        )
                    }),
                )
                .route(
                    "/next",
                    get(move |headers: axum::http::HeaderMap| {
                        let url = final_url.clone();
                        async move {
                            assert_eq!(headers["cookie"], "source-private");
                            (
                                axum::http::StatusCode::TEMPORARY_REDIRECT,
                                [("location", url)],
                            )
                        }
                    }),
                ),
        )
        .await
        .ok()
        .unwrap()
    });
    let headers = [
        ("authorization", "Bearer source-private"),
        ("cookie", "source-private"),
        ("x-api-key", "source-private"),
        ("referer", "https://source.example"),
        ("user-agent", "SourceClient"),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.into()))
    .collect();
    let result = inspect(&initial, &headers, true, &|| async { Ok(()) })
        .await
        .ok()
        .unwrap()
        .unwrap();
    assert_eq!(result.url, url);
    assert!(result.headers.is_empty());
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    owner.abort();
    recipient.abort();
}

#[tokio::test]
async fn revoked_authority_during_probe_never_returns_a_direct_destination() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/media", listener.local_addr().unwrap());
    let entered = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    let start = entered.clone();
    let finish = resume.clone();
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/media",
                get(move || {
                    let start = start.clone();
                    let finish = finish.clone();
                    async move {
                        start.notify_one();
                        finish.notified().await;
                        "x"
                    }
                }),
            ),
        )
        .await
        .ok()
        .unwrap()
    });
    let revoked = Arc::new(AtomicBool::new(false));
    let state = revoked.clone();
    let probe = tokio::spawn(async move {
        inspect(&url, &BTreeMap::new(), true, &|| async {
            if state.load(Ordering::SeqCst) {
                Err(Error::Code("playback_expired"))
            } else {
                Ok(())
            }
        })
        .await
    });
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .ok()
        .unwrap();
    revoked.store(true, Ordering::SeqCst);
    resume.notify_one();
    assert!(matches!(
        probe.await.unwrap(),
        Err(Error::Code("playback_expired"))
    ));
    task.abort();
}

#[tokio::test]
async fn missing_location_and_redirect_loops_select_gateway_instead_of_unchecked_direct() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route(
                    "/missing",
                    get(|| async { axum::http::StatusCode::TEMPORARY_REDIRECT }),
                )
                .route(
                    "/loop",
                    get(move || {
                        let calls = calls.clone();
                        async move {
                            calls.fetch_add(1, Ordering::SeqCst);
                            (
                                axum::http::StatusCode::TEMPORARY_REDIRECT,
                                [("location", "/loop")],
                            )
                        }
                    }),
                ),
        )
        .await
        .ok()
        .unwrap()
    });
    for path in ["missing", "loop"] {
        assert!(inspect(
            &format!("{base}/{path}"),
            &BTreeMap::new(),
            true,
            &|| async { Ok(()) }
        )
        .await
        .ok()
        .unwrap()
        .is_none());
    }
    assert_eq!(count.load(Ordering::SeqCst), 5);
    task.abort();
}

#[tokio::test]
async fn authority_revoked_while_waiting_for_probe_capacity_prevents_dns_or_media_dispatch() {
    let probe = Probe::default();
    let permit = probe.slots.acquire_many(32).await.unwrap();
    let revoked = AtomicBool::new(false);
    let check = || async {
        if revoked.load(Ordering::SeqCst) {
            Err(Error::Code("playback_expired"))
        } else {
            Ok(())
        }
    };
    let headers = BTreeMap::new();
    let mut waiting = Box::pin(probe.resolve("https://public.example/media", &headers, check));
    assert!(futures::poll!(waiting.as_mut()).is_pending());
    revoked.store(true, Ordering::SeqCst);
    drop(permit);
    assert!(matches!(
        waiting.await,
        Err(Error::Code("playback_expired"))
    ));
}
