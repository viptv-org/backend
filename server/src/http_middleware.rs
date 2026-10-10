use super::*;

pub(crate) async fn private_api_response(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}

pub(crate) async fn json_errors(req: Request, next: Next) -> Response {
    let v2 = req.uri().path().starts_with("/v2/") || req.uri().path().starts_with("/api/v2/");
    let response = next.run(req).await;
    if response.status().is_client_error()
        && !response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|s| s.starts_with("application/json"))
    {
        let status = response.status();
        if v2 {
            // Framework rejections can contain a submitted value or parser
            // diagnostic. Preserve their HTTP status, never their body.
            let mut response = account_api::Error::Code("invalid_v2_request").into_response();
            *response.status_mut() = status;
            return response;
        }
        return ApiError(
            status,
            status
                .canonical_reason()
                .unwrap_or("Invalid request")
                .into(),
        )
        .into_response();
    }
    response
}
fn capture_lease(
    app: &App,
    token: Option<&str>,
    principal: auth::Principal,
) -> Result<ResourceLease, ApiError> {
    // Authentication already succeeded. Resolve its credential once to a stable lease;
    // downstream media requests carry only the playback capability, not this credential.
    let token = token.ok_or_else(auth::unauthorized)?;
    let db = app.db.lock().unwrap();
    let session_id = db.query_row("SELECT id FROM auth_sessions WHERE access_hash=?1 AND account_id=?2 AND id IS ?3 AND access_expires>?4", params![format!("{:x}",Sha256::digest(token.as_bytes())), principal.account_id(), principal.session_id(), util::now()], |r| r.get::<_,String>(0)).optional().map_err(db_error)?;
    let policy_revision = kids::revision(&db, &principal)?;
    let lease = ResourceLease {
        policy_revision,
        principal,
        session_id,
    };
    lease.validate(&db)?;
    Ok(lease)
}
pub(crate) async fn authorize_resources(
    State(mut app): State<App>,
    mut req: Request,
    next: Next,
) -> Response {
    let Some(principal) = req.extensions().get::<auth::Principal>().cloned() else {
        return auth::unauthorized().into_response();
    };
    let credential = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::to_owned)
        .or_else(|| {
            req.headers()
                .get(header::COOKIE)
                .and_then(|value| value.to_str().ok())
                .and_then(|cookies| {
                    cookies
                        .split(';')
                        .filter_map(|value| value.trim().split_once('='))
                        .find(|(name, _)| *name == "viptv_session")
                        .map(|(_, value)| value.to_owned())
                })
        });
    let worker_app = app.clone();
    let worker_principal = principal.clone();
    let lease =
        match blocking(move || capture_lease(&worker_app, credential.as_deref(), worker_principal))
            .await
        {
            Ok(lease) => lease,
            Err(error) => return error.into_response(),
        };
    req.extensions_mut().insert(lease.clone());
    app = app.with_lease(lease);
    let worker_app = app.clone();
    let worker_principal = principal.clone();
    let path = req
        .uri()
        .path()
        .strip_prefix("/api")
        .unwrap_or(req.uri().path())
        .to_owned();
    if retired::is_path(&path) {
        return retired::reject().await.into_response();
    }
    let result = blocking(move || {
        let segments: Vec<_> = path.trim_matches('/').split('/').collect();
        let media_route = matches!(
            segments.as_slice(),
            ["catalogs", ..]
                | ["discover", ..]
                | ["meta", ..]
                | ["streams", ..]
                | ["live", ..]
                | ["guide", ..]
                | ["playback", ..]
        );
        if media_route
            && matches!(
                &worker_principal,
                auth::Principal::Account {
                    profile_id: None,
                    ..
                }
            )
        {
            return Err(ApiError(StatusCode::FORBIDDEN, MSG_PROFILE_REQUIRED.into()));
        }
        match segments.as_slice() {
            ["providers", ..]
            | ["matches", ..]
            | ["lineup", ..]
            | ["account-pools", ..]
            | ["automation", ..]
            | ["health", ..]
            | ["service-health", ..]
            | ["activity", ..]
            | ["guides", ..]
            | ["live-policy", ..]
                if !worker_principal.is_owner() =>
            {
                return Err(ApiError(
                    StatusCode::FORBIDDEN,
                    "Owner access required".into(),
                ));
            }
            ["profiles", id, "favorites", ..]
            | ["profiles", id, "progress", ..]
            | ["profiles", id, "continue", ..]
            | ["profiles", id, "preferences", ..]
            | ["profiles", id, "integrations", ..] => {
                let profile = id
                    .parse::<i64>()
                    .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "Invalid profile id".into()))?;
                let db = worker_app.db.lock().map_err(|_| {
                    ApiError(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Database unavailable".into(),
                    )
                })?;
                worker_app.require_profile(&db, profile)?;
            }
            ["streams", id, ..] => worker_app.check_resource(&worker_principal, "job", id)?,
            ["playback", "startups", ..] => {}
            ["playback", id, ..] => worker_app.check_resource(&worker_principal, "playback", id)?,
            _ => {}
        }
        Ok(())
    })
    .await;
    if let Err(error) = result {
        return error.into_response();
    }
    let policy_path = req
        .uri()
        .path()
        .strip_prefix("/api")
        .unwrap_or(req.uri().path())
        .to_owned();
    match kids::before(&app, &mut req).await {
        Ok(Some(value)) => return axum::Json(value).into_response(),
        Ok(None) => (),
        Err(error) => return error.into_response(),
    }
    let response = next.run(req).await;
    kids::after(&app, &policy_path, response).await
}

