use super::*;
#[tokio::test]
async fn fetches_are_bounded_coalesced_cached_and_never_forward_inbound_credentials() {
    use axum::routing::get;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let reads = Arc::new(AtomicUsize::new(0));
    let count = reads.clone();
    let router = axum::Router::new()
        .route(
            "/image",
            get(move |headers: axum::http::HeaderMap| {
                let count = count.clone();
                async move {
                    assert!(!headers.contains_key(header::AUTHORIZATION));
                    assert!(!headers.contains_key(header::COOKIE));
                    count.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    (
                        [(header::CONTENT_TYPE, "text/html")],
                        b"RIFF\x10\0\0\0WEBPtest".to_vec(),
                    )
                }
            }),
        )
        .route("/large", get(|| async { vec![0u8; 2 * 1024 * 1024 + 1] }))
        .route(
            "/html",
            get(|| async { b"<html>access denied</html>".to_vec() }),
        )
        .route(
            "/redirect",
            get(|| async {
                (
                    StatusCode::FOUND,
                    [(header::LOCATION, "http://127.0.0.1/private")],
                )
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let image = descriptor::Image::parts("episodes", "12/abcdef_w.webp").unwrap();
    let service = Service::new();
    let target = url::Url::parse(&format!("http://{address}/image")).unwrap();
    let (a, b) = tokio::join!(
        service.fetch(image.clone(), Some(target.clone())),
        service.fetch(image.clone(), Some(target))
    );
    assert_eq!(a.unwrap().content_type, "image/webp");
    assert_eq!(b.unwrap().bytes.len(), 16);
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    let full = Service::new();
    full.records.lock().await.bytes = 32 * 1024 * 1024;
    let bounded = full
        .fetch(
            descriptor::Image::parts("episodes", "budget_w.webp").unwrap(),
            Some(url::Url::parse(&format!("http://{address}/image")).unwrap()),
        )
        .await
        .unwrap();
    assert_eq!(bounded.bytes.len(), 16);
    assert_eq!(full.records.lock().await.bytes, 32 * 1024 * 1024);
    assert!(full
        .records
        .lock()
        .await
        .entries
        .values()
        .next()
        .unwrap()
        .lock()
        .await
        .value
        .is_none());
    for path in ["html", "redirect", "large"] {
        let image = descriptor::Image::parts("episodes", &format!("{path}_w.webp")).unwrap();
        assert!(service
            .fetch(
                image,
                Some(url::Url::parse(&format!("http://{address}/{path}")).unwrap())
            )
            .await
            .is_err());
    }
    task.abort();
}

#[tokio::test]
async fn queued_images_wait_for_a_slot_and_local_deadlines_never_poison_the_cache() {
    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = reads.clone();
    let routes = axum::Router::new().route(
        "/image",
        axum::routing::get(move || {
            let count = count.clone();
            async move {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                b"RIFF\x10\0\0\0WEBPtest".to_vec()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    let service = Arc::new(Service::new());
    let target = url::Url::parse(&format!("http://{address}/image")).unwrap();
    let held = service.limit.acquire_many(6).await.unwrap();
    let queued = service.clone();
    let target_copy = target.clone();
    let first = descriptor::Image::parts("posters", "queued_m.webp").unwrap();
    let image_copy = first.clone();
    let worker = tokio::spawn(async move { queued.fetch(image_copy, Some(target_copy)).await });
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(
        !worker.is_finished(),
        "queue capacity is backpressure, not an image failure"
    );
    assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 0);
    drop(held);
    assert_eq!(worker.await.unwrap().unwrap().content_type, "image/webp");
    let held = service.limit.acquire_many(6).await.unwrap();
    let second = descriptor::Image::parts("posters", "retry_m.webp").unwrap();
    let started = Instant::now();
    let result = service.fetch(second.clone(), Some(target.clone())).await;
    assert_eq!(result.err().unwrap().0, StatusCode::SERVICE_UNAVAILABLE);
    assert!(started.elapsed() < Duration::from_secs(9));
    let entry = service.records.lock().await.entries[&second].clone();
    assert!(
        entry.lock().await.value.is_none(),
        "local timeout must not be negative cached"
    );
    drop(held);
    assert_eq!(
        service
            .fetch(second, Some(target))
            .await
            .unwrap()
            .content_type,
        "image/webp"
    );
    assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 2);
    server.abort();
}

#[tokio::test]
async fn gif_artwork_keeps_its_bytes_and_is_served_as_gif() {
    let routes = axum::Router::new()
        .route(
            "/gif",
            axum::routing::get(|| async { b"GIF89a\x01\x00\x01\x00\x80\x00\x00;\x00".to_vec() }),
        )
        .route(
            "/legacy",
            axum::routing::get(|| async { b"GIF87a\x01\x00\x01\x00;\x00\x00\x00".to_vec() }),
        )
        .route(
            "/not-an-image",
            axum::routing::get(|| async { b"<html>denied</html>".to_vec() }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    let service = Service::new();
    for path in ["gif", "legacy"] {
        let image = descriptor::Image::parts("posters", &format!("11/abc_{path}.gif")).unwrap();
        let asset = service
            .fetch(
                image,
                Some(url::Url::parse(&format!("http://{address}/{path}")).unwrap()),
            )
            .await
            .unwrap();
        assert_eq!(asset.content_type, "image/gif");
        assert!(asset.bytes.starts_with(b"GIF8"));
    }
    let image = descriptor::Image::parts("posters", "11/abc_broken.gif").unwrap();
    assert!(service
        .fetch(
            image,
            Some(url::Url::parse(&format!("http://{address}/not-an-image")).unwrap())
        )
        .await
        .is_err());
    server.abort();
}

#[test]
fn signature_reads_the_type_from_the_bytes() {
    assert_eq!(signature(b"GIF89a\x01\x00"), Some("image/gif"));
    assert_eq!(signature(b"GIF87a\x01\x00"), Some("image/gif"));
    assert_eq!(signature(b"RIFF\x10\0\0\0WEBPtest"), Some("image/webp"));
    assert_eq!(signature(b"\x89PNG\r\n\x1a\n"), Some("image/png"));
    assert_eq!(signature(b"\xff\xd8\xff\xe0"), Some("image/jpeg"));
    assert_eq!(signature(b"GIF90a"), None);
    assert_eq!(signature(b"<html>"), None);
}
