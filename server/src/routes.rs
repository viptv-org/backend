use super::*;

pub fn router(app: App) -> Router {
    router_with_viewers(app, None)
}

pub fn router_with_viewers(app: App, tv_dashboard: Option<std::path::PathBuf>) -> Router {
    let api = Router::new()
        .route("/profiles/:id/viewing-action", post(engine_v3::viewing_action::resolve))
        .route("/profiles/:id/addon-activation", get(engine_v3::viewing_action::addon_states).put(engine_v3::viewing_action::set_addon))
        .route(
            "/profiles/:id/source-integrations",
            get(engine_v3::source_integrations::schemas)
                .put(engine_v3::source_integrations::configure),
        )
        .route(
            "/profiles/:id/source-integrations/:connection",
            delete(engine_v3::source_integrations::remove),
        )
        .route(
            "/profiles/:id/library/membership",
            get(engine_v3::library::membership::get),
        )
        .route(
            "/profiles/:id/library/details",
            get(engine_v3::library::hydration::status),
        )
        .route(
            "/profiles/:id/imports/simkl",
            get(simkl::imports::read).post(simkl::imports::read),
        )
        .route(
            "/profiles/:id/preferences",
            get(engine_v3::preferences::get).put(engine_v3::preferences::update),
        )
        .route(
            "/profiles/:id/source-ranking",
            post(engine_v3::source_ranking::rank),
        )
        .route(
            "/profiles/:id/live/channels",
            get(engine_v3::live::channels),
        )
        .route(
            "/profiles/:id/live/favorites",
            get(engine_v3::live::favorites::get),
        )
        .route(
            "/profiles/:id/live/:channel/favorite",
            axum::routing::put(engine_v3::live::favorites::add)
                .delete(engine_v3::live::favorites::remove),
        )
        .route(
            "/profiles/:id/live/:channel/guide",
            get(engine_v3::live::guide),
        )
        .route(
            "/profiles/:id/live/:channel/sources",
            post(engine_v3::live::sources),
        )
        .route("/bootstrap", get(engine_v3::bootstrap))
        .route(
            "/profiles",
            get(engine_v3::profiles::list).post(engine_v3::profiles::create),
        )
        .route(
            "/profiles/:id",
            axum::routing::patch(engine_v3::profiles::update).delete(delete_profile_authenticated),
        )
        .route(
            "/profiles/:id/pin",
            get(engine_v3::profiles::pin_status).put(engine_v3::profiles::unsupported_pin),
        )
        .route("/parent/search", get(engine_v3::search::parent))
        .route(
            "/profiles/:id/approvals",
            get(kids::titles::list).post(kids::titles::approve),
        )
        .route("/parent/status", get(kids::status))
        .route("/parent/unlock", post(kids::unlock))
        .route("/parent/pin", axum::routing::put(kids::set_pin))
        .route(
            "/profiles/:id/integrations",
            get(engine_v3::configuration::get).put(engine_v3::configuration::update),
        )
        .route(
            "/profiles/:id/continue",
            get(engine_v3::continuation::get),
        )
        .route(
            "/profiles/:id/continue/next",
            post(engine_v3::continuation::resolve),
        )
        .route(
            "/profiles/:id/continue/visibility",
            axum::routing::put(engine_v3::continuation::update),
        )
        .route("/profiles/:id/home", get(engine_v3::home))
        .route(
            "/profiles/:id/revisions",
            get(engine_v3::revisions::get),
        )
        .route(
            "/profiles/:id/history",
            get(engine_v3::history::list).delete(engine_v3::history::clear),
        )
        .route(
            "/profiles/:id/library",
            get(engine_v3::library::list).put(engine_v3::library::save),
        )
        .route(
            "/profiles/:id/library/:kind/:item",
            delete(engine_v3::library::remove),
        )
        .route(
            "/account/devices",
            get(engine_v3::account_devices::get).delete(engine_v3::account_devices::remove_all),
        )
        .route(
            "/account/devices/:id",
            delete(engine_v3::account_devices::remove),
        )
        .route("/discover", get(engine_v3::discover))
        .route("/search", get(engine_v3::search::get))
        .route("/calendar", get(simkl::calendar::get))
        .route("/discovery-controls", get(engine_v3::discovery_controls))
        .route("/meta/:kind/:id", get(engine_v3::detail))
        .route(
            "/subtitles/:kind/:id",
            get(engine_v3::subtitles::discover),
        )
        .route(
            "/subtitle-content/:id",
            get(engine_v3::subtitles::content),
        )
        .route(
            "/profiles/:id/simkl",
            get(simkl::status).delete(simkl::disconnect),
        )
        .route("/profiles/:id/simkl/authorize", post(simkl::authorize))
        .route("/profiles/:id/simkl/search", get(simkl::search))
        .route(
            "/profiles/:id/simkl/meta/:category/:title",
            get(simkl::detail),
        )
        .route("/profiles/:id/simkl/watched", post(simkl::watched))
        .route("/profiles/:id/playback-events", post(simkl::playback))
        .route(
            "/profiles/:id/progress",
            axum::routing::put(simkl::progress),
        )
        .route(
            "/profiles/:id/imports/stremio/preview",
            post(stremio_import::preview),
        )
        .route(
            "/profiles/:id/imports/stremio/:preview_id/review",
            post(stremio_import::review),
        )
        .route(
            "/profiles/:id/imports/stremio/:preview_id/apply",
            post(stremio_import::apply),
        )
        .route(
            "/profiles/:id/imports/stremio/:preview_id",
            delete(stremio_import::cancel),
        )
        .route("/catalogs", get(catalogs))
        .route("/catalogs/revision", get(catalogs_revision))
        .route("/streams", post(provider::discovery_v2::start))
        .route(
            "/addons",
            get(addon::http_v2::list).post(addon::http_v2::create),
        )
        .route(
            "/addons/:id",
            axum::routing::patch(addon::http_v2::update).delete(addon::http_v2::delete),
        )
        .route("/addons/:id/logo", get(addon::http_v2::logo))
        .route(
            "/iptv/connections",
            get(provider::connections_v2::list).post(provider::connections_v2::create),
        )
        .route(
            "/iptv/connections/:id",
            axum::routing::patch(provider::connections_v2::update)
                .delete(provider::connections_v2::delete),
        )
        .route(
            "/iptv/connections/:id/credentials",
            axum::routing::put(provider::connections_v2::renew),
        )
        .route(
            "/iptv/connections/:id/refresh",
            get(provider::refresh_v2::get)
                .post(provider::refresh_v2::request)
                .delete(provider::refresh_v2::cancel),
        )
        .route(
            "/streams/:id",
            get(provider::discovery_v2::poll).delete(provider::discovery_v2::cancel),
        )
        .route("/iptv/guide/:id", get(provider::discovery_v2::guide))
        .route(
            "/iptv/live/:id/source",
            post(provider::discovery_v2::live_source),
        )
        .route(
            "/profiles/:id/favorites",
            get(favorites_authenticated).put(save_favorite_authenticated),
        )
        .route("/profiles/:id/favorites/page", get(library::favorites_page))
        .route("/profiles/:id/favorites/toggle", post(library::toggle))
        .route(
            "/profiles/:id/favorites/:kind/:item",
            delete(delete_favorite_authenticated),
        )
        .route(
            "/profiles/:id/kids",
            get(kids::get_policy).put(kids::set_policy),
        )
        .route(
            "/profiles/:id/progress/series",
            get(library::series_history),
        )
        .route("/profiles/:id/progress/page", get(library::history_page))
        .route(
            "/profiles/:id/progress/correct",
            axum::routing::put(library::correct),
        )
        .merge(
            Router::new()
                .route("/playback-protocol", get(gateway::playback::support))
                .route(
                    "/torrent-runtime-protocol",
                    get(gateway::playback::runtime_support),
                )
                .route("/playback", post(gateway::playback::start))
                .route(
                    "/playback-decoder-start",
                    post(gateway::playback::decoder_start),
                )
                .route(
                    "/playback/:id/subtitles",
                    post(gateway::playback::attach_subtitle),
                )
                .route(
                    "/playback/:id/first-frame",
                    post(gateway::playback::first_frame),
                )
                .route(
                    "/playback/:id",
                    get(gateway::playback::get).delete(gateway::playback::stop),
                )
                .route("/playback/:id/heartbeat", post(gateway::playback::renew))
                .route(
                    "/playback/:id/progress",
                    get(gateway::playback::progress),
                )
                .route(
                    "/playback-requests/:request_id",
                    axum::routing::delete(gateway::playback::cancel_request),
                ),
        )
        .route(
            "/gateways",
            get(gateway::http::list).post(gateway::http::register),
        )
        .route(
            "/gateways/:id",
            axum::routing::patch(gateway::http::update)
                .put(gateway::http::replace)
                .delete(gateway::http::delete),
        )
        .route("/gateways/:id/check", post(gateway::http::check))
        .route(
            "/gateways/:id/grants",
            get(gateway::http::grants).put(gateway::http::grant),
        )
        .route(
            "/iptv/matches",
            get(provider::v2_http::matches).put(provider::v2_http::override_match),
        )
        .route(
            "/iptv/live-default",
            get(provider::v2_http::live_default).put(provider::v2_http::set_live_default),
        )
        .route(
            "/iptv/live/channels",
            get(provider::v2_http::live_channels),
        )
        .route(
            "/iptv/live/categories",
            get(provider::v2_http::live_categories),
        )
        .fallback(|| async { ApiError(StatusCode::NOT_FOUND, "API route not found".into()) })
        .route_layer(middleware::from_fn_with_state(
            app.clone(),
            authorize_resources,
        ))
        .merge(auth::router())
        .route_layer(middleware::from_fn_with_state(
            app.clone(),
            auth::authenticate,
        ))
        .layer(middleware::from_fn(json_errors))
        .layer(middleware::map_response(private_api_response))
        .layer(middleware::from_fn(gateway::playback::control_deadline))
        .layer(middleware::from_fn(gateway::diagnostics::observe))
        .layer(middleware::from_fn(images::negotiate));
    let mut r = Router::new()
        .route(
            "/api/images/simkl/:folder/*asset",
            get(images::get).options(images::options),
        )
        .nest("/api", api)
        .route(
            "/api/simkl/callback",
            get(simkl::callback).layer(middleware::map_response(private_api_response)),
        )
        .route(
            "/api/health",
            get(|| async {
                axum::Json(json!({"status":"ok","version":env!("CARGO_PKG_VERSION")}))
            }),
        )
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
        .with_state(app);
    let app_links = android_links::configured();
    r = r.route(
        "/.well-known/assetlinks.json",
        get(move || {
            let body = app_links.clone();
            async move { android_links::response(body) }
        }),
    );
    r = r
        .route("/privacy", get(privacy_page))
        .route("/terms", get(terms_page));
    if let Some(path) = tv_dashboard {
        let root_index = path.join("index.html");
        let web: Router = Router::new()
            .fallback_service(
                tower_http::services::ServeDir::new(&path)
                    .fallback(tower_http::services::ServeFile::new(path.join("index.html"))),
            )
            .layer(middleware::from_fn(web_asset_cache));
        r = r
            .route("/", get(move || dashboard_entry(root_index.clone())))
            .fallback_service(web);
    } else {
        r = r.route(
            "/",
            get(|| async {
                (
                    StatusCode::OK,
                    [(header::CACHE_CONTROL, "no-store")],
                    "VIPTV server",
                )
            }),
        );
    }
    r
}
fn html_page(body: impl IntoResponse) -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response()
}
async fn privacy_page() -> Response {
    html_page(
        r#"<!DOCTYPE html><html lang="en"><head><meta charset="utf-8"><title>VIPTV Privacy Policy</title><meta name="viewport" content="width=device-width,initial-scale=1"><style>body{font-family:system-ui,sans-serif;max-width:640px;margin:40px auto;padding:0 20px;line-height:1.6;color:#e0e0e0;background:#101112}h1{color:#fff}a{color:#90a0ff}</style></head><body><h1>Privacy Policy</h1><p>VIPTV is a private streaming application. We do not collect, store, or share personal information beyond what is strictly necessary to provide the service.</p><p>Account credentials are stored securely and are never shared with third parties. Playback sessions are ephemeral and not logged.</p><p>For questions, contact vynxcai@gmail.com.</p><p><a href="/">Back to VIPTV</a></p></body></html>"#,
    )
}

