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
