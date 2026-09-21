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
    automation::start(&app);
    health::start(&app);
    guides::start(&app);
    let api = Router::new()
        .route(
            "/profiles",
            get(profiles_authenticated).post(create_profile_authenticated),
        )
        .route(
            "/profiles/:id",
            axum::routing::patch(update_profile_authenticated).delete(delete_profile_authenticated),
        )
        .route("/playback/startups/:id", delete(session::cancel_startup))
        .route("/playback/:id/recover", post(session::recover_live))
        .route(
            "/automation/catalog",
            get(automation::list).patch(automation::configure),
        )
        .route("/guides", get(guides::list).patch(guides::configure))
        .route("/guides/sources", post(guides::add_source))
        .route(
            "/guides/sources/:id",
            axum::routing::patch(guides::enable_source),
        )
        .route(
            "/guides/channels/:id",
            axum::routing::patch(guides::map_channel),
        )
        .route("/guides/channels/:id/repair", post(guides::repair_channel))
        .route("/guides/run", post(guides::run_now))
        .route("/guides/cancel", post(guides::cancel))
        .route("/activity", get(activity::list))
        .route("/activity/pause", post(activity::pause))
        .route("/activity/undo/:id", post(activity::undo))
        .route("/stream-health", get(health::list).patch(health::configure))
        .route(
            "/stream-health/:id",
            axum::routing::patch(health::override_candidate),
        )
        .route("/stream-health/:id/check", post(health::check))
        .route("/automation/catalog/run", post(automation::run))
        .route("/automation/catalog/cancel", post(automation::cancel))
        .route("/lineup", get(lineup::list).post(lineup::create))
        .route(
            "/lineup/matching",
            get(lineup::matching::list).patch(lineup::matching::configure),
        )
        .route("/lineup/matching/run", post(lineup::matching::run))
        .route(
            "/lineup/matching/groups/:id",
            axum::routing::patch(lineup::matching::group),
        )
        .route(
            "/lineup/:id/matching",
            axum::routing::patch(lineup::matching::correct),
        )
        .route("/lineup/settings", axum::routing::patch(lineup::configure))
        .route("/lineup/candidates", get(lineup::inventory))
        .route("/lineup/:id", axum::routing::patch(lineup::update))
        .route("/providers", get(providers).post(add_provider))
        .route("/providers/import", post(provider::accounts::import))
        .route("/account-pools", get(provider::pools::list))
        .route("/providers/:id/status", post(provider::pools::refresh))
        .route(
            "/account-pools/:id",
            axum::routing::patch(provider::pools::configure),
        )
        .route(
            "/providers/:id/pool",
            axum::routing::patch(provider::pools::assign),
        )
        .route(
            "/providers/:id/credentials",
            post(provider::accounts::renew),
        )
        .route(
            "/providers/:id",
            delete(delete_provider).patch(update_provider),
        )
        .route("/providers/:id/sync", post(sync_provider))
        .route(
            "/live-policy",
            get(live_policy::list).put(live_policy::update),
        )
        .route("/addons", get(addons).post(add_addon))
        .route("/addons/:id", delete(delete_addon).patch(update_addon))
        .route("/catalogs", get(catalogs))
        .route("/discover", get(discover))
        .route("/meta/:kind/:id", get(meta))
        .route("/streams", post(start_streams_authenticated))
        .route("/streams/:id", get(poll_streams_authenticated))
        .route("/streams/:id/events", get(stream_events_authenticated))
        .route("/live", get(live))
        .route("/live/categories", get(live_categories))
        .route("/guide/:id", get(guide))
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
        .route("/matches", get(matches).put(override_match))
        .route("/playback", post(start_playback_authenticated))
        .route("/playback/:id", delete(stop_playback))
        .route("/playback/:id/heartbeat", post(heartbeat))
        .route("/status", get(status))
        .route("/service-health", get(service_health::list))
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
        .layer(middleware::from_fn(json_errors));
    // Read-only CORS for session media only. An instrument or debugger running on
    // another origin (for example an external player or inspection tool) needs to
    // fetch a session's bytes directly; the session capability is the credential,
    // so this grants no access that the capability did not already grant.
    //
    // Deliberately scoped to this one route rather than the whole router: the
    // API's origin policy exists to stop cross-site account requests, and a
    // wildcard response header there would undermine that. No credentials are
    // allowed and the methods are read-only, so a cookie-authenticated mutation
    // can never succeed from a foreign origin.
    let media_route = match media_cors() {
        Some(cors) => get(media).layer(cors),
        None => get(media),
    };
    let mut r = Router::new()
        .nest("/api", api)
        .route(
            "/api/health",
            get(|| async {
                axum::Json(json!({"status":"ok","version":env!("CARGO_PKG_VERSION")}))
            }),
        )
        .route("/media/:id/:cap/:file", media_route)
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
fn media_cors() -> Option<CorsLayer> {
    let configured = std::env::var("VIPTV_MEDIA_CORS_ORIGINS")
        .unwrap_or_else(|_| "https://mediabunny.dev".into());
    let mut origins = Vec::new();
    for value in configured
        .split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        // Validate as a real HTTPS origin, matching how the request Origin header
        // is parsed. A HeaderValue only rejects control characters, so without
        // this a typo like "mediabunny.dev" would silently enter the allow list
        // as an origin no browser ever sends.
        let valid = crate::util::validate_url(value)
            .ok()
            .filter(|u| {
                u.scheme() == "https"
                    && u.path() == "/"
                    && u.query().is_none()
                    && u.host_str().is_some()
            })
            .map(|u| u.origin().ascii_serialization());
        match valid.and_then(|origin| origin.parse::<axum::http::HeaderValue>().ok()) {
            Some(origin) => origins.push(origin),
            // A malformed entry must not silently become a wildcard, and must not
            // be ignored either: ignoring it would leave an operator believing a
            // tool is allowed when it is not.
            None => {
                tracing::warn!("Ignoring malformed VIPTV_MEDIA_CORS_ORIGINS entry");
                return None;
            }
        }
    }
    if origins.is_empty() {
        return None;
    }
    Some(
        CorsLayer::new()
            // Read-only: a session capability is a bearer secret, so no cookie
            // may accompany a cross-origin media request.
            .allow_credentials(false)
            .allow_methods([axum::http::Method::GET, axum::http::Method::HEAD])
            // Range is required for byte-range media reads; the browser may also
            // preflight these.
            .allow_headers([
                header::RANGE,
                header::IF_RANGE,
                header::ACCEPT,
                header::ACCEPT_ENCODING,
            ])
            .expose_headers([
                header::CONTENT_LENGTH,
                header::CONTENT_RANGE,
                header::ACCEPT_RANGES,
                header::ETAG,
            ])
            .allow_origin(origins),
    )
}

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

