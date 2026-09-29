//! Shared authorization boundary for account-owned settings, not viewing media.
use crate::{
    app_state::{App, ResourceLease},
    auth, kids, ApiError,
};
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

pub(crate) enum Error {
    Auth(ApiError),
    Code(&'static str),
}
impl From<&'static str> for Error {
    fn from(code: &'static str) -> Self {
        Self::Code(code)
    }
}
impl From<ApiError> for Error {
    fn from(error: ApiError) -> Self {
        Self::Auth(error)
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let code = match self {
            Self::Auth(error) => return error.into_response(),
            Self::Code(code) => code,
        };
        let (status, message) = details(code);
        (status, Json(json!({"error":message,"error_code":code}))).into_response()
    }
}
pub(crate) async fn run<T: Send + 'static>(
    app: App,
    lease: ResourceLease,
    action: impl FnOnce(&rusqlite::Connection, i64) -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    tokio::task::spawn_blocking(move || {
        let db = app
            .db
            .lock()
            .map_err(|_| Error::Code("provider_storage_unavailable"))?;
        lease.validate(&db)?;
        if matches!(&lease.principal, auth::Principal::Account { role, .. } if role == "device") {
            return Err(Error::Code("account_session_required"));
        }
        kids::require_parent(&db, &lease.principal)?;
        action(
            &db,
            lease
                .principal
                .account_id()
                .ok_or_else(auth::unauthorized)?,
        )
    })
    .await
    .map_err(|_| Error::Code("provider_storage_unavailable"))?
}

pub(crate) async fn work(
    app: App,
    lease: ResourceLease,
    action: impl FnOnce(&rusqlite::Connection, i64) -> Result<Value, Error> + Send + 'static,
) -> Result<Json<Value>, Error> {
    run(app, lease, action).await.map(Json)
}