async fn terms_page() -> Response {
    html_page(
        r#"<!DOCTYPE html><html lang="en"><head><meta charset="utf-8"><title>VIPTV Terms of Use</title><meta name="viewport" content="width=device-width,initial-scale=1"><style>body{font-family:system-ui,sans-serif;max-width:640px;margin:40px auto;padding:0 20px;line-height:1.6;color:#e0e0e0;background:#101112}h1{color:#fff}a{color:#90a0ff}</style></head><body><h1>Terms of Use</h1><p>VIPTV is provided as-is for personal, non-commercial use. Users are responsible for their own content and streaming sources.</p><p>The service is for authorized users only. Unauthorized access or redistribution is prohibited.</p><p>We reserve the right to modify or discontinue the service at any time.</p><p><a href="/">Back to VIPTV</a></p></body></html>"#,
    )
}

async fn dashboard_entry(path: PathBuf) -> Response {
    const MAX_INDEX_BYTES: usize = 2 * 1024 * 1024;
    match tokio::fs::read(path).await {
        Ok(bytes) if bytes.len() <= MAX_INDEX_BYTES => html_page(bytes),
        Ok(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CACHE_CONTROL, "no-store")],
            "Dashboard entry exceeds 2 MiB",
        )
            .into_response(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (
            StatusCode::NOT_FOUND,
            [(header::CACHE_CONTROL, "no-store")],
            "Dashboard entry not found",
        )
            .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CACHE_CONTROL, "no-store")],
            "Dashboard entry unavailable",
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tv_web_mount_tests {
    use super::*;
    use tower::ServiceExt;
    async fn web_request(app:&Router,path:&str)->Response {
        app.clone().oneshot(axum::http::Request::builder().uri(path).body(axum::body::Body::empty()).unwrap()).await.unwrap()
    }
    #[tokio::test]
    async fn optional_tv_mount_preserves_root_dashboard_api_and_deep_links() {
        let root=tempfile::tempdir().unwrap();let tv=tempfile::tempdir().unwrap();let dashboard=tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("index.html"),"KMP root").unwrap();
        let app=router_with_viewers(crate::auth_integration_tests::fixture(),Some(root.path().into()));
        for (path,expected) in [("/","KMP root"),("/device","KMP root")] {
            let response=app.clone().oneshot(axum::http::Request::builder().uri(path).body(axum::body::Body::empty()).unwrap()).await.unwrap();
            assert_eq!(response.status(),StatusCode::OK,"{path}");
            let bytes=axum::body::to_bytes(response.into_body(),1024).await.unwrap();assert_eq!(&bytes[..],expected.as_bytes(),"{path}");
        }
        let response=app.oneshot(axum::http::Request::builder().uri("/api/health").body(axum::body::Body::empty()).unwrap()).await.unwrap();assert_eq!(response.status(),StatusCode::OK);
    }
    #[tokio::test]
    async fn tv_mount_answers_client_routes_with_the_shell_and_assets_with_themselves() {
        const SHELL:&str="<!doctype html><div id=\"viptv-ui-layer\"></div>";
        let web=tempfile::tempdir().unwrap();
        std::fs::create_dir(web.path().join("composeResources")).unwrap();
        std::fs::write(web.path().join("index.html"),SHELL).unwrap();
        std::fs::write(web.path().join("composeApp.js"),"startApp()").unwrap();
        std::fs::write(web.path().join("819d242a46ca39e513ca.wasm"),"wasm").unwrap();
        std::fs::write(web.path().join("composeResources/avatar.png"),"png").unwrap();
        let app=router_with_viewers(crate::auth_integration_tests::fixture(),Some(web.path().into()));
        for path in ["/","/device","/device?code=12345678","/tv/show/123","/tv/show/123?season=2","/device/"] {
            let response=web_request(&app,path).await;
            assert_eq!(response.status(),StatusCode::OK,"{path}");
            assert!(response.headers()[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/html"),"{path}");
            let bytes=axum::body::to_bytes(response.into_body(),4096).await.unwrap();
            assert_eq!(&bytes[..],SHELL.as_bytes(),"{path}");
        }
        for (path,content_type,body) in [
            ("/composeApp.js","text/javascript","startApp()"),
            ("/819d242a46ca39e513ca.wasm","application/wasm","wasm"),
            ("/composeResources/avatar.png","image/png","png"),
        ] {
            let response=web_request(&app,path).await;
            assert_eq!(response.status(),StatusCode::OK,"{path}");
            assert_eq!(response.headers()[header::CONTENT_TYPE],content_type,"{path}");
            let bytes=axum::body::to_bytes(response.into_body(),4096).await.unwrap();
            assert_eq!(&bytes[..],body.as_bytes(),"{path}");
        }
    }
    #[tokio::test]
    async fn tv_mount_revalidates_shell_files_and_immutably_caches_hashed_assets() {
        let web=tempfile::tempdir().unwrap();
        for name in ["index.html","composeApp.js","composeApp.js.map","819d242a46ca39e513ca.wasm","819d242a46ca39e513c.js"] {
            std::fs::write(web.path().join(name),name).unwrap();
        }
        let app=router_with_viewers(crate::auth_integration_tests::fixture(),Some(web.path().into()));
        for (path,cache) in [
            ("/","no-store"),
            ("/composeApp.js","no-cache"),
            ("/composeApp.js.map","no-cache"),
            ("/device","no-cache"),
            ("/819d242a46ca39e513c.js","no-cache"),
            ("/819d242a46ca39e513ca.wasm","public, max-age=31536000, immutable"),
        ] {
            let response=app.clone().oneshot(axum::http::Request::builder().uri(path).body(axum::body::Body::empty()).unwrap()).await.unwrap();
            assert_eq!(response.status(),StatusCode::OK,"{path}");
            assert_eq!(response.headers()[header::CACHE_CONTROL],cache,"{path}");
        }
    }
}