#[cfg(test)]
mod media_cors_tests {
    use super::media_cors;

    /// The layer must exist for the default origin, must refuse a malformed
    /// entry rather than degrade to a wildcard, and must disappear when the
    /// operator opts out.
    ///
    /// This guards the rule that CORS never reaches `/api`: the layer is applied
    /// to the `/media` route alone, and a `CorsLayer` that cannot be constructed
    /// is `None` rather than permissive.
    #[test]
    fn media_cors_is_explicit_and_never_a_wildcard() {
        // The default is the Mediabunny instrument, not `*`.
        std::env::remove_var("VIPTV_MEDIA_CORS_ORIGINS");
        assert!(
            media_cors().is_some(),
            "default origin must produce a layer"
        );

        // An empty setting disables cross-origin media reads entirely.
        std::env::set_var("VIPTV_MEDIA_CORS_ORIGINS", "");
        assert!(media_cors().is_none(), "empty must disable, not widen");

        // Whitespace and multiple entries are tolerated.
        std::env::set_var(
            "VIPTV_MEDIA_CORS_ORIGINS",
            " https://mediabunny.dev , https://other.example ",
        );
        assert!(media_cors().is_some());

        // Entries that are not a bare HTTPS origin are refused, never widened:
        // a bare host, a non-HTTPS scheme and a value carrying a path.
        for bad in [
            "not a valid origin",
            "mediabunny.dev",
            "http://mediabunny.dev",
            "https://mediabunny.dev/path",
            "https://mediabunny.dev/?q=1",
            "*",
        ] {
            std::env::set_var("VIPTV_MEDIA_CORS_ORIGINS", bad);
            assert!(
                media_cors().is_none(),
                "{bad} must disable the layer, never widen it"
            );
        }

        std::env::remove_var("VIPTV_MEDIA_CORS_ORIGINS");
    }
}
