//! Android App Links: `/.well-known/assetlinks.json` lets Android open the VIPTV app, rather than
//! the browser, for this server's links (the sign-in QR code, for one). The server only vouches for
//! the app it is told about, so the file is served only when an operator configures it.

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// `VIPTV_ANDROID_PACKAGE`: the application id, e.g. `org.viptv.app`.
const PACKAGE_VARIABLE: &str = "VIPTV_ANDROID_PACKAGE";
/// `VIPTV_ANDROID_CERT_SHA256`: comma-separated SHA-256 fingerprints of the app's signing certificates.
const FINGERPRINT_VARIABLE: &str = "VIPTV_ANDROID_CERT_SHA256";

fn valid_package(value: &str) -> bool {
    let mut segments = value.split('.');
    let first_ok = segments.next().is_some_and(valid_segment);
    first_ok && value.contains('.') && segments.all(valid_segment)
}

fn valid_segment(segment: &str) -> bool {
    segment.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && segment.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Android requires upper-case hex pairs separated by colons (32 bytes).
fn normalized_fingerprint(value: &str) -> Option<String> {
    let pairs: Vec<_> = value.trim().split(':').collect();
    let valid = pairs.len() == 32 && pairs.iter().all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit()));
    valid.then(|| value.trim().to_ascii_uppercase())
}

/// The statement list, or `None` when no valid package and fingerprint are configured.
pub(crate) fn statements(package: Option<&str>, fingerprints: Option<&str>) -> Option<String> {
    let package = package.map(str::trim).filter(|p| valid_package(p))?;
    let fingerprints: Vec<String> = fingerprints?.split(',').filter_map(normalized_fingerprint).collect();
    if fingerprints.is_empty() {
        return None;
    }
    Some(
        json!([{
            "relation": ["delegate_permission/common.handle_all_urls"],
            "target": {"namespace": "android_app", "package_name": package, "sha256_cert_fingerprints": fingerprints}
        }])
        .to_string(),
    )
}

pub(crate) fn configured() -> Option<String> {
    statements(
        std::env::var(PACKAGE_VARIABLE).ok().as_deref(),
        std::env::var(FINGERPRINT_VARIABLE).ok().as_deref(),
    )
}

pub(crate) fn response(body: Option<String>) -> Response {
    match body {
        Some(body) => (
            [
                (header::CONTENT_TYPE, "application/json"),
                // Short enough that fixing a wrong fingerprint takes effect quickly.
                (header::CACHE_CONTROL, "public, max-age=300"),
            ],
            body,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FINGERPRINT: &str = "14:6D:E9:83:C5:73:06:50:D8:EE:B9:95:2F:34:FC:64:16:A0:83:42:E6:1D:BE:A8:8A:04:96:B2:3F:CF:44:E5";

    #[test]
    fn serves_the_statement_for_the_configured_app() {
        let body = statements(Some("org.viptv.experiment"), Some(FINGERPRINT)).unwrap();
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value[0]["relation"][0], "delegate_permission/common.handle_all_urls");
        assert_eq!(value[0]["target"]["namespace"], "android_app");
        assert_eq!(value[0]["target"]["package_name"], "org.viptv.experiment");
        assert_eq!(value[0]["target"]["sha256_cert_fingerprints"][0], FINGERPRINT);
    }

    #[test]
    fn lists_every_valid_fingerprint_and_normalizes_case() {
        let lower = FINGERPRINT.to_ascii_lowercase();
        let other = FINGERPRINT.replace("14:6D", "AA:BB");
        let body = statements(Some("org.viptv.app"), Some(&format!("{lower}, {other}, not-a-fingerprint"))).unwrap();
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value[0]["target"]["sha256_cert_fingerprints"], json!([FINGERPRINT, other]));
    }

    #[test]
    fn serves_nothing_unless_both_values_are_valid() {
        assert!(statements(None, Some(FINGERPRINT)).is_none());
        assert!(statements(Some("org.viptv.app"), None).is_none());
        assert!(statements(Some("org.viptv.app"), Some("")).is_none());
        assert!(statements(Some("org.viptv.app"), Some("14:6D:E9")).is_none());
        for package in ["", "viptv", "org..app", "org.viptv.app\"}]", "1org.app", "org.viptv app"] {
            assert!(statements(Some(package), Some(FINGERPRINT)).is_none(), "{package}");
        }
    }
}
