use serde_json::{Value, json};

/// Display facts only. Never promote arbitrary response content into UI diagnostics.
pub fn api_error(v: &Value) -> Value {
    let status = v["status"].as_u64().unwrap_or(0);
    let raw = v["error"].as_str().unwrap_or("").trim();
    let supplied = v["error_code"]
        .as_str()
        .filter(|code| {
            code.len() <= 64
                && code
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
        .unwrap_or("");
    let code = match (supplied, raw) {
        (
            "",
            "Provider connection limit reached"
            | "All available connections are busy. Try this channel again shortly.",
        ) => "provider_connection_limit",
        ("", "Playback capacity reached") => "playback_capacity",
        ("", "Stream expired; discover again") => "source_expired",
        _ => supplied,
    };
    let known = match code {
        "playback_expired" | "authorization_expired" => {
            Some("This playback session has expired. Start playback again to reconnect.")
        }
        "playback_failed" => {
            Some("The selected source could not start. Try another source or retry playback.")
        }
        "gateway_required" => Some(
            "This device or source requires a playback gateway. Configure one in account settings or ask the server operator.",
        ),
        "gateway_capacity" => {
            Some("Playback capacity is currently full. Stop another stream or try again shortly.")
        }
        "gateway_cleanup_pending" => {
            Some("The previous stream is still being closed. Try again shortly.")
        }
        "gateway_startup_timeout" => Some(
            "The gateway took too long to prepare this source. Try again or choose another source.",
        ),
        "gateway_processing_failed" => Some(
            "The gateway could not prepare this stream. Choose another source or check the gateway.",
        ),
        "gateway_key_rejected" => {
            Some("The gateway rejected this integration key. Check that it is active.")
        }
        "gateway_scope_missing" => {
            Some("The gateway key must allow this namespace and all required playback operations.")
        }
        "gateway_protocol_invalid" => {
            Some("The gateway returned an incompatible response. Check its service version.")
        }
        "gateway_redirect_rejected" => Some(
            "Use the gateway's final HTTPS endpoint; control requests cannot follow redirects.",
        ),
        "gateway_not_ready" | "gateway_dns_unavailable" | "gateway_unavailable" => Some(
            "The gateway is not ready or could not be reached securely. Try again or check its address.",
        ),
        "gateway_storage_unavailable" => {
            Some("Gateway settings are temporarily unavailable. Try again.")
        }
        "invalid_playback_request" => {
            Some("Check the source, playback position and device capabilities.")
        }
        "playback_conflict" => Some(
            "This playback request ID was already used for a different request. Start a new request.",
        ),
        "provider_rate_limited" => {
            Some("The IPTV provider is limiting API requests. Wait before trying again.")
        }
        "provider_credentials_rejected" => Some(
            "The IPTV provider rejected access. Check your credentials, subscription status or provider access restrictions.",
        ),
        "provider_timeout" | "provider_refresh_timeout" => {
            Some("The IPTV provider took too long to respond. Try again later.")
        }
        "provider_protocol_invalid" | "provider_response_too_large" => {
            Some("The IPTV provider returned an invalid or oversized response. Try another source.")
        }
        "provider_discovery_failed"
        | "provider_unavailable"
        | "provider_dns_unavailable"
        | "provider_response_interrupted" => Some(
            "This IPTV provider could not return sources. Try again or choose another provider.",
        ),
        "source_not_found" | "playback_not_found" => Some(
            "This source or playback session is unavailable in your account. Refresh the sources.",
        ),
        "source_configuration_changed" => {
            Some("The source configuration changed. Refresh the sources and try again.")
        }
        "source_format_unsupported" => {
            Some("This source format is not supported. Choose another source.")
        }
        "source_headers_unsupported" => Some(
            "This source requires unsupported or invalid request headers. Choose another source.",
        ),
        "source_credentials_migration_required" => Some(
            "This source needs an ownership and encryption migration before it can be used. Ask the server operator to migrate it and configure the encryption keyring.",
        ),
        "source_route_migration_required" => Some(
            "This source still uses a retired routing configuration. Ask the server operator to update it.",
        ),
        "invalid_episode_selection" => {
            Some("Choose a specific season and episode before requesting IPTV sources.")
        }
        "addon_timeout" => {
            Some("The addon took too long to respond. Try again or choose another addon.")
        }
        "addon_access_denied" => {
            Some("The addon rejected access. Check its configuration or subscription.")
        }
        "addon_rate_limited" => Some("The addon is limiting requests. Wait before trying again."),
        "addon_protocol_invalid" | "addon_response_too_large" => {
            Some("The addon returned an invalid or oversized response. Try another addon.")
        }
        "addon_unavailable" | "addon_dns_unavailable" | "addon_response_interrupted" => {
            Some("The addon could not return sources. Try again or choose another addon.")
        }
        "addon_private_destination" | "provider_private_destination" => {
            Some("This source uses a private network address that the server does not allow.")
        }
        "addon_redirect_rejected" | "provider_redirect_rejected" => Some(
            "The source returned a redirect that the server could not safely follow. Check its address.",
        ),
        "secret_store_not_configured"
        | "secret_key_unavailable"
        | "secret_authentication_failed"
        | "invalid_secret_envelope" => Some(
            "The server could not unlock saved source credentials. Ask the server operator to check its encryption keys.",
        ),
        "catalog_changed" => Some(
            "This playlist changed while you were browsing. Reload it to see the current channels.",
        ),
        "client_update_required" => {
            Some("Update this app to use the server's current playback and catalog API.")
        }
        "provider_connection_limit" => Some(
            "This IPTV provider has reached its connection limit. Stop another stream or choose another provider.",
        ),
        "playback_capacity" => Some(
            "The server has reached its playback limit. Stop another stream or try again later.",
        ),
        "source_expired" => {
            Some("This stream has expired. Refresh the sources and choose it again.")
        }
        "source_access_denied" => Some(
            "The provider rejected access to this stream. Check the provider account or choose another source.",
        ),
        "source_unavailable" => Some(
            "The source could not be reached or inspected. Try again or choose another source.",
        ),
        "delivery_unsupported" => Some(
            "This source cannot be played with the current playback configuration. Choose another source.",
        ),
        "parent_required" => Some("Parent PIN required"),
        "parent_pin_invalid" => Some("Incorrect parent PIN"),
        "profile_required" => Some("Profile selection required"),
        "profile_policy_changed" => Some("Profile policy changed"),
        _ => None,
    };
    let lower = raw.to_ascii_lowercase();
    let protocol = matches!(
        raw,
        "authorization_pending" | "slow_down" | "expired_token" | "access_denied" | "invalid_grant"
    );
    let safe = protocol
        || !raw.is_empty()
            && raw.chars().count() <= 240
            && !raw.chars().any(char::is_control)
            && ![
                "://",
                "bearer ",
                "authorization",
                "cookie",
                "password",
                "token=",
                "token:",
                "secret=",
                "<",
                ">",
                "traceback",
                "stack trace",
            ]
            .iter()
            .any(|part| lower.contains(part));
    let fallback = match status {
        0 => "Could not reach the server. Check your connection and try again.",
        401 => "Your session has expired. Sign in or pair this device again.",
        403 => "VIPTV refused this request. Check your profile permissions.",
        404 | 410 => "This item or stream is no longer available. Refresh and try again.",
        429 => "Too many requests. Wait a moment and try again.",
        502..=504 => {
            "The server or provider is temporarily unavailable. Try again or choose another source."
        }
        _ => "VIPTV could not complete this request. Try again.",
    };
    json!({"message": known.unwrap_or(if safe {raw} else {fallback}), "code": code})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capacity_is_not_rate_limiting_and_legacy_servers_work() {
        let legacy = api_error(&json!({"status":429,"error":"Provider connection limit reached"}));
        let coded = api_error(&json!({"status":429,"error_code":"provider_connection_limit"}));
        assert_eq!(legacy, coded);
        assert!(
            coded["message"]
                .as_str()
                .unwrap()
                .contains("Stop another stream")
        );
        assert!(
            api_error(&json!({"status":429}))["message"]
                .as_str()
                .unwrap()
                .contains("Too many requests")
        );
    }
    #[test]
    fn unknown_errors_are_useful_without_leaking_transport_details() {
        assert_eq!(
            api_error(&json!({"status":400,"error":"authorization_pending"}))["message"],
            "authorization_pending"
        );
        assert_eq!(
            api_error(&json!({"status":400,"error":"No enabled providers"}))["message"],
            "No enabled providers"
        );
        for secret in [
            "https://provider.test/password",
            "Authorization: Bearer secret",
            "cookie=secret",
            "token=secret",
            "<html>gateway</html>",
            "failure\nprivate details",
        ] {
            let result = api_error(&json!({"status":502,"error":secret}));
            assert!(!result["message"].as_str().unwrap().contains(secret));
        }
    }
}
