//! Credential-safe HTTP(S) Xtream transport: validate DNS and pin every request.
use super::*;

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
    crate::source_http::validate(&url, fixture).map_err(|e| e.provider_code())?;
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
        crate::source_http::json(
            target,
            limit,
            request_timeout,
            self.fixture_transport(),
            false,
        )
        .await
        .map_err(|e| e.provider_code().to_owned())
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
