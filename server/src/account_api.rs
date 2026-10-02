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
    authorized(app, lease, false, action).await
}
async fn authorized<T: Send + 'static>(
    app: App,
    lease: ResourceLease,
    device_read: bool,
    action: impl FnOnce(&rusqlite::Connection, i64) -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    tokio::task::spawn_blocking(move || {
        let db = app
            .db
            .lock()
            .map_err(|_| Error::Code("provider_storage_unavailable"))?;
        lease.validate(&db)?;
        if matches!(&lease.principal, auth::Principal::Account { role, .. } if role == "device") {
            if !device_read {
                return Err(Error::Code("account_session_required"));
            }
            if matches!(
                &lease.principal,
                auth::Principal::Account {
                    profile_id: None,
                    ..
                }
            ) {
                return Err(Error::Auth(ApiError(
                    StatusCode::FORBIDDEN,
                    crate::app_state::MSG_PROFILE_REQUIRED.into(),
                )));
            }
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

/// Read-only settings metadata for viewing devices with a selected profile.
/// Mutations must use work/run; parent restrictions still apply to this view.
pub(crate) async fn view(
    app: App,
    lease: ResourceLease,
    action: impl FnOnce(&rusqlite::Connection, i64) -> Result<Value, Error> + Send + 'static,
) -> Result<Json<Value>, Error> {
    authorized(app, lease, true, action).await.map(Json)
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
        "stremio_invalid_request" => (StatusCode::BAD_REQUEST,"Check the import credentials, options and confirmation."),
        "stremio_confirmation_required" => (StatusCode::BAD_REQUEST,"Review the preview and explicitly confirm before importing."),
        "stremio_credentials_invalid" => (StatusCode::UNPROCESSABLE_ENTITY,"Stremio rejected these credentials. Check them and try again."),
        "stremio_source_unavailable" => (StatusCode::BAD_GATEWAY,"Stremio returned an unavailable, invalid or oversized response. Try again later."),
        "stremio_addon_unavailable" => (StatusCode::BAD_GATEWAY,"A selected addon could not be safely verified or saved. Review a fresh preview."),
        "stremio_restricted_profile" => (StatusCode::FORBIDDEN,"Import is not available for restricted profiles. Choose your personal profile."),
        "stremio_preview_not_found" => (StatusCode::NOT_FOUND,"This preview is unavailable in this session. Create another preview."),
        "stremio_preview_expired" => (StatusCode::GONE,"This preview expired. Create another preview before importing."),
        "stremio_preview_stale" => (StatusCode::CONFLICT,"Your profile changed after this preview. Review a fresh preview before importing."),
        "stremio_import_busy" => (StatusCode::TOO_MANY_REQUESTS,"Too many imports are pending. Cancel a preview or try again shortly."),
        "stremio_backup_failed" => (StatusCode::SERVICE_UNAVAILABLE,"The safety backup could not be completed. Nothing was imported. Contact the server operator."),
        "stremio_storage_unavailable" => (StatusCode::SERVICE_UNAVAILABLE,"Import storage is temporarily unavailable. Try again."),
        "invalid_v2_request" => (StatusCode::BAD_REQUEST,"This request is invalid. Check its path, query and body, then try again."),
        "secret_too_large" => (StatusCode::PAYLOAD_TOO_LARGE,"This source configuration exceeds the server's storage limit. Use a smaller configuration."),
        "invalid_addon_endpoint" => (StatusCode::BAD_REQUEST,"Use a valid HTTP or HTTPS addon manifest URL without embedded user credentials or fragments."),
        "addon_private_destination" => (StatusCode::BAD_REQUEST,"The addon uses a private or reserved network address, which the server's source policy does not allow."),
        "invalid_addon_configuration" => (StatusCode::BAD_REQUEST,"Use a valid addon manifest and supported settings."),
        "addon_not_found" => (StatusCode::NOT_FOUND,"This addon is unavailable in your account."),
        "addon_configuration_changed" => (StatusCode::CONFLICT,"This addon changed while its manifest was loading. Refresh settings and try again."),
        "addon_checks_busy" => (StatusCode::SERVICE_UNAVAILABLE,"Addon checks are busy. Try again shortly."),
        "addon_redirect_rejected" => (StatusCode::BAD_GATEWAY,"The addon returned an unsafe or excessive redirect. Check its final manifest address."),
        "addon_dns_unavailable" => (StatusCode::BAD_GATEWAY,"The addon address could not be resolved. Check its address or try again later."),
        "addon_response_interrupted" => (StatusCode::BAD_GATEWAY,"The addon's response was interrupted. Try again later."),
        "addon_encryption_required" => (StatusCode::CONFLICT,"This legacy addon needs the operator's reviewed encryption migration before it can be updated."),
        "source_credentials_migration_required" => (StatusCode::CONFLICT,"This source needs the operator's reviewed ownership and encryption migration before it can be used. Ask the server operator to migrate it and configure the encryption keyring."),
        "addon_storage_unavailable" => (StatusCode::SERVICE_UNAVAILABLE,"Addon settings are temporarily unavailable. Try again."),
        "invalid_episode_selection" => (StatusCode::BAD_REQUEST,"Choose a specific season and episode before requesting IPTV sources."),
        "invalid_discovery_request" => (StatusCode::BAD_REQUEST,"Choose a movie, exact episode or live channel and valid source filters."),
        "invalid_discovery_cursor" => (StatusCode::BAD_REQUEST,"Use a valid non-negative discovery event position or restart source discovery."),
        "discovery_capacity" => (StatusCode::TOO_MANY_REQUESTS,"Too many source searches are active. Wait briefly before trying again."),
        "discovery_not_found" => (StatusCode::NOT_FOUND,"This source search expired or is unavailable in this session. Search for sources again."),
        "provider_discovery_failed" => (StatusCode::BAD_GATEWAY,"This IPTV provider could not return sources. Try again or choose another provider."),
        "source_format_unsupported" => (StatusCode::NOT_ACCEPTABLE,"This source format is not supported. Choose another source."),
        "source_headers_unsupported" => (StatusCode::NOT_ACCEPTABLE,"This source requires unsupported or invalid request headers. Choose another source."),
        "addon_timeout" => (StatusCode::GATEWAY_TIMEOUT,"The addon took too long to respond. Try again or choose another addon."),
        "addon_access_denied" => (StatusCode::BAD_GATEWAY,"The addon rejected access. Check its configuration or subscription."),
        "addon_rate_limited" => (StatusCode::TOO_MANY_REQUESTS,"The addon is limiting requests. Wait before trying again."),
        "addon_protocol_invalid" | "addon_response_too_large" => (StatusCode::BAD_GATEWAY,"The addon returned an invalid or oversized response. Try another addon."),
        "addon_unavailable" => (StatusCode::BAD_GATEWAY,"The addon could not return sources. Try again or choose another addon."),
        "provider_response_interrupted" => (StatusCode::BAD_GATEWAY,"The IPTV provider's response was interrupted. Try again later."),
        "provider_refresh_failed" => (StatusCode::BAD_GATEWAY,"The provider catalog could not be refreshed. The previous catalog is still available. Try again later."),
        "provider_refresh_timeout" => (StatusCode::GATEWAY_TIMEOUT,"The provider catalog refresh timed out. The previous catalog was kept."),
        "provider_refresh_cancelled" | "provider_refresh_interrupted" => (StatusCode::CONFLICT,"The catalog refresh was interrupted or cancelled. Request another refresh when ready."),
        "invalid_provider_configuration" => (StatusCode::BAD_REQUEST,"Check the connection name, credentials and enabled content types."),
        "invalid_provider_endpoint" | "provider_private_destination" => (StatusCode::BAD_REQUEST,"Use a public HTTP or HTTPS Xtream server URL without embedded credentials or query parameters."),
        "provider_not_found" => (StatusCode::NOT_FOUND,"This IPTV connection is unavailable in your account."),
        "provider_encryption_required" => (StatusCode::CONFLICT,"This legacy connection needs the operator's reviewed encryption migration before it can be managed here."),
        "provider_already_configured" => (StatusCode::CONFLICT,"This IPTV login is already configured in your account."),
        "too_many_providers" => (StatusCode::CONFLICT,"This account has reached its limit of 64 IPTV connections."),
        "provider_checks_busy" => (StatusCode::SERVICE_UNAVAILABLE,"IPTV checks are busy. Try again shortly."),
        "provider_credentials_rejected" => (StatusCode::UNPROCESSABLE_ENTITY,"The IPTV provider rejected access. Check your credentials, subscription status or provider access restrictions."),
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
            "secret_store_not_configured" => (StatusCode::SERVICE_UNAVAILABLE, "The server's encryption keyring is not configured. Ask the server operator to configure or restore it."),
            "secret_key_unavailable" | "secret_authentication_failed" | "invalid_secret_envelope" => (StatusCode::SERVICE_UNAVAILABLE, "Saved credentials could not be unlocked. Ask the server operator to restore the correct encryption keys."),
            "gateway_storage_unavailable" => (StatusCode::SERVICE_UNAVAILABLE, "Gateway settings are temporarily unavailable. Try again."),
            "invalid_cursor" => (
                StatusCode::BAD_REQUEST,
                "This page token is no longer valid. Reload the list.",
            ),
            "catalog_changed" => (StatusCode::CONFLICT, "This catalog changed while you were browsing. Reload the list to see the current titles or channels."),
            "catalog_cursor_too_large" => (StatusCode::BAD_GATEWAY, "This playlist contains identifiers too large for paging. Ask the provider to use shorter identifiers or choose another playlist. Stored catalog data has not been changed."),
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
