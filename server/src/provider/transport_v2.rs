//! Credential-safe HTTP(S) Xtream transport: validate DNS and pin every request.
use super::*;
use crate::gateway::client::public_ip;
use url::Host;

fn request_error(error: &reqwest::Error, body: bool) -> &'static str {
    if error.is_timeout() {
        "provider_timeout"
    } else if body {
        "provider_response_interrupted"
    } else {
        "provider_unavailable"
    }
}

pub(super) fn base(raw: &str, fixture: bool) -> Result<Url, &'static str> {
    if raw.len() > 4096 || raw.chars().any(char::is_control) {
        return Err("invalid_provider_endpoint");
    }
    let mut url = Url::parse(raw).map_err(|_| "invalid_provider_endpoint")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port_or_known_default() == Some(0)
    {
        return Err("invalid_provider_endpoint");
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
        return Err("provider_private_destination");
    }
    let allowed = |ip: std::net::IpAddr| public_ip(ip) || (fixture && ip.is_loopback());
    if matches!(url.host(),Some(Host::Ipv4(ip)) if !allowed(ip.into()))
        || matches!(url.host(),Some(Host::Ipv6(ip)) if !allowed(ip.into()))
    {
        return Err("provider_private_destination");
    }
    if url.path().ends_with("/player_api.php") || url.path().ends_with("/get.php") {
        let path = url
            .path()
            .rsplit_once('/')
            .map(|v| v.0)
            .unwrap_or("")
            .to_owned();
        url.set_path(&path);
    }
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    Ok(url)
}

impl ProviderService {
    pub(super) fn fixture_transport(&self) -> bool {
        #[cfg(test)]
        {
            self.allow_test_loopback
        }
        #[cfg(not(test))]
        {
            false
        }
    }
    pub(super) async fn protected_json(&self, target: Url, limit: usize) -> Result<Value, String> {
        self.protected_json_with_timeout(target, limit, Duration::from_secs(25))
            .await
    }
    async fn protected_json_with_timeout(
        &self,
        target: Url,
        limit: usize,
        request_timeout: Duration,
    ) -> Result<Value, String> {
        tokio::time::timeout(request_timeout + Duration::from_secs(5), async {
            let host = match target.host().ok_or("invalid_provider_endpoint")? {
                Host::Domain(v) => v.to_owned(),
                Host::Ipv4(v) => v.to_string(),
                Host::Ipv6(v) => v.to_string(),
            };
            let port = target
                .port_or_known_default()
                .ok_or("invalid_provider_endpoint")?;
            let addresses = tokio::time::timeout(
                Duration::from_secs(3),
                tokio::net::lookup_host((host.as_str(), port)),
            )
            .await
            .map_err(|_| "provider_dns_unavailable")?
            .map_err(|_| "provider_dns_unavailable")?
            .take(17)
            .collect::<Vec<_>>();
            if addresses.is_empty()
                || addresses.len() > 16
                || addresses.iter().any(|a| {
                    !public_ip(a.ip()) && !(self.fixture_transport() && a.ip().is_loopback())
                })
            {
                return Err("provider_private_destination".into());
            }
            let client = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .resolve_to_addrs(&host, &addresses)
                .connect_timeout(Duration::from_secs(3))
                .timeout(request_timeout)
                .build()
                .map_err(|error| request_error(&error, false))?;
            let mut response = client
                .get(target)
                .send()
                .await
                .map_err(|error| request_error(&error, false))?;
            match response.status().as_u16() {
                200..=299 => {}
                300..=399 => return Err("provider_redirect_rejected".into()),
                401 | 403 => return Err("provider_credentials_rejected".into()),
                429 => return Err("provider_rate_limited".into()),
                _ => return Err("provider_unavailable".into()),
            }
            if response.content_length().is_some_and(|n| n > limit as u64) {
                return Err("provider_response_too_large".into());
            }
            let mut bytes = zeroize::Zeroizing::new(Vec::new());
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|error| request_error(&error, true))?
            {
                if bytes.len().saturating_add(chunk.len()) > limit {
                    return Err("provider_response_too_large".into());
                }
                bytes.extend_from_slice(&chunk);
            }
            tokio::task::spawn_blocking(move || {
                serde_json::from_slice(&bytes).map_err(|_| "provider_protocol_invalid".to_string())
            })
            .await
            .map_err(|_| "provider_unavailable".to_string())?
        })
        .await
        .map_err(|_| "provider_timeout".to_string())?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn request_and_body_deadlines_are_distinct_from_interrupted_bodies() {
        let mut service = ProviderService::new(
            Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            reqwest::Client::new(),
        );
        service.allow_test_loopback = true;
        for mode in 0..3 {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = Url::parse(&format!(
                "http://{}/player_api.php?username=private-user&password=private-password",
                listener.local_addr().unwrap()
            ))
            .unwrap();
            let fixture = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut data = Vec::new();
                let mut buffer = [0u8; 1024];
                while !data.windows(4).any(|v| v == b"\r\n\r\n") && data.len() < 8192 {
                    let n = stream.read(&mut buffer).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    data.extend_from_slice(&buffer[..n]);
                }
                if mode != 0 {
                    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100\r\nConnection: close\r\n\r\n").await.unwrap();
                    if mode == 1 {
                        stream.write_all(b"{}").await.unwrap();
                        return;
                    }
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            });
            let result = service
                .protected_json_with_timeout(url, 1024, Duration::from_millis(500))
                .await;
            fixture.abort();
            let expected = if mode == 1 {
                "provider_response_interrupted"
            } else {
                "provider_timeout"
            };
            assert_eq!(result.unwrap_err(), expected);
        }
    }
}
