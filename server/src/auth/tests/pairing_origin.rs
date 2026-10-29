use super::*;

const CANONICAL: &str = "https://viptv.example:8443";
const VIEWING: &str = "https://watch.example:8443";

fn served(host: &'static str, origin: Option<&'static str>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static(host));
    if let Some(origin) = origin {
        headers.insert(header::ORIGIN, HeaderValue::from_static(origin));
    }
    headers
}

#[test]
fn the_served_origin_pairs_while_browser_routes_stay_pinned() {
    let canonical = parse_origin(CANONICAL).unwrap();
    let configured = std::slice::from_ref(&canonical);
    assert!(check_origin(&served("viptv.example:8443", Some(CANONICAL)), configured).is_ok());
    assert!(check_device_origin(
        &served("viptv.example:8443", Some(CANONICAL)),
        configured
    )
    .is_ok());
    let viewing = served("watch.example:8443", Some(VIEWING));
    assert!(check_origin(&viewing, configured).is_err());
    assert!(check_device_origin(&viewing, configured).is_ok());
    assert!(check_device_origin(
        &viewing,
        &[canonical.clone(), parse_origin(VIEWING).unwrap()]
    )
    .is_ok());
    let native = served("tv.example", None);
    assert!(check_origin(&native, configured).is_ok());
    assert!(check_device_origin(&native, configured).is_ok());
    assert!(check_device_origin(&viewing, &[]).is_ok());
    let foreign = served("watch.example:8443", Some("https://unlisted.example"));
    assert!(check_origin(&foreign, configured).is_err());
    assert!(check_device_origin(&foreign, configured).is_err());
    assert!(check_device_origin(
        &served("unparsable host", Some(VIEWING)),
        configured
    )
    .is_err());
    let mut cross_site = viewing.clone();
    cross_site.insert("sec-fetch-site", HeaderValue::from_static("cross-site"));
    assert!(check_device_origin(&cross_site, configured).is_err());
    let mut insecure = viewing.clone();
    insecure.insert(
        header::ORIGIN,
        HeaderValue::from_static("http://watch.example:8443"),
    );
    assert!(check_device_origin(&insecure, configured).is_err());
}

#[test]
fn pairing_covers_the_device_credential_surface_only() {
    for path in [
        "/auth/device/code",
        "/auth/device/login",
        "/auth/device/token",
        "/auth/device/refresh",
        "/auth/device/revoke",
        "/auth/device/revoke-own",
        "/auth/profile",
        "/auth/logout",
    ] {
        assert!(device_path(path), "{path}");
    }
    for path in [
        "/auth/login",
        "/auth/register",
        "/auth/refresh",
        "/auth/recover",
        "/auth/device/lookup",
        "/auth/device/approve",
        "/auth/device/deny",
        "/auth/device/revoke-admin",
    ] {
        assert!(!device_path(path), "{path}");
    }
}