pub(crate) fn description(code: &str) -> &'static str {
    details(code).1
}
fn details(code: &str) -> (StatusCode, &'static str) {
    match code {
        "invalid_provider_configuration" => (StatusCode::BAD_REQUEST,"Check the connection name, credentials and enabled content types."),
        "invalid_provider_endpoint" | "provider_private_destination" => (StatusCode::BAD_REQUEST,"Use a public HTTP or HTTPS Xtream server URL without embedded credentials or query parameters."),
        "provider_not_found" => (StatusCode::NOT_FOUND,"This IPTV connection is unavailable in your account."),
        "provider_encryption_required" => (StatusCode::CONFLICT,"This legacy connection needs the operator's reviewed encryption migration before it can be managed here."),
        "provider_already_configured" => (StatusCode::CONFLICT,"This IPTV login is already configured in your account."),
        "too_many_providers" => (StatusCode::CONFLICT,"This account has reached its limit of 64 IPTV connections."),
        "provider_checks_busy" => (StatusCode::SERVICE_UNAVAILABLE,"IPTV checks are busy. Try again shortly."),
        "provider_credentials_rejected" => (StatusCode::UNPROCESSABLE_ENTITY,"The IPTV provider rejected these credentials or the subscription is inactive or expired."),
        "provider_redirect_rejected" => (StatusCode::UNPROCESSABLE_ENTITY,"The IPTV provider redirected this request. Use its final server address; credentials are not forwarded to redirects."),
        "provider_rate_limited" => (StatusCode::TOO_MANY_REQUESTS,"The IPTV provider is limiting API requests. Wait before trying again."),
        "provider_response_too_large" | "provider_protocol_invalid" => (StatusCode::BAD_GATEWAY,"The IPTV provider returned an oversized or invalid response."),
        "provider_timeout" => (StatusCode::GATEWAY_TIMEOUT,"The IPTV provider took too long to respond. Try again later."),
        "provider_dns_unavailable" | "provider_unavailable" => (StatusCode::BAD_GATEWAY,"The IPTV provider could not be reached. Check its address or try again later."),
        "gateway_processing_failed" => (StatusCode::BAD_GATEWAY,"The gateway could not prepare this stream. Choose another source or check the gateway."),
            "gateway_required" => (StatusCode::CONFLICT, "This device or source requires a playback gateway. Configure one in account settings or ask the server operator."),
            "invalid_playback_request" => (StatusCode::BAD_REQUEST, "Check the source, playback position and device capabilities."),
            "playback_not_found" | "source_not_found" => (StatusCode::NOT_FOUND, "This source or playback session is unavailable in your account. Refresh the sources."),
            "playback_conflict" => (StatusCode::CONFLICT, "This playback request ID was already used for a different request. Start a new request."),
            "playback_expired" | "authorization_expired" => (StatusCode::GONE, "Playback authorization expired. Reconnect and choose the source again."),
            "source_configuration_changed" => (StatusCode::CONFLICT, "The source configuration changed. Refresh the sources and try again."),
            "source_route_migration_required" => (StatusCode::CONFLICT, "This source still uses a retired routing configuration. Update its connection before playing."),
            "provider_connection_limit" => (StatusCode::TOO_MANY_REQUESTS, "This IPTV provider has reached its connection limit. Stop another stream or choose another provider."),
            "gateway_capacity" | "playback_capacity" => (StatusCode::SERVICE_UNAVAILABLE, "Playback capacity is currently full. Stop another stream or try again shortly."),
            "gateway_cleanup_pending" => (StatusCode::CONFLICT, "The previous stream is still being closed. Try again shortly."),
            "gateway_startup_timeout" => (StatusCode::GATEWAY_TIMEOUT, "The gateway took too long to prepare this source. Try again or choose another source."),
            "source_unavailable" => (StatusCode::BAD_GATEWAY, "The source could not be reached or inspected. Try another source."),
            "delivery_unsupported" => (StatusCode::NOT_ACCEPTABLE, "This source cannot be delivered with this device's playback capabilities."),
            "gateway_already_configured" => (StatusCode::CONFLICT, "This gateway and namespace are already configured in your account. Edit the existing connection."),
            "gateway_checks_busy" => (StatusCode::SERVICE_UNAVAILABLE, "Gateway checks are busy. Try again shortly."),
            "invalid_gateway_configuration" => (StatusCode::BAD_REQUEST, "Check the gateway name, namespace and priority."),
            "invalid_gateway_key" => (StatusCode::BAD_REQUEST, "Use a scoped gateway integration key starting with pgk_, not the bootstrap API_KEY."),
            "invalid_gateway_endpoint" | "gateway_private_destination" => (StatusCode::BAD_REQUEST, "Use a public HTTPS gateway endpoint. IPTV source URLs may still use HTTP."),
            "gateway_not_found" => (StatusCode::NOT_FOUND, "This gateway is unavailable in your account."),
            "gateway_account_unavailable" => (StatusCode::NOT_FOUND, "The account selected for this grant is unavailable."),
            "too_many_gateways" => (StatusCode::CONFLICT, "This account has reached its gateway configuration limit."),
            "gateway_key_rejected" => (StatusCode::UNPROCESSABLE_ENTITY, "The gateway rejected this integration key. Check that it is active."),
            "gateway_scope_missing" => (StatusCode::UNPROCESSABLE_ENTITY, "The gateway key must allow this namespace and all required playback operations."),
            "gateway_protocol_invalid" => (StatusCode::UNPROCESSABLE_ENTITY, "The gateway returned an incompatible response. Check its service version."),
            "gateway_redirect_rejected" => (StatusCode::UNPROCESSABLE_ENTITY, "Use the gateway's final HTTPS endpoint; control requests cannot follow redirects."),
            "gateway_not_ready" | "gateway_dns_unavailable" | "gateway_unavailable" => (StatusCode::BAD_GATEWAY, "The gateway is not ready or could not be reached securely. Try again or check its address."),
            "secret_store_not_configured" => (StatusCode::SERVICE_UNAVAILABLE, "The backend needs an encryption keyring before it can save provider or gateway credentials. Ask the server operator."),
            "secret_key_unavailable" | "secret_authentication_failed" | "invalid_secret_envelope" => (StatusCode::SERVICE_UNAVAILABLE, "Saved credentials could not be unlocked. Ask the server operator to restore the correct encryption keys."),
            "gateway_storage_unavailable" => (StatusCode::SERVICE_UNAVAILABLE, "Gateway settings are temporarily unavailable. Try again."),
            "invalid_cursor" => (
                StatusCode::BAD_REQUEST,
                "This page token is no longer valid. Reload the list.",
            ),
            "catalog_changed" => (StatusCode::CONFLICT, "This playlist changed while you were browsing. Reload it to see the current channels."),
            "invalid_catalog_query" | "invalid_matches_query" => (
                StatusCode::BAD_REQUEST,
                "Check the IPTV filters and choose a page size from 1 to 200.",
            ),
            "invalid_match_request" => (
                StatusCode::BAD_REQUEST,
                "Choose a valid movie or series match.",
            ),
            "catalog_unavailable" => (
                StatusCode::NOT_FOUND,
                "This live playlist is unavailable in your account.",
            ),
            "stream_candidate_not_found" => (
                StatusCode::NOT_FOUND,
                "This stream is unavailable in your IPTV accounts.",
            ),
            "account_session_required" => (
                StatusCode::FORBIDDEN,
                "Manage account settings from an account session, not a paired TV.",
            ),
            _ => (
                StatusCode::SERVICE_UNAVAILABLE,
                "IPTV data is temporarily unavailable. Try again.",
            ),
        }
}
