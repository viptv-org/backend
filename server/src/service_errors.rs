//! Closed public classifications. Never return an upstream diagnostic as copy.
pub(crate) fn provider(raw: &str) -> Option<&'static str> {
    Some(match raw {
        "Series streams require season and episode" => "invalid_episode_selection",
        "invalid_provider_endpoint" => "invalid_provider_endpoint",
        "provider_private_destination" => "provider_private_destination",
        "provider_dns_unavailable" => "provider_dns_unavailable",
        "provider_redirect_rejected" => "provider_redirect_rejected",
        "provider_credentials_rejected" => "provider_credentials_rejected",
        "provider_rate_limited" => "provider_rate_limited",
        "provider_response_too_large" | "Provider response is too large" => {
            "provider_response_too_large"
        }
        "provider_response_interrupted" | "Provider response could not be read" => {
            "provider_response_interrupted"
        }
        "provider_protocol_invalid"
        | "Provider returned invalid JSON"
        | "Provider returned invalid episode or guide data"
        | "Provider index contains invalid entries" => "provider_protocol_invalid",
        "provider_timeout" | "IPTV candidate timed out" => "provider_timeout",
        "provider_unavailable" | "Provider request failed or timed out" => "provider_unavailable",
        "provider_checks_busy" => "provider_checks_busy",
        "provider_connection_limit" | "Provider connection limit reached" => {
            "provider_connection_limit"
        }
        "source_configuration_changed"
        | "Provider credentials changed or account became unavailable; retry the request" => {
            "source_configuration_changed"
        }
        "source_route_migration_required" | "Provider WARP route unavailable" => {
            "source_route_migration_required"
        }
        "source_headers_unsupported" => "source_headers_unsupported",
        "secret_store_not_configured" => "secret_store_not_configured",
        "secret_key_unavailable" => "secret_key_unavailable",
        "secret_authentication_failed" => "secret_authentication_failed",
        "invalid_secret_envelope" => "invalid_secret_envelope",
        "source_not_found"
        | "Live channel not found"
        | "Provider not found or disabled"
        | "Provider not found or content scope disabled"
        | "Provider unavailable in this account"
        | "This IPTV source is no longer available in your account." => "source_not_found",
        "provider_storage_unavailable"
        | "Provider database is unavailable"
        | "Provider database worker stopped"
        | "Provider database operation failed" => "provider_storage_unavailable",
        "provider_refresh_cancelled" => "provider_refresh_cancelled",
        "provider_refresh_timeout" => "provider_refresh_timeout",
        _ => return None,
    })
}
pub(crate) fn addon(raw: &str) -> Option<&'static str> {
    Some(match raw {
        "invalid_addon_endpoint" | "Manifest URL must end in /manifest.json" => {
            "invalid_addon_endpoint"
        }
        "Invalid addon manifest" | "invalid_addon_configuration" => "invalid_addon_configuration",
        "addon_checks_busy" => "addon_checks_busy",
        "addon_configuration_changed" => "addon_configuration_changed",
        "addon_private_destination" => "addon_private_destination",
        "addon_dns_unavailable" => "addon_dns_unavailable",
        "addon_redirect_rejected" => "addon_redirect_rejected",
        "addon_response_interrupted" => "addon_response_interrupted",
        "addon_timeout" | "Addon timed out" | "Upstream timed out" => "addon_timeout",
        "addon_access_denied" | "Upstream returned HTTP 401" | "Upstream returned HTTP 403" => {
            "addon_access_denied"
        }
        "addon_rate_limited" | "Upstream returned HTTP 429" => "addon_rate_limited",
        "addon_protocol_invalid"
        | "Upstream returned invalid JSON"
        | "Invalid addon stream response" => "addon_protocol_invalid",
        "addon_response_too_large" | "Upstream response exceeds size limit" => {
            "addon_response_too_large"
        }
        "addon_unavailable" => "addon_unavailable",
        _ => return None,
    })
}
pub(crate) fn discovery(source: &str, raw: &str) -> &'static str {
    if raw == "source_not_found" {
        return "source_not_found";
    }
    if source == "addon" || source.starts_with("addon:") {
        addon(raw).unwrap_or("addon_unavailable")
    } else {
        provider(raw).unwrap_or("provider_discovery_failed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_diagnostics_are_never_public_and_rate_limits_are_not_connection_limits() {
        let raw="HTTP 403 https://provider.invalid/private-user/private-password token=private-token max_connections";
        assert_eq!(provider(raw), None);
        assert_eq!(discovery("iptv:1", raw), "provider_discovery_failed");
        assert_eq!(discovery("addon:1", raw), "addon_unavailable");
        assert_eq!(
            discovery("iptv:1", "provider_rate_limited"),
            "provider_rate_limited"
        );
        assert_eq!(
            discovery("addon:1", "Upstream returned HTTP 429"),
            "addon_rate_limited"
        );
        assert_eq!(
            provider("Provider connection limit reached"),
            Some("provider_connection_limit")
        );
    }
}
