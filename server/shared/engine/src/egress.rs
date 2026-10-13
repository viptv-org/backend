//! Bounded public HTTP egress shared by source integrations.
use crate::Error;
use futures::StreamExt;
use std::{
    collections::BTreeMap,
    net::IpAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use url::{Host, Url};
#[derive(Clone)]
pub struct HttpRequest {
    pub url: Url,
    pub method: String,
    pub headers: BTreeMap<String, String>,
    pub body: Option<String>,
    pub binary_body: Option<Vec<u8>>,
    pub timeout: Option<Duration>,
    pub max_redirects: Option<usize>,
}
impl HttpRequest {
    pub fn get(url: Url) -> Self {
        Self {
            url,
            method: "GET".into(),
            headers: BTreeMap::new(),
            body: None,
            binary_body: None,
            timeout: None,
            max_redirects: None,
        }
    }
}
pub struct HttpResponse {
    pub url: Url,
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
    pub truncated: bool,
}
pub fn public_ip(ip: IpAddr) -> bool {
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
            let w = ip.segments();
            (w[0] & 0xe000) == 0x2000
                && w[0] != 0x2002
                && !(w[0] == 0x2001 && (w[1] < 0x200 || w[1] == 0xdb8))
        }
    }
}
pub fn public_url(url: &Url) -> bool {
    let h = url
        .host_str()
        .unwrap_or_default()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    url.as_str().len() <= 16384
        && matches!(url.scheme(), "http" | "https")
        && !h.is_empty()
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && url.port_or_known_default() != Some(0)
        && h != "localhost"
        && !h.ends_with(".localhost")
        && !h.ends_with(".local")
        && !h.ends_with(".internal")
        && !matches!(url.host(),Some(Host::Ipv4(ip)) if !public_ip(ip.into()))
        && !matches!(url.host(),Some(Host::Ipv6(ip)) if !public_ip(ip.into()))
}
/// Each engine is scoped to a single job; SDK caches cannot cross accounts or revisions.
pub struct ScopedHttp {
    authorize: Arc<dyn Fn() -> bool + Send + Sync>,
    requests: AtomicUsize,
    permits: tokio::sync::Semaphore,
    deadline: tokio::time::Instant,
}
impl ScopedHttp {
    pub fn new(authorize: Arc<dyn Fn() -> bool + Send + Sync>) -> Self {
        Self {
            authorize,
            requests: AtomicUsize::new(0),
            permits: tokio::sync::Semaphore::new(4),
            deadline: tokio::time::Instant::now() + Duration::from_secs(45),
        }
    }
    fn check(&self, url: &Url) -> Result<(), Error> {
        if !(self.authorize)() || tokio::time::Instant::now() >= self.deadline || !public_url(url) {
            Err(Error::Forbidden)
        } else {
            Ok(())
        }
    }
    pub async fn fetch(
        &self,
        mut request: HttpRequest,
        limit: usize,
        prefix: bool,
    ) -> Result<HttpResponse, Error> {
        self.check(&request.url)?;
        let denied = || Error::Forbidden;
        if !matches!(request.method.as_str(), "GET" | "POST" | "HEAD")
            || request
                .binary_body
                .as_ref()
                .map(Vec::len)
                .unwrap_or_else(|| request.body.as_ref().map(String::len).unwrap_or(0))
                > 1024 * 1024
            || request.headers.len() > 32
            || request.headers.iter().any(|(k, v)| {
                k.len() > 128
                    || v.len() > 8192
                    || k.eq_ignore_ascii_case("host")
                    || k.eq_ignore_ascii_case("content-length")
                    || k.eq_ignore_ascii_case("transfer-encoding")
            })
        {
            return Err(denied());
        }
        if self.requests.fetch_add(1, Ordering::Relaxed) >= 96 {
            return Err(denied());
        }
        let _permit = self.permits.acquire().await.map_err(|_| denied())?;
        let redirects = request.max_redirects.unwrap_or(8).min(8);
        for hop in 0..=redirects {
            self.check(&request.url)?;
            let err = || Error::Unavailable;
            let host = request.url.host_str().ok_or_else(err)?;
            let addresses: Vec<_> = tokio::net::lookup_host((
                host,
                request.url.port_or_known_default().ok_or_else(err)?,
            ))
            .await
            .map_err(|_| err())?
            .take(17)
            .collect();
            if addresses.is_empty()
                || addresses.len() > 16
                || addresses.iter().any(|a| !public_ip(a.ip()))
            {
                return Err(Error::Forbidden);
            }
            let client = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .resolve_to_addrs(host, &addresses)
                .connect_timeout(Duration::from_secs(3))
                .timeout(
                    request
                        .timeout
                        .unwrap_or(Duration::from_secs(8))
                        .min(Duration::from_secs(10)),
                )
                .build()
                .map_err(|_| err())?;
            let method =
                reqwest::Method::from_bytes(request.method.as_bytes()).map_err(|_| err())?;
            let mut outgoing = client.request(method, request.url.clone());
            for (k, v) in &request.headers {
                outgoing = outgoing.header(k, v);
            }
            if let Some(body) = &request.binary_body {
                outgoing = outgoing.body(body.clone());
            } else if let Some(body) = &request.body {
                outgoing = outgoing.body(body.clone());
            }
            let response = outgoing.send().await.map_err(|_| err())?;
            self.check(&request.url)?;
            let status = response.status().as_u16();
            let headers: BTreeMap<_, _> = response
                .headers()
                .iter()
                .filter_map(|(k, v)| {
                    v.to_str()
                        .ok()
                        .map(|v| (k.as_str().to_ascii_lowercase(), v.to_string()))
                })
                .collect();
            if matches!(status, 301 | 302 | 303 | 307 | 308) && hop < redirects {
                let next = request
                    .url
                    .join(headers.get("location").ok_or_else(err)?)
                    .map_err(|_| err())?;
                if request.url.scheme() == "https" && next.scheme() != "https" {
                    return Err(err());
                }
                if request.url.origin() != next.origin() {
                    // A redirect recipient never inherits declared credentials or POST data.
                    if request.body.is_some() || request.binary_body.is_some() {
                        return Err(err());
                    }
                    request.headers.clear();
                }
                if status == 303 || ((status == 301 || status == 302) && request.method == "POST") {
                    request.method = "GET".into();
                    request.body = None;
                    request.binary_body = None;
                }
                request.url = next;
                continue;
            }
            let mut bytes = Vec::new();
            let mut truncated = false;
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                self.check(&request.url)?;
                let chunk = chunk.map_err(|_| err())?;
                if bytes.len() + chunk.len() > limit {
                    if !prefix {
                        return Err(err());
                    }
                    bytes.extend_from_slice(&chunk[..limit.saturating_sub(bytes.len())]);
                    truncated = true;
                    break;
                }
                bytes.extend_from_slice(&chunk);
            }
            self.check(&request.url)?;
            return Ok(HttpResponse {
                url: request.url,
                status,
                headers,
                body: bytes,
                truncated,
            });
        }
        Err(Error::Unavailable)
    }
}

impl ScopedHttp {
    pub async fn request(&self, request: HttpRequest) -> Result<HttpResponse, Error> {
        tokio::time::timeout_at(self.deadline, self.fetch(request, 3 * 1024 * 1024, false))
            .await
            .map_err(|_| Error::Unavailable)?
    }
    pub async fn probe(
        &self,
        request: HttpRequest,
        max_bytes: usize,
    ) -> Result<HttpResponse, Error> {
        tokio::time::timeout_at(
            self.deadline,
            self.fetch(request, max_bytes.clamp(1, 8192), true),
        )
        .await
        .map_err(|_| Error::Unavailable)?
    }
}
