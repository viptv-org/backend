//! Shared credential-safe JSON egress for configured sources.
use serde_json::Value;
use std::{net::IpAddr, time::Duration};
use url::{Host, Url};
pub(crate) fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_multicast()
                && !ip.is_broadcast()
                && !ip.is_unspecified()
                && a != 0
                && a < 224
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 192 && b == 0 && (c == 0 || c == 2))
                && !(a == 192 && b == 88 && c == 99)
                && !(a == 198 && (b == 18 || b == 19))
                && !(a == 198 && b == 51 && c == 100)
                && !(a == 203 && b == 0 && c == 113)
        }
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return public_ip(v4.into());
            }
            let words = ip.segments();
            (words[0] & 0xe000) == 0x2000
                && words[0] != 0x2002
                && !(words[0] == 0x2001 && (words[1] < 0x200 || words[1] == 0xdb8))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    InvalidEndpoint,
    Private,
    Dns,
    Unavailable,
    Timeout,
    Redirect,
    AccessDenied,
    RateLimited,
    TooLarge,
    Protocol,
    Interrupted,
}
impl Failure {
    pub(crate) fn provider_code(self) -> &'static str {
        match self {
            Self::InvalidEndpoint => "invalid_provider_endpoint",
            Self::Private => "provider_private_destination",
            Self::Dns => "provider_dns_unavailable",
            Self::Unavailable => "provider_unavailable",
            Self::Timeout => "provider_timeout",
            Self::Redirect => "provider_redirect_rejected",
            Self::AccessDenied => "provider_credentials_rejected",
            Self::RateLimited => "provider_rate_limited",
            Self::TooLarge => "provider_response_too_large",
            Self::Protocol => "provider_protocol_invalid",
            Self::Interrupted => "provider_response_interrupted",
        }
    }
    pub(crate) fn addon_code(self) -> &'static str {
        match self {
            Self::InvalidEndpoint => "invalid_addon_endpoint",
            Self::Private => "addon_private_destination",
            Self::Dns => "addon_dns_unavailable",
            Self::Unavailable => "addon_unavailable",
            Self::Timeout => "addon_timeout",
            Self::Redirect => "addon_redirect_rejected",
            Self::AccessDenied => "addon_access_denied",
            Self::RateLimited => "addon_rate_limited",
            Self::TooLarge => "addon_response_too_large",
            Self::Protocol => "addon_protocol_invalid",
            Self::Interrupted => "addon_response_interrupted",
        }
    }
}
pub(crate) fn validate(url: &Url, fixture: bool) -> Result<(), Failure> {
    if url.as_str().len() > 16384
        || !matches!(url.scheme(), "http" | "https")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.port_or_known_default() == Some(0)
    {
        return Err(Failure::InvalidEndpoint);
    }
    let host = url
        .host_str()
        .unwrap_or_default()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
    {
        return Err(Failure::Private);
    }
    let allowed = |ip: IpAddr| public_ip(ip) || (fixture && ip.is_loopback());
    if matches!(url.host(),Some(Host::Ipv4(ip)) if !allowed(ip.into()))
        || matches!(url.host(),Some(Host::Ipv6(ip)) if !allowed(ip.into()))
    {
        return Err(Failure::Private);
    }
    Ok(())
}
fn request_error(error: &reqwest::Error, body: bool) -> Failure {
    if error.is_timeout() {
        Failure::Timeout
    } else if body {
        Failure::Interrupted
    } else {
        Failure::Unavailable
    }
}
fn redirect_target(target: &Url, location: &str, fixture: bool) -> Result<Url, Failure> {
    if location.len() > 16384 || location.chars().any(char::is_control) {
        return Err(Failure::Redirect);
    }
    let next = target.join(location).map_err(|_| Failure::Redirect)?;
    if target.scheme() == "https" && next.scheme() != "https" {
        return Err(Failure::Redirect);
    }
    validate(&next, fixture)?;
    Ok(next)
}
/// Addons may follow at most ten public redirects. Each hop is resolved and
/// pinned independently, without Referer, cookies, copied credentials or proxies.
/// Provider callers retain their canonical-endpoint/no-redirect contract.
pub(crate) async fn json(
    mut target: Url,
    limit: usize,
    timeout: Duration,
    fixture: bool,
    redirects: bool,
) -> Result<Value, Failure> {
    tokio::time::timeout(timeout + Duration::from_secs(5), async {
        for hop in 0..=10 {
            validate(&target, fixture)?;
            let host = match target.host().ok_or(Failure::InvalidEndpoint)? {
                Host::Domain(v) => v.to_owned(),
                Host::Ipv4(v) => v.to_string(),
                Host::Ipv6(v) => v.to_string(),
            };
            let port = target
                .port_or_known_default()
                .ok_or(Failure::InvalidEndpoint)?;
            let addresses = tokio::time::timeout(
                Duration::from_secs(3),
                tokio::net::lookup_host((host.as_str(), port)),
            )
            .await
            .map_err(|_| Failure::Dns)?
            .map_err(|_| Failure::Dns)?
            .take(17)
            .collect::<Vec<_>>();
            if addresses.is_empty()
                || addresses.len() > 16
                || addresses
                    .iter()
                    .any(|a| !public_ip(a.ip()) && !(fixture && a.ip().is_loopback()))
            {
                return Err(Failure::Private);
            }
            let client = reqwest::Client::builder()
                .no_proxy()
                .user_agent("VIPTV/0.1")
                .redirect(reqwest::redirect::Policy::none())
                .resolve_to_addrs(&host, &addresses)
                .connect_timeout(Duration::from_secs(3))
                .timeout(timeout)
                .build()
                .map_err(|e| request_error(&e, false))?;
            let mut response = client
                .get(target.clone())
                .send()
                .await
                .map_err(|e| request_error(&e, false))?;
            match response.status().as_u16() {
                200..=299 => {}
                301 | 302 | 303 | 307 | 308 => {
                    if !redirects || hop == 10 {
                        return Err(Failure::Redirect);
                    }
                    let location = response
                        .headers()
                        .get(reqwest::header::LOCATION)
                        .and_then(|v| v.to_str().ok())
                        .filter(|v| v.len() <= 16384 && !v.chars().any(char::is_control))
                        .ok_or(Failure::Redirect)?;
                    target = redirect_target(&target, location, fixture)?;
                    continue;
                }
                300..=399 => return Err(Failure::Redirect),
                401 | 403 => return Err(Failure::AccessDenied),
                429 => return Err(Failure::RateLimited),
                _ => return Err(Failure::Unavailable),
            }
            if response.content_length().is_some_and(|n| n > limit as u64) {
                return Err(Failure::TooLarge);
            }
            let mut bytes = zeroize::Zeroizing::new(Vec::new());
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|e| request_error(&e, true))?
            {
                if bytes.len().saturating_add(chunk.len()) > limit {
                    return Err(Failure::TooLarge);
                }
                bytes.extend_from_slice(&chunk);
            }
            return tokio::task::spawn_blocking(move || {
                serde_json::from_slice(&bytes).map_err(|_| Failure::Protocol)
            })
            .await
            .map_err(|_| Failure::Unavailable)?;
        }
        Err(Failure::Redirect)
    })
    .await
    .map_err(|_| Failure::Timeout)?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn redirect_targets_reject_downgrades_private_addresses_and_userinfo() {
        let source =
            Url::parse("https://public.example/private-token/manifest.json?key=private-token")
                .unwrap();
        assert_eq!(
            redirect_target(&source, "http://public.example/manifest.json", false).unwrap_err(),
            Failure::Redirect
        );
        assert_eq!(
            redirect_target(&source, "https://169.254.169.254/manifest.json", false).unwrap_err(),
            Failure::Private
        );
        assert_eq!(
            redirect_target(
                &source,
                "https://user:secret@public.example/manifest.json",
                false
            )
            .unwrap_err(),
            Failure::InvalidEndpoint
        );
        let next = redirect_target(&source, "https://cdn.example/manifest.json", false).unwrap();
        assert!(next.query().is_none());
        assert!(!next.as_str().contains("private-token"));
    }
    #[tokio::test]
    async fn cross_origin_redirect_does_not_copy_query_credentials_or_cookies() {
        use axum::{
            http::{HeaderMap, StatusCode, Uri},
            routing::get,
            Json, Router,
        };
        let destination = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let final_url = format!("http://{}/final", destination.local_addr().unwrap());
        let receiving = tokio::spawn(async move {
            axum::serve(
                destination,
                Router::new().route(
                    "/final",
                    get(|headers: HeaderMap, uri: Uri| async move {
                        assert!(headers.get("referer").is_none());
                        assert!(headers.get("authorization").is_none());
                        assert!(headers.get("cookie").is_none());
                        assert!(uri.query().is_none());
                        Json(serde_json::json!({"ok":true}))
                    }),
                ),
            )
            .await
            .unwrap();
        });
        let source = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!(
            "http://{}/private-token?api_key=private-token",
            source.local_addr().unwrap()
        ))
        .unwrap();
        let redirecting = tokio::spawn(async move {
            axum::serve(
                source,
                Router::new().route(
                    "/private-token",
                    get(move || {
                        let url = final_url.clone();
                        async move {
                            (
                                StatusCode::FOUND,
                                [
                                    ("location", url),
                                    ("set-cookie", "credential=private-token".to_owned()),
                                ],
                                "",
                            )
                        }
                    }),
                ),
            )
            .await
            .unwrap();
        });
        let value = json(url, 1024, Duration::from_secs(3), true, true)
            .await
            .unwrap();
        assert_eq!(value["ok"], true);
        redirecting.abort();
        receiving.abort();
    }
}
