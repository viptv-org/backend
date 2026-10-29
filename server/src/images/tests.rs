use super::*;
#[test]
fn descriptors_accept_only_public_provider_images_and_rewrite_asset_fields_only() {
    for href in [
        "https://simkl.in/episodes/12/abcdef_w.webp",
        "https://wsrv.nl/?url=https://simkl.in/episodes/12/abcdef_w.webp&q=90",
        "/api/images/simkl/episodes/12/abcdef_w.webp",
        "https://native.example:18451/api/images/simkl/episodes/12/abcdef_w.webp",
    ] {
        let image = descriptor::Image::href(href).unwrap();
        assert_eq!(
            image.proxy(),
            "/api/images/simkl/episodes/12/abcdef_w.webp"
        );
    }
    for href in [
        "https://user:pass@simkl.in/episodes/x_w.webp",
        "https://wsrv.nl/?url=http://127.0.0.1/file",
        "https://simkl.in/episodes/../secret_w.webp",
        "https://evil.example/episodes/a_w.webp",
        "https://simkl.in/episodes/a_w.webp?token=secret",
        "https://simkl.in/episodes/a_w.html",
    ] {
        assert!(descriptor::Image::href(href).is_none(), "bad descriptor");
    }
    for (folder, asset) in [
        ("other", "a_w.webp"),
        ("episodes", "a_d.jpg"),
        ("episodes", "../../secret_w.webp"),
        ("fanart", "a_d.webp"),
        ("posters", "a_xl.webp"),
        ("episodes", "a_s.webp"),
        ("fanart", "a_s192.webp"),
        ("posters", "a_m.html"),
    ] {
        assert!(descriptor::Image::parts(folder, asset).is_none());
    }
    let mut value = json!({"card":{"landscape":"https://wsrv.nl/?url=https://simkl.in/episodes/12/abcdef_w.webp","description":"https://simkl.in/episodes/12/abcdef_w.webp","logo":"https://images.metahub.space/logo/medium/tt123/img"},"credentials":{"url":"https://simkl.in/episodes/12/abcdef_w.webp"}});
    rewrite(&mut value, "proxy");
    assert_eq!(
        value["card"]["landscape"],
        "/api/images/simkl/episodes/12/abcdef_w.webp"
    );
    assert_eq!(
        value["card"]["description"],
        "https://simkl.in/episodes/12/abcdef_w.webp"
    );
    assert!(
        value["card"]["logo"]
            .as_str()
            .unwrap()
            .starts_with("https://images.metahub.space/")
    );
    assert_eq!(
        value["credentials"]["url"],
        "https://simkl.in/episodes/12/abcdef_w.webp"
    );
}
#[test]
fn descriptors_accept_every_size_and_extension_simkl_serves() {
    for (folder, asset) in [
        ("posters", "11/abc_s.webp"),
        ("posters", "11/abc_cm.webp"),
        ("posters", "11/abc_c.webp"),
        ("posters", "11/abc_ca.jpg"),
        ("posters", "11/abc_m.webp"),
        ("episodes", "12/abc_c.webp"),
        ("episodes", "12/abc_m.jpg"),
        ("fanart", "11/abc_mobile.webp"),
        ("fanart", "11/abc_s48.webp"),
        ("fanart", "11/abc_d.jpg"),
        ("posters", "11/abc_m.gif"),
        ("episodes", "12/abc_c.gif"),
    ] {
        assert_eq!(
            descriptor::Image::parts(folder, asset)
                .unwrap_or_else(|| panic!("{asset}"))
                .proxy(),
            format!("/api/images/simkl/{folder}/{asset}")
        );
    }
    assert_eq!(
        descriptor::Image::href("https://simkl.in/fanart/11/abc_w.gif")
            .unwrap()
            .direct(),
        "https://simkl.in/fanart/11/abc_medium.gif"
    );
}
#[tokio::test]
async fn negotiation_preserves_legacy_and_changes_only_opt_in_json_assets() {
    use tower::ServiceExt;
    let app=axum::Router::new().route("/",axum::routing::get(||async{axum::Json(json!({"poster":"https://wsrv.nl/?url=https://simkl.in/posters/12/abcdef_m.webp&q=90"}))})).layer(middleware::from_fn(negotiate));
    for (mode, prefix) in [
        (None, "https://wsrv.nl/"),
        (Some("direct"), "https://simkl.in/"),
        (Some("proxy"), "/api/images/simkl/"),
    ] {
        let mut request = axum::http::Request::builder().uri("/");
        if let Some(mode) = mode {
            request = request.header("x-viptv-image-delivery", mode);
        }
        let response = app
            .clone()
            .oneshot(request.body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 10000)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert!(value["poster"].as_str().unwrap().starts_with(prefix));
    }
    let app = crate::auth_integration_tests::fixture();
    let response = crate::router(app)
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/images/simkl/other/a_w.webp")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn public_image_preflight_requires_no_account_and_allows_public_image_headers() {
    use tower::ServiceExt;
    let app = crate::auth_integration_tests::fixture();
    let response = crate::router(app)
        .oneshot(
            axum::http::Request::builder()
                .method("OPTIONS")
                .uri("/api/images/simkl/episodes/12/abcdef_w.webp")
                .header(header::ORIGIN, "https://public-view.example")
                .header(
                    header::ACCESS_CONTROL_REQUEST_HEADERS,
                    "x-viptv-image-delivery",
                )
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
    assert!(
        response.headers()[header::ACCESS_CONTROL_ALLOW_HEADERS]
            .to_str()
            .unwrap()
            .contains("x-viptv-image-delivery")
    );
    assert!(!response.headers().contains_key(header::SET_COOKIE));
}

#[tokio::test]
async fn stable_descriptor_urls_revalidate_and_a_replaced_upstream_image_gets_a_new_etag() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::ServiceExt;
    let generation = Arc::new(AtomicUsize::new(0));
    let count = generation.clone();
    let upstream = axum::Router::new().route(
        "/posters/11/abc_m.webp",
        axum::routing::get(move || {
            let count = count.clone();
            async move {
                let mut body = b"RIFF\x10\0\0\0WEBP".to_vec();
                body.push(b'0' + count.load(Ordering::SeqCst) as u8);
                ([(header::CONTENT_TYPE, "image/webp")], body)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
    let app = crate::auth_integration_tests::fixture();
    app.images
        .deliver_from(url::Url::parse(&format!("http://{address}/")).unwrap());
    let images = app.images.clone();
    let router = crate::router(app);
    let fetch = move |conditional: Option<String>| {
        let router = router.clone();
        async move {
            let mut request =
                axum::http::Request::builder().uri("/api/images/simkl/posters/11/abc_m.webp");
            if let Some(etag) = conditional {
                request = request.header(header::IF_NONE_MATCH, etag);
            }
            router
                .oneshot(request.body(axum::body::Body::empty()).unwrap())
                .await
                .unwrap()
        }
    };
    let first = fetch(None).await;
    assert_eq!(first.status(), StatusCode::OK);
    let cache_control = first.headers()[header::CACHE_CONTROL]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        !cache_control.contains("immutable"),
        "stable descriptors must stay revalidatable: {cache_control}"
    );
    let max_age = cache_control
        .split("max-age=")
        .nth(1)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap();
    assert!(
        max_age > 0 && max_age <= MAX_AGE.as_secs(),
        "advertised freshness must not outlive the server copy: {max_age}"
    );
    let etag = first.headers()[header::ETAG].to_str().unwrap().to_owned();
    assert!(axum::body::to_bytes(first.into_body(), 10000)
        .await
        .unwrap()
        .ends_with(b"0"));
    let unchanged = fetch(Some(etag.clone())).await;
    assert_eq!(unchanged.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(unchanged.headers()[header::ETAG], etag.as_str());
    assert_eq!(unchanged.headers()[header::CACHE_CONTROL], cache_control.as_str());
    assert!(axum::body::to_bytes(unchanged.into_body(), 10000)
        .await
        .unwrap()
        .is_empty());
    generation.store(1, Ordering::SeqCst);
    images.expire().await;
    let replaced = fetch(Some(etag.clone())).await;
    assert_eq!(replaced.status(), StatusCode::OK);
    let new_etag = replaced.headers()[header::ETAG].to_str().unwrap().to_owned();
    assert_ne!(new_etag, etag);
    assert!(axum::body::to_bytes(replaced.into_body(), 10000)
        .await
        .unwrap()
        .ends_with(b"1"));
    assert_eq!(
        fetch(Some("*".into())).await.status(),
        StatusCode::NOT_MODIFIED
    );
    assert_eq!(
        fetch(Some("\"0000000000000000\"".into())).await.status(),
        StatusCode::OK
    );
    server.abort();
}
