use super::*;

/// Backward-compatible application router with the optional account dashboard at `/`.
pub fn router(app: App, dashboard: Option<std::path::PathBuf>) -> Router {
    router_with_tv(app, dashboard, None)
}

/// Mount the account dashboard at `/` and the TV React bundle at `/tv` without
/// changing API/media origins or enabling cross-origin credentials.
pub fn router_with_tv(
    app: App,
    dashboard: Option<std::path::PathBuf>,
    tv_dashboard: Option<std::path::PathBuf>,
) -> Router {
    let api = Router::new()
        .route(
            "/profiles",
            get(profiles_authenticated).post(create_profile_authenticated),
        )
        .route(
            "/profiles/:id",
            axum::routing::patch(update_profile_authenticated).delete(delete_profile_authenticated),
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
        .route("/addons", get(addons).post(add_addon))
        .route("/addons/:id", delete(delete_addon).patch(update_addon))
        .route("/catalogs", get(catalogs))
        .route("/catalogs/revision", get(catalogs_revision))
        .route("/discover", get(discover))
        .route("/meta/:kind/:id", get(meta))
        .route("/v2/streams", post(provider::discovery_v2::start))
        .route(
            "/v2/addons",
            get(addon::http_v2::list).post(addon::http_v2::create),
        )
        .route(
            "/v2/addons/:id",
            axum::routing::patch(addon::http_v2::update).delete(addon::http_v2::delete),
        )
        .route(
            "/v2/iptv/connections",
            get(provider::connections_v2::list).post(provider::connections_v2::create),
        )
        .route(
            "/v2/iptv/connections/:id",
            axum::routing::patch(provider::connections_v2::update)
                .delete(provider::connections_v2::delete),
        )
        .route(
            "/v2/iptv/connections/:id/credentials",
            axum::routing::put(provider::connections_v2::renew),
        )
        .route(
            "/v2/iptv/connections/:id/refresh",
            get(provider::refresh_v2::get)
                .post(provider::refresh_v2::request)
                .delete(provider::refresh_v2::cancel),
        )
        .route("/v2/streams/:id", get(provider::discovery_v2::poll))
        .route("/v2/iptv/guide/:id", get(provider::discovery_v2::guide))
        .route(
            "/v2/iptv/live/:id/source",
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
            "/profiles/:id/progress",
            get(progress_authenticated).put(save_progress_authenticated),
        )
        .route(
            "/profiles/:id/preferences",
            get(preferences::get).put(preferences::put),
        )
        .route(
            "/profiles/:id/approvals",
            get(kids::approvals).post(kids::approve),
        )
        .route("/parent/search", get(kids::search))
        .route("/parent/status", get(kids::status))
        .route("/parent/pin", axum::routing::put(kids::set_pin))
        .route("/parent/unlock", post(kids::unlock))
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
        .route("/profiles/:id/continue/page", get(continuation::page))
        .route("/profiles/:id/continue/next", post(continuation::next))
        .route(
            "/profiles/:id/continue/visibility",
            axum::routing::put(continuation::visibility),
        )
        .route(
            "/profiles/:id/continue/settings",
            get(continuation::settings).put(continuation::save_settings),
        )
        .route(
            "/profiles/:id/continue",
            get(continue_watching_authenticated),
        )
        .route("/v2/playback", post(gateway::playback::start))
        .route(
            "/v2/playback/:id",
            get(gateway::playback::get).delete(gateway::playback::stop),
        )
        .route("/v2/playback/:id/heartbeat", post(gateway::playback::renew))
        .route(
            "/v2/gateways",
            get(gateway::http::list).post(gateway::http::register),
        )
        .route(
            "/v2/gateways/:id",
            axum::routing::patch(gateway::http::update)
                .put(gateway::http::replace)
                .delete(gateway::http::delete),
        )
        .route("/v2/gateways/:id/check", post(gateway::http::check))
        .route(
            "/v2/gateways/:id/grants",
            get(gateway::http::grants).put(gateway::http::grant),
        )
        .route(
            "/v2/iptv/matches",
            get(provider::v2_http::matches).put(provider::v2_http::override_match),
        )
        .route(
            "/v2/iptv/live-default",
            get(provider::v2_http::live_default).put(provider::v2_http::set_live_default),
        )
        .route(
            "/v2/iptv/live/channels",
            get(provider::v2_http::live_channels),
        )
        .route(
            "/v2/iptv/live/categories",
            get(provider::v2_http::live_categories),
        )
        .merge(retired::router())
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
        .layer(middleware::map_response(private_api_response));
    let mut r = Router::new()
        .nest("/api", api)
        .route(
            "/api/health",
            get(|| async {
                axum::Json(json!({"status":"ok","version":env!("CARGO_PKG_VERSION")}))
            }),
        )
        .route("/media/*path", axum::routing::any(retired::reject))
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
        .with_state(app);
    r = r
        .route("/privacy", get(privacy_page))
        .route("/terms", get(terms_page));
    if let Some(path) = dashboard {
        let root_index = path.join("index.html");
        let device_index = root_index.clone();
        r = r
            .route("/", get(move || dashboard_entry(root_index.clone())))
            .route(
                "/device",
                get(move || dashboard_entry(device_index.clone())),
            )
            .fallback_service(
                tower_http::services::ServeDir::new(&path).not_found_service(
                    tower_http::services::ServeFile::new(path.join("index.html")),
                ),
            );
    } else {
        // A server-only image still answers / with its identity so health
        // checks pass without a mounted dashboard.
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
    if let Some(path) = tv_dashboard {
        let tv_entry = path.join("index.html");
        r = r
            .route("/tv", get(move || dashboard_entry(tv_entry.clone())))
            .nest_service(
                "/tv/",
                tower_http::services::ServeDir::new(&path).fallback(
                    tower_http::services::ServeFile::new(path.join("index.html")),
                ),
            );
    }
    r
}
/// Origins allowed to read session media cross-origin, from
/// `VIPTV_MEDIA_CORS_ORIGINS` (comma-separated, exact `scheme://host[:port]`).
///
/// Defaults to the Mediabunny instrument, which reads a session's bytes directly
/// to inspect and debug them. Set the variable to an empty string to disable
/// cross-origin media reads entirely (the native client is same-origin and does
/// not need them).
// Static HTML responses share one non-cacheable envelope.
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
