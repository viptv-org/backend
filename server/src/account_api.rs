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
        let (status, message) = match code {
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
            "secret_store_not_configured" => (StatusCode::SERVICE_UNAVAILABLE, "The backend needs an encryption keyring before it can save gateway credentials. Ask the server operator."),
            "secret_key_unavailable" | "secret_authentication_failed" | "invalid_secret_envelope" => (StatusCode::SERVICE_UNAVAILABLE, "Saved credentials could not be unlocked. Ask the server operator to restore the correct encryption keys."),
            "gateway_storage_unavailable" => (StatusCode::SERVICE_UNAVAILABLE, "Gateway settings are temporarily unavailable. Try again."),
            "invalid_cursor" => (
                StatusCode::BAD_REQUEST,
                "This page token is no longer valid. Reload the list.",
            ),
            "invalid_matches_query" => (
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
        };
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