#[cfg(test)]
mod v2_error_tests {
    use crate::{auth_integration_tests::fixture, router, test_support::request};
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use serde_json::{json, Value};
    use tower::ServiceExt;

    #[tokio::test]
    async fn v2_framework_rejections_have_safe_codes_without_changing_legacy_or_auth() {
        let app = fixture();
        for (method, path) in [
            ("DELETE", "/api/v2/addons/private-sentinel"),
            ("DELETE", "/api/v2/iptv/connections/private-sentinel"),
            ("GET", "/api/v2/iptv/connections/private-sentinel/refresh"),
            (
                "PUT",
                "/api/v2/iptv/connections/private-sentinel/credentials",
            ),
            ("GET", "/api/v2/streams/%FF"),
            ("GET", "/api/v2/playback/%FF"),
            ("GET", "/api/v2/iptv/guide/%FF"),
            ("POST", "/api/v2/gateways/%FF/check"),
        ] {
            let (status, body) = request(&app, "member-token-1", method, path, json!({})).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {body}");
            assert_eq!(body["error_code"], "invalid_v2_request", "{path}: {body}");
            assert!(!body.to_string().contains("private-sentinel"));
        }
        let (status, error) = request(
            &app,
            "invalid-token",
            "DELETE",
            "/api/v2/addons/private-sentinel",
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{error}");
        assert_ne!(error["error_code"], "invalid_v2_request");
        let (_, legacy) = request(
            &app,
            "member-token-1",
            "GET",
            "/api/meta/%FF/item",
            Value::Null,
        )
        .await;
        assert_ne!(legacy["error_code"], "invalid_v2_request");
        app.db
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO kids_profiles(profile_id,enabled) VALUES(1,1)",
                [],
            )
            .unwrap();
        let (status, error) = request(
            &app,
            "member-token-1",
            "GET",
            "/api/v2/iptv/live/channels",
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(error["error_code"], "parent_required");
    }

    #[tokio::test]
    async fn v2_query_and_json_rejections_keep_route_specific_safe_codes() {
        let app = fixture();
        for (path, expected) in [
            (
                "/api/v2/addons?limit=private-sentinel",
                "invalid_catalog_query",
            ),
            (
                "/api/v2/iptv/connections?limit=private-sentinel",
                "invalid_catalog_query",
            ),
            (
                "/api/v2/iptv/live/channels?limit=private-sentinel",
                "invalid_catalog_query",
            ),
            (
                "/api/v2/iptv/matches?limit=private-sentinel",
                "invalid_matches_query",
            ),
            (
                "/api/v2/streams/job?after=private-sentinel",
                "invalid_discovery_cursor",
            ),
        ] {
            let (status, body) = request(&app, "member-token-1", "GET", path, Value::Null).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(body["error_code"], expected);
            assert!(!body.to_string().contains("private-sentinel"));
        }
        for (path, expected) in [
            ("/api/v2/addons", "invalid_addon_endpoint"),
            ("/api/v2/iptv/connections", "invalid_provider_configuration"),
            ("/api/v2/gateways", "invalid_gateway_configuration"),
            ("/api/v2/playback", "invalid_playback_request"),
            ("/api/v2/streams", "invalid_discovery_request"),
        ] {
            let response = router(app.clone(), None)
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(path)
                        .header("authorization", "Bearer member-token-1")
                        .header("content-type", "application/json")
                        .body(Body::from("{private-sentinel"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let bytes = to_bytes(response.into_body(), 4096).await.unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["error_code"], expected);
            assert!(!body.to_string().contains("private-sentinel"));
        }
    }
}
