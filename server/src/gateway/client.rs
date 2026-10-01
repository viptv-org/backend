//! Gateway-only HTTPS control transport. Resolve/validate/pin each destination;
//! do not follow redirects with integration credentials or inherit host proxies.
pub(crate) use crate::source_http::public_ip;
use futures::FutureExt;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use url::{Host, Url};

type Result<T> = std::result::Result<T, &'static str>;
pub(crate) fn endpoint(raw: &str) -> Result<Url> {
    if raw.len() > 2048 || raw.chars().any(char::is_control) {
        return Err("invalid_gateway_endpoint");
    }
    let mut url = Url::parse(raw).map_err(|_| "invalid_gateway_endpoint")?;
    if url.scheme() != "https"
        || url.host().is_none()
        || url.port_or_known_default() == Some(0)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("invalid_gateway_endpoint");
    }
    let host = url.host_str().unwrap_or_default().trim_end_matches('.');
    if host.eq_ignore_ascii_case("localhost")
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
    {
        return Err("gateway_private_destination");
    }
    if matches!(url.host(),Some(Host::Ipv4(ip)) if !public_ip(ip.into()))
        || matches!(url.host(),Some(Host::Ipv6(ip)) if !public_ip(ip.into()))
    {
        return Err("gateway_private_destination");
    }
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    if url.as_str().len() > 2048 {
        return Err("invalid_gateway_endpoint");
    }
    Ok(url)
}
#[derive(Clone)]
pub(crate) struct Client {
    gate: std::sync::Arc<tokio::sync::Semaphore>,
    starts: std::sync::Arc<tokio::sync::Semaphore>,
    #[cfg(test)]
    fixture: Option<Url>,
}
impl Default for Client {
    fn default() -> Self {
        Self {
            gate: std::sync::Arc::new(tokio::sync::Semaphore::new(4)),
            starts: std::sync::Arc::new(tokio::sync::Semaphore::new(8)),
            #[cfg(test)]
            fixture: None,
        }
    }
}
#[derive(Clone, Deserialize)]
pub(crate) struct Capabilities {
    pub version: u32,
    pub ready: bool,
    pub protocols: Vec<String>,
    pub namespaces: Vec<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
    pub available: Option<Capacity>,
}
#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct Capacity {
    pub inputs: u32,
    pub outputs: u32,
    pub viewers: u32,
}
impl Client {
    #[cfg(test)]
    pub(crate) fn fixture(url: Url) -> Self {
        Self {
            fixture: Some(url),
            ..Self::default()
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn request<'a>(
        &'a self,
        base: &'a str,
        key: &'a [u8],
        method: reqwest::Method,
        path: &'a str,
        body: Option<&'a serde_json::Value>,
        idempotency: Option<&'a str>,
        timeout: Duration,
    ) -> futures::future::BoxFuture<'a, Result<serde_json::Value>> {
        async move {
            let gate = if path == "v1/sessions" && method == reqwest::Method::POST {
                &self.starts
            } else {
                &self.gate
            };
            let _permit = gate
                .clone()
                .try_acquire_owned()
                .map_err(|_| "gateway_checks_busy")?;
            let target = endpoint(base)?
                .join(path)
                .map_err(|_| "invalid_gateway_endpoint")?;
            #[cfg(test)]
            let target = if let Some(base) = &self.fixture {
                base.join(path).map_err(|_| "invalid_gateway_endpoint")?
            } else {
                target
            };
            let host = match target.host().ok_or("invalid_gateway_endpoint")? {
                Host::Domain(value) => value.to_owned(),
                Host::Ipv4(value) => value.to_string(),
                Host::Ipv6(value) => value.to_string(),
            };
            let port = target
                .port_or_known_default()
                .ok_or("invalid_gateway_endpoint")?;
            let addresses = tokio::time::timeout(
                Duration::from_secs(3),
                tokio::net::lookup_host((host.as_str(), port)),
            )
            .await
            .map_err(|_| "gateway_dns_unavailable")?
            .map_err(|_| "gateway_dns_unavailable")?
            .take(17)
            .collect::<Vec<_>>();
            #[cfg(test)]
            let fixture = self.fixture.is_some();
            #[cfg(not(test))]
            let fixture = false;
            if addresses.is_empty()
                || addresses.len() > 16
                || addresses.iter().any(|address| {
                    !public_ip(address.ip()) && !(fixture && address.ip().is_loopback())
                })
            {
                return Err("gateway_private_destination");
            }
            let client = reqwest::Client::builder()
                .no_proxy()
                .https_only(!fixture)
                .redirect(reqwest::redirect::Policy::none())
                .resolve_to_addrs(&host, &addresses)
                .connect_timeout(Duration::from_secs(3))
                .timeout(timeout)
                .build()
                .map_err(|_| "gateway_unavailable")?;
            let mut authorization = Vec::from(b"Bearer ".as_slice());
            authorization.extend_from_slice(key);
            let mut header = reqwest::header::HeaderValue::from_bytes(&authorization)
                .map_err(|_| "invalid_gateway_key")?;
            header.set_sensitive(true);
            use zeroize::Zeroize;
            authorization.zeroize();
            let mut request = client
                .request(method, target)
                .header(reqwest::header::AUTHORIZATION, header);
            if let Some(body) = body {
                request = request.json(body);
            }
            if let Some(idempotency) = idempotency {
                request = request.header("idempotency-key", idempotency);
            }
            let mut response = request.send().await.map_err(|_| "gateway_unavailable")?;
            let status = response.status().as_u16();
            if (300..400).contains(&status) {
                return Err("gateway_redirect_rejected");
            }
            if status == 204 {
                return Ok(serde_json::Value::Null);
            }
            if response.content_length().is_some_and(|size| size > 65536) {
                return Err("gateway_protocol_invalid");
            }
            let mut data = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| "gateway_unavailable")? {
                if data.len() + chunk.len() > 65536 {
                    return Err("gateway_protocol_invalid");
                }
                data.extend_from_slice(&chunk);
            }
            let value: serde_json::Value = if data.is_empty() && !(200..300).contains(&status) {
                serde_json::Value::Null
            } else {
                serde_json::from_slice(&data).map_err(|_| "gateway_protocol_invalid")?
            };
            if !(200..300).contains(&status) {
                return Err(super::protocol::failure_code(
                    value
                        .pointer("/error/code")
                        .and_then(serde_json::Value::as_str),
                )
                .unwrap_or(match status {
                    401 => "gateway_key_rejected",
                    403 => "gateway_scope_missing",
                    _ => "gateway_unavailable",
                }));
            }
            Ok(value)
        }
        .boxed()
    }
    pub(crate) async fn capabilities(
        &self,
        base: &str,
        key: &[u8],
        namespace: &str,
    ) -> Result<Capabilities> {
        let value = self
            .request(
                base,
                key,
                reqwest::Method::GET,
                "v1/capabilities",
                None,
                None,
                Duration::from_secs(5),
            )
            .await?;
        let capabilities: Capabilities =
            serde_json::from_value(value).map_err(|_| "gateway_protocol_invalid")?;
        if capabilities.version != 1
            || capabilities.protocols.len() > 16
            || capabilities.namespaces.len() > 32
            || capabilities.scopes.len() > 5
            || !capabilities.protocols.iter().any(|value| value == "hls")
        {
            return Err("gateway_protocol_invalid");
        }
        if !capabilities
            .namespaces
            .iter()
            .any(|value| value == namespace)
            || ["capabilities", "create", "read", "renew", "release"]
                .iter()
                .any(|scope| !capabilities.scopes.iter().any(|value| value == scope))
        {
            return Err("gateway_scope_missing");
        }
        if !capabilities.ready {
            return Err("gateway_not_ready");
        }
        Ok(capabilities)
    }
}
