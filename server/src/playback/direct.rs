//! Original-media transport. Only server-discovered resources enter the map;
//! viewers supply opaque resource keys, never upstream URLs.
use super::*;
use axum::{
    body::{Body, Bytes},
    http::{header, HeaderMap, Method, StatusCode},
    response::Response,
};
use futures::StreamExt;
use std::sync::atomic::{AtomicBool, Ordering};
use url::Url;

const CHUNK: u64 = 1024 * 1024;
const RESOURCE_LIMIT: usize = 4096;
const CACHE_LIMIT: usize = 8 * 1024 * 1024;

pub(super) struct Direct {
    client: reqwest::Client,
    headers: HeaderMap,
    root: Url,
    resources: Mutex<HashMap<String, Url>>,
    // One input reservation means one upstream request at a time. File reads
    // yield between bounded ranges so independent viewers cannot monopolize it.
    fetch: Arc<Mutex<()>>,
    proxy: Option<String>,
    destinations: Mutex<HashMap<String, reqwest::Client>>,
    cache: Mutex<HashMap<String, (Instant, Bytes)>>,
    pub closed: AtomicBool,
    pub failed: AtomicBool,
    progress: Mutex<Option<String>>,
    size: Option<u64>,
    validator: Option<String>,
    permits: Arc<InputPermits>,
}

impl Direct {
    pub async fn prepare(
        url: Url,
        headers: &HashMap<String, String>,
        format: &str,
        permits: Arc<InputPermits>,
    ) -> Result<Arc<Self>, String> {
        let mut public = HeaderMap::new();
        for (name, value) in headers {
            if name.eq_ignore_ascii_case(crate::provider::egress::HEADER) {
                continue;
            }
            if [
                "host",
                "connection",
                "content-length",
                "transfer-encoding",
                "range",
                "accept-encoding",
            ]
            .contains(&name.to_ascii_lowercase().as_str())
            {
                continue;
            }
            public.insert(
                header::HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| "Invalid media header")?,
                value.parse().map_err(|_| "Invalid media header")?,
            );
        }
        let client = crate::provider::egress::builder(
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .read_timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none()),
            headers
                .get(crate::provider::egress::HEADER)
                .map(String::as_str),
        )?
        .build()
        .map_err(|_| "Media transport unavailable")?;
        let mut direct = Self {
            client,
            headers: public,
            root: url,
            resources: Mutex::new(HashMap::new()),
            fetch: Arc::new(Mutex::new(())),
            proxy: headers.get(crate::provider::egress::HEADER).cloned(),
            destinations: Mutex::new(HashMap::new()),
            cache: Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            progress: Mutex::new(None),
            size: None,
            validator: None,
            permits,
        };
        if format == "mp4" {
            let response = direct
                .request(direct.root.clone(), Some("bytes=0-0"))
                .await?;
            let (start, end, total) =
                content_range(response.headers()).ok_or("Source does not support byte ranges")?;
            if response.status() != StatusCode::PARTIAL_CONTENT
                || start != 0
                || end != 0
                || total == 0
            {
                return Err("Source does not support byte ranges".into());
            }
            direct.size = Some(total);
            direct.validator = response
                .headers()
                .get(header::ETAG)
                .and_then(|v| v.to_str().ok())
                .filter(|s| !s.starts_with("W/"))
                .map(str::to_owned);
            let bytes = bounded(response, 1).await?;
            if bytes.len() != 1 {
                return Err("Invalid source range".into());
            }
        }
        let direct = Arc::new(direct);
        if format == "hls" {
            direct.playlist(direct.root.clone()).await?;
        }
        Ok(direct)
    }
    async fn destination_client(&self, url: &Url) -> Result<reqwest::Client, String> {
        if url.origin() == self.root.origin() {
            return Ok(self.client.clone());
        }
        if !public_host(url) {
            return Err("Unsafe media destination".into());
        }
        // An HTTP egress proxy resolves hostnames itself. Until its resolved
        // destination can be enforced, cross-origin WARP media uses managed HLS.
        if self.proxy.is_some() {
            return Err("Cross-origin proxy media requires managed playback".into());
        }
        let key = url.origin().ascii_serialization();
        if let Some(client) = self.destinations.lock().await.get(&key) {
            return Ok(client.clone());
        }
        let host = url.host_str().ok_or("Invalid media host")?;
        let port = url.port_or_known_default().ok_or("Invalid media port")?;
        let addresses = timeout(
            Duration::from_secs(3),
            tokio::net::lookup_host((host, port)),
        )
        .await
        .map_err(|_| "Media DNS timed out")?
        .map_err(|_| "Media DNS unavailable")?
        .collect::<Vec<_>>();
        if addresses.is_empty() || addresses.iter().any(|address| !public_ip(address.ip())) {
            return Err("Unsafe media destination".into());
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .resolve_to_addrs(host, &addresses)
            .build()
            .map_err(|_| "Media transport unavailable")?;
        let mut destinations = self.destinations.lock().await;
        if destinations.len() >= 32 {
            return Err("Too many media destinations".into());
        }
        destinations.insert(key, client.clone());
        Ok(client)
    }
    async fn request(
        &self,
        mut url: Url,
        range: Option<&str>,
    ) -> Result<reqwest::Response, String> {
        for _ in 0..4 {
            if self.closed.load(Ordering::Acquire) {
                return Err("Media expired".into());
            }
            crate::util::validate_url(url.as_str())?;
            let client = self.destination_client(&url).await?;
            let mut headers = self.headers.clone();
            if url.origin() != self.root.origin() {
                headers.remove(header::AUTHORIZATION);
                headers.remove(header::COOKIE);
            }
            let mut request = client
                .get(url.clone())
                .headers(headers)
                .header(header::ACCEPT_ENCODING, "identity");
            if let Some(range) = range {
                request = request.header(header::RANGE, range);
            }
            if let Some(validator) = &self.validator {
                request = request.header(header::IF_MATCH, validator);
            }
            let response = request
                .send()
                .await
                .map_err(|_| "Media origin unavailable")?;
            if response.status().is_redirection() {
                let location = response
                    .headers()
                    .get(header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or("Invalid media redirect")?;
                url = url.join(location).map_err(|_| "Invalid media redirect")?;
                continue;
            }
            if !response.status().is_success() {
                return Err(format!("Media origin HTTP {}", response.status().as_u16()));
            }
            return Ok(response);
        }
        Err("Too many media redirects".into())
    }
    async fn playlist(&self, url: Url) -> Result<Bytes, String> {
        let _guard = self.fetch.lock().await;
        let response = self.request(url, None).await?;
        let base = response.url().clone();
        let bytes = bounded(response, 256 * 1024).await?;
        let text = std::str::from_utf8(&bytes).map_err(|_| "Invalid HLS playlist")?;
        let (output, resources) = rewrite(text, &base)?;
        let mut origins = HashSet::new();
        for url in resources.values() {
            if origins.insert(url.origin()) {
                self.destination_client(url).await?;
            }
        }
        let mut map = self.resources.lock().await;
        if map.len() + resources.len() > RESOURCE_LIMIT {
            map.clear();
        }
        map.extend(resources);
        *self.progress.lock().await = Some(
            text.lines()
                .filter(|l| l.starts_with("#EXT-X-MEDIA-SEQUENCE:") || !l.starts_with('#'))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        Ok(Bytes::from(output))
    }
    async fn cached_range(&self, url: Url, start: u64, end: u64) -> Result<Bytes, String> {
        let key = format!("{}:{start}:{end}", url.as_str());
        let _guard = self.fetch.lock().await;
        if let Some((at, data)) = self.cache.lock().await.get(&key) {
            if at.elapsed() < Duration::from_secs(15) {
                return Ok(data.clone());
            }
        }
        let response = self
            .request(url, Some(&format!("bytes={start}-{end}")))
            .await?;
        if response.status() != StatusCode::PARTIAL_CONTENT
            || content_range(response.headers())
                != Some((start, end, self.size.ok_or("Missing source length")?))
        {
            return Err("Invalid source range response".into());
        }
        let bytes = bounded(response, (end - start + 1) as usize).await?;
        if bytes.len() as u64 != end - start + 1 {
            return Err("Incomplete source range".into());
        }
        self.cache_insert(key, bytes.clone()).await;
        Ok(bytes)
    }
    async fn cache_insert(&self, key: String, bytes: Bytes) {
        let mut cache = self.cache.lock().await;
        cache.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(15));
        if cache.values().map(|(_, b)| b.len()).sum::<usize>() + bytes.len() > CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(key, (Instant::now(), bytes));
    }
    pub async fn serve(
        self: Arc<Self>,
        file: &str,
        method: Method,
        headers: HeaderMap,
    ) -> Result<Response, String> {
        if self.closed.load(Ordering::Acquire) {
            return Err("Media expired".into());
        }
        if let Some(size) = self.size {
            if file != "source.mp4" {
                return Err("Media not found".into());
            }
            let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
            let range = if headers
                .get(header::IF_RANGE)
                .is_some_and(|v| Some(v.to_str().unwrap_or("")) != self.validator.as_deref())
            {
                None
            } else {
                range
            };
            let Some((start, end)) = range_bounds(range, size) else {
                return Ok(Response::builder()
                    .status(416)
                    .header(header::CONTENT_RANGE, format!("bytes */{size}"))
                    .body(Body::empty())
                    .unwrap());
            };
            let mut builder = Response::builder()
                .status(if range.is_some() { 206 } else { 200 })
                .header(header::CONTENT_TYPE, "video/mp4")
                .header(header::ACCEPT_RANGES, "bytes")
                .header(header::CONTENT_LENGTH, (end - start + 1).to_string())
                .header(header::CACHE_CONTROL, "no-store");
            if range.is_some() {
                builder =
                    builder.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{size}"));
            }
            if let Some(value) = &self.validator {
                builder = builder.header(header::ETAG, value);
            }
            if method == Method::HEAD {
                return builder
                    .body(Body::empty())
                    .map_err(|_| "Invalid media response".into());
            }
            let stream = async_stream::try_stream! {
                let _permits=self.permits.clone();
                let mut offset=start;
                while offset<=end {
                    if self.closed.load(Ordering::Acquire) {Err(std::io::Error::other("Media expired"))?;}
                    let last=end.min(offset.saturating_add(CHUNK-1));
                    let bytes=self.cached_range(self.root.clone(),offset,last).await.map_err(|_|std::io::Error::other("Media range failed"))?;
                    offset=last+1;
                    yield bytes;
                }
            };
            return builder
                .body(Body::from_stream(futures::StreamExt::map(
                    stream,
                    |item: Result<axum::body::Bytes, std::io::Error>| item,
                )))
                .map_err(|_| "Invalid media response".into());
        }
        let url = if file == "index.m3u8" {
            self.root.clone()
        } else {
            self.resources
                .lock()
                .await
                .get(file)
                .cloned()
                .ok_or("Media not found")?
        };
        if file.ends_with(".m3u8") {
            let bytes = self
                .playlist(url)
                .await
                .inspect_err(|_| self.failed.store(true, Ordering::Release))?;
            return Ok(Response::builder()
                .header(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")
                .header(header::CACHE_CONTROL, "no-store")
                .body(if method == Method::HEAD {
                    Body::empty()
                } else {
                    Body::from(bytes)
                })
                .unwrap());
        }
        // HLS resources are bounded and serialized; cached segments are shared
        // between viewers without sharing their capability URLs.
        let range = headers
            .get(header::RANGE)
            .and_then(|v| v.to_str().ok())
            .filter(|v| v.len() < 128 && v.starts_with("bytes=") && !v.contains(','));
        let key = format!("{}:{}", url, range.unwrap_or(""));
        let guard = self.fetch.clone().lock_owned().await;
        // Ranges carry upstream status and lengths; do not use a status-less cache.
        if range.is_none() {
            if let Some((at, bytes)) = self.cache.lock().await.get(&key) {
                if at.elapsed() < Duration::from_secs(15) {
                    return Ok(Response::builder()
                        .header(header::CONTENT_TYPE, "application/octet-stream")
                        .header(header::CACHE_CONTROL, "no-store")
                        .body(if method == Method::HEAD {
                            Body::empty()
                        } else {
                            Body::from(bytes.clone())
                        })
                        .unwrap());
                }
            }
        }
        let response = self
            .request(url, range)
            .await
            .inspect_err(|_| self.failed.store(true, Ordering::Release))?;
        if range.is_some() && response.status() != StatusCode::PARTIAL_CONTENT {
            return Err("Invalid HLS resource range".into());
        }
        let mut builder = Response::builder()
            .status(response.status())
            .header(header::CACHE_CONTROL, "no-store");
        for name in [
            header::CONTENT_TYPE,
            header::CONTENT_RANGE,
            header::ACCEPT_RANGES,
        ] {
            if let Some(value) = response.headers().get(&name) {
                builder = builder.header(name, value);
            }
        }
        if let Some(value) = response.headers().get(header::CONTENT_LENGTH) {
            builder = builder.header(header::CONTENT_LENGTH, value);
        }
        if method == Method::HEAD {
            return builder
                .body(Body::empty())
                .map_err(|_| "Invalid media response".into());
        }
        let cacheable = range.is_none();
        let (send, mut receive) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(2);
        tokio::spawn(async move {
            let _guard = guard;
            let _permits = self.permits.clone();
            let result = async {
                let mut chunks = response.bytes_stream();
                let mut saved = Vec::new();
                let mut cacheable = cacheable;
                while let Some(chunk) = chunks.next().await {
                    if self.closed.load(Ordering::Acquire) {
                        return Err(std::io::Error::other("Media expired"));
                    }
                    let chunk = chunk.map_err(|_| {
                        self.failed.store(true, Ordering::Release);
                        std::io::Error::other("Media body failed")
                    })?;
                    if cacheable && saved.len() + chunk.len() <= CACHE_LIMIT {
                        saved.extend_from_slice(&chunk);
                    } else if cacheable {
                        cacheable = false;
                        saved.clear();
                        saved.shrink_to_fit();
                    }
                    // A stalled viewer must not keep the shared input locked.
                    timeout(Duration::from_secs(5), send.send(Ok(chunk)))
                        .await
                        .map_err(|_| std::io::Error::other("Media consumer stalled"))?
                        .map_err(|_| std::io::Error::other("Media consumer closed"))?;
                }
                if cacheable {
                    self.cache_insert(key, Bytes::from(saved)).await;
                }
                Ok::<(), std::io::Error>(())
            }
            .await;
            if let Err(error) = result {
                let _ = timeout(Duration::from_secs(1), send.send(Err(error))).await;
            }
        });
        let stream =
            async_stream::stream! {while let Some(chunk)=receive.recv().await {yield chunk;}};
        builder
            .body(Body::from_stream(stream))
            .map_err(|_| "Invalid media response".into())
    }

    pub async fn progress(&self) -> Option<String> {
        self.progress.lock().await.clone()
    }
}
fn public_host(url: &Url) -> bool {
    let host = url.host_str().unwrap_or("").trim_matches(['[', ']']);
    if host.eq_ignore_ascii_case("localhost")
        || host.ends_with(".localhost")
        || host.ends_with(".local")
    {
        return false;
    }
    host.parse::<std::net::IpAddr>()
        .map(public_ip)
        .unwrap_or(true)
}
fn public_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => {
            let octets = ip.octets();
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_unspecified()
                && !ip.is_multicast()
                && !ip.is_broadcast()
                && octets[0] != 0
                && octets[0] < 224
                && !(octets[0] == 100 && (64..=127).contains(&octets[1]))
        }
        std::net::IpAddr::V6(ip) => {
            !ip.is_loopback()
                && !ip.is_unspecified()
                && !ip.is_unique_local()
                && !ip.is_unicast_link_local()
                && !ip.is_multicast()
                && ip.to_ipv4_mapped().is_none()
        }
    }
}

async fn bounded(response: reqwest::Response, limit: usize) -> Result<Bytes, String> {
    let mut output = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "Media read failed")?;
        if output.len() + chunk.len() > limit {
            return Err("Media resource exceeds limit".into());
        }
        output.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(output))
}
fn content_range(headers: &HeaderMap) -> Option<(u64, u64, u64)> {
    let raw = headers
        .get(header::CONTENT_RANGE)?
        .to_str()
        .ok()?
        .strip_prefix("bytes ")?;
    let (range, total) = raw.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?, total.parse().ok()?))
}
fn range_bounds(raw: Option<&str>, size: u64) -> Option<(u64, u64)> {
    if size == 0 {
        return None;
    }
    let Some(raw) = raw else {
        return Some((0, size - 1));
    };
    let (start, end) = raw.strip_prefix("bytes=")?.split_once('-')?;
    if start.is_empty() {
        let count = end.parse::<u64>().ok()?;
        return (count > 0).then_some((size.saturating_sub(count), size - 1));
    }
    let start = start.parse::<u64>().ok()?;
    let end = if end.is_empty() {
        size - 1
    } else {
        end.parse::<u64>().ok()?.min(size - 1)
    };
    (start < size && start <= end).then_some((start, end))
}
fn resource(
    base: &Url,
    raw: &str,
    playlist: bool,
    map: &mut HashMap<String, Url>,
) -> Result<String, String> {
    if raw.contains("{$") {
        return Err("Unsupported HLS variables".into());
    }
    let url = base.join(raw).map_err(|_| "Invalid HLS resource")?;
    crate::util::validate_url(url.as_str())?;
    let hash = Sha256::digest(url.as_str().as_bytes());
    // Some native demuxers validate segment suffixes before inspecting bytes.
    // Preserve only a small safe extension, never the upstream filename/query.
    let suffix = url
        .path()
        .rsplit('.')
        .next()
        .filter(|s| ["ts", "m4s", "mp4", "aac", "vtt", "key"].contains(s))
        .unwrap_or("ts");
    let key = format!("d-{:x}.{}", hash, if playlist { "m3u8" } else { suffix });
    map.insert(key.clone(), url);
    Ok(key)
}
fn rewrite(text: &str, base: &Url) -> Result<(String, HashMap<String, Url>), String> {
    if !text.trim_start().starts_with("#EXTM3U") {
        return Err("Invalid HLS playlist".into());
    }
    let mut out = String::new();
    let mut map = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        // Until every rendition is inspected, adaptive master playlists and
        // alternate tracks use managed HLS; a probe of one variant is not proof
        // that the other variants are compatible.
        if [
            "#EXT-X-STREAM-INF:",
            "#EXT-X-MEDIA:",
            "#EXT-X-I-FRAME-STREAM-INF:",
            "#EXT-X-DEFINE:",
            "#EXT-X-PART",
            "#EXT-X-PRELOAD-HINT",
            "#EXT-X-RENDITION-REPORT",
            "#EXT-X-SESSION-",
        ]
        .iter()
        .any(|p| line.starts_with(p))
        {
            return Err("HLS rendition requires managed playback".into());
        }
        if line.starts_with("#EXT-X-KEY:")
            && !line.contains("METHOD=AES-128,")
            && !line.contains("METHOD=NONE")
        {
            return Err("Unsupported HLS encryption".into());
        }
        if !line.is_empty() && !line.starts_with('#') {
            out.push_str(&resource(base, line, false, &mut map)?);
        } else if let Some(at) = line.find("URI=\"") {
            if !line.starts_with("#EXT-X-KEY:") && !line.starts_with("#EXT-X-MAP:") {
                return Err("Unsupported HLS reference".into());
            }
            let start = at + 5;
            let end = start + line[start..].find('"').ok_or("Invalid HLS URI")?;
            out.push_str(&line[..start]);
            out.push_str(&resource(base, &line[start..end], false, &mut map)?);
            out.push_str(&line[end..]);
        } else {
            if line.contains("URI=") {
                return Err("Invalid HLS reference".into());
            }
            out.push_str(line);
        }
        out.push('\n');
    }
    if map.len() > RESOURCE_LIMIT {
        return Err("Too many HLS resources".into());
    }
    Ok((out, map))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{extract::State, routing::get, Router};
    use std::sync::atomic::AtomicUsize;

    async fn fixture() -> (Url, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        async fn origin(State(count): State<Arc<AtomicUsize>>, headers: HeaderMap) -> Response {
            count.fetch_add(1, Ordering::SeqCst);
            let bytes = b"0123456789abcdef";
            let (start, end) = range_bounds(
                headers.get(header::RANGE).and_then(|v| v.to_str().ok()),
                bytes.len() as u64,
            )
            .unwrap();
            Response::builder()
                .status(206)
                .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/16"))
                .header(header::ETAG, "\"fixture-v1\"")
                .body(Body::from(bytes[start as usize..=end as usize].to_vec()))
                .unwrap()
        }
        let count = Arc::new(AtomicUsize::new(0));
        let app=Router::new().route("/movie.mp4",get(origin)).route("/live.m3u8",get(||async{"#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:6,\nsegment.ts?secret=never-public\n"})).route("/segment.ts",get(||async{b"original-segment".to_vec()})).route("/large.m3u8",get(||async{"#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\nlarge.ts\n"})).route("/large.ts",get(||async{vec![7u8;9*1024*1024]})).with_state(count.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, count, task)
    }
    fn permits() -> Arc<InputPermits> {
        Arc::new(InputPermits {
            _playback: Arc::new(Semaphore::new(1)).try_acquire_owned().unwrap(),
            _provider: None,
        })
    }
    #[tokio::test]
    async fn redirects_stay_server_side_and_reject_private_cross_origin() {
        let app = Router::new()
            .route(
                "/redirect.m3u8",
                get(|| async {
                    Response::builder()
                        .status(302)
                        .header(header::LOCATION, "/actual.m3u8?secret=private")
                        .body(Body::empty())
                        .unwrap()
                }),
            )
            .route(
                "/actual.m3u8",
                get(|| async { "#EXTM3U\n#EXTINF:6,\nsegment.ts?secret=private\n" }),
            )
            .route(
                "/unsafe.m3u8",
                get(|| async {
                    Response::builder()
                        .status(302)
                        .header(header::LOCATION, "http://127.0.0.1:1/private.m3u8")
                        .body(Body::empty())
                        .unwrap()
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let direct = Direct::prepare(
            base.join("/redirect.m3u8").unwrap(),
            &HashMap::new(),
            "hls",
            permits(),
        )
        .await
        .unwrap();
        let response = direct
            .serve("index.m3u8", Method::GET, HeaderMap::new())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!response.headers().contains_key(header::LOCATION));
        let playlist = String::from_utf8(bytes(response).await.to_vec()).unwrap();
        assert!(!playlist.contains("secret"));
        assert!(!playlist.contains("http"));
        let unsafe_result = Direct::prepare(
            base.join("/unsafe.m3u8").unwrap(),
            &HashMap::new(),
            "hls",
            permits(),
        )
        .await;
        assert!(matches!(unsafe_result, Err(error) if error == "Unsafe media destination"));
        task.abort();
    }
    async fn bytes(response: Response) -> Bytes {
        axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap()
    }
    #[tokio::test]
    async fn original_file_ranges_resume_share_bytes_and_reject_expired_access() {
        let (base, count, task) = fixture().await;
        let direct = Direct::prepare(
            base.join("/movie.mp4").unwrap(),
            &HashMap::new(),
            "mp4",
            permits(),
        )
        .await
        .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(header::RANGE, "bytes=4-9".parse().unwrap());
        let response = direct
            .clone()
            .serve("source.mp4", Method::GET, headers.clone())
            .await
            .unwrap();
        assert_eq!(response.status(), 206);
        assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes 4-9/16");
        assert_eq!(bytes(response).await, "456789");
        assert_eq!(
            bytes(
                direct
                    .clone()
                    .serve("source.mp4", Method::GET, headers)
                    .await
                    .unwrap()
            )
            .await,
            "456789"
        );
        assert_eq!(
            count.load(Ordering::SeqCst),
            2,
            "initial metadata range and one shared media range"
        );
        let mut headers = HeaderMap::new();
        headers.insert(header::RANGE, "bytes=90-100".parse().unwrap());
        assert_eq!(
            direct
                .clone()
                .serve("source.mp4", Method::GET, headers)
                .await
                .unwrap()
                .status(),
            416
        );
        let head = direct
            .clone()
            .serve("source.mp4", Method::HEAD, HeaderMap::new())
            .await
            .unwrap();
        assert_eq!(head.headers()[header::CONTENT_LENGTH], "16");
        assert!(bytes(head).await.is_empty());
        direct.closed.store(true, Ordering::Release);
        assert!(direct
            .serve("source.mp4", Method::GET, HeaderMap::new())
            .await
            .is_err());
        task.abort();
    }
    #[tokio::test]
    async fn original_hls_never_returns_origin_urls_and_preserves_segment_bytes() {
        let (base, _, task) = fixture().await;
        let direct = Direct::prepare(
            base.join("/live.m3u8").unwrap(),
            &HashMap::new(),
            "hls",
            permits(),
        )
        .await
        .unwrap();
        let response = direct
            .clone()
            .serve("index.m3u8", Method::GET, HeaderMap::new())
            .await
            .unwrap();
        let text = String::from_utf8(bytes(response).await.to_vec()).unwrap();
        assert!(!text.contains("secret"));
        assert!(!text.contains("http"));
        assert!(text.contains("#EXT-X-MEDIA-SEQUENCE:1"));
        let segment = text.lines().find(|l| l.starts_with("d-")).unwrap();
        assert_eq!(
            bytes(
                direct
                    .clone()
                    .serve(segment, Method::GET, HeaderMap::new())
                    .await
                    .unwrap()
            )
            .await,
            "original-segment"
        );
        assert!(direct
            .serve(
                "http://untrusted.invalid/file",
                Method::GET,
                HeaderMap::new()
            )
            .await
            .is_err());
        task.abort();
    }
    #[tokio::test]
    async fn large_hls_delivery_and_stalled_viewer_isolation() {
        let (base, _, task) = fixture().await;
        let direct = Direct::prepare(
            base.join("/large.m3u8").unwrap(),
            &HashMap::new(),
            "hls",
            permits(),
        )
        .await
        .unwrap();
        let playlist = bytes(
            direct
                .clone()
                .serve("index.m3u8", Method::GET, HeaderMap::new())
                .await
                .unwrap(),
        )
        .await;
        let text = std::str::from_utf8(&playlist).unwrap();
        let segment = text.lines().find(|line| line.starts_with("d-")).unwrap();
        let response = direct
            .clone()
            .serve(segment, Method::GET, HeaderMap::new())
            .await
            .unwrap();
        let received = axum::body::to_bytes(response.into_body(), 10 * 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(received.len(), 9 * 1024 * 1024);
        assert!(received.iter().all(|b| *b == 7));
        let stalled = direct
            .clone()
            .serve(segment, Method::GET, HeaderMap::new())
            .await
            .unwrap();
        let other = timeout(
            Duration::from_secs(9),
            direct
                .clone()
                .serve("index.m3u8", Method::GET, HeaderMap::new()),
        )
        .await
        .expect("stalled consumer monopolized input")
        .unwrap();
        assert_eq!(other.status(), 200);
        drop(stalled);
        direct.closed.store(true, Ordering::Release);
        task.abort();
    }

    #[test]
    fn hls_keys_maps_and_unsupported_variants_do_not_leak() {
        let base = Url::parse("https://origin.invalid/folder/list.m3u8").unwrap();
        let (text,resources)=rewrite("#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"../key?token=secret\"\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXT-X-BYTERANGE:20@0\nfile.ts\n",&base).unwrap();
        assert_eq!(resources.len(), 3);
        assert!(!text.contains("secret"));
        assert!(text.contains("#EXT-X-BYTERANGE:20@0"));
        for text in [
            "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=100\nvariant.m3u8",
            "#EXTM3U\n#EXT-X-MAP:URI=\"file:///etc/passwd\"",
            "#EXTM3U\n#EXT-X-CONTENT-STEERING:SERVER-URI=\"https://other.invalid\"",
            "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"key\"",
        ] {
            assert!(rewrite(text, &base).is_err());
        }
    }
    #[test]
    fn original_range_semantics() {
        assert_eq!(range_bounds(None, 16), Some((0, 15)));
        assert_eq!(range_bounds(Some("bytes=-4"), 16), Some((12, 15)));
        assert_eq!(range_bounds(Some("bytes=4-"), 16), Some((4, 15)));
        assert_eq!(range_bounds(Some("bytes=4-900"), 16), Some((4, 15)));
        for raw in [
            "bytes=-0",
            "bytes=20-30",
            "bytes=4-2",
            "bytes=0-1,5-6",
            "garbage",
        ] {
            assert_eq!(range_bounds(Some(raw), 16), None);
        }
    }
}

#[cfg(test)]
mod real_tests {
    use super::*;
    use axum::{
        extract::{Path, State},
        routing::get,
        Router,
    };
    #[tokio::test]
    #[ignore = "requires configured real FFmpeg/ffprobe; controlled fixtures only"]
    async fn real_original_mp4_hls_resume_and_decode_without_encoder() {
        let ffmpeg = std::env::var("VIPTV_TEST_FFMPEG").unwrap();
        let ffprobe = std::env::var("VIPTV_TEST_FFPROBE").unwrap();
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("movie.mp4");
        let result = Command::new(&ffmpeg)
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=640x360:rate=30",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000",
                "-t",
                "60",
                "-c:v",
                "libx264",
                "-threads",
                "1",
                "-profile:v",
                "high",
                "-level:v",
                "4.1",
                "-g",
                "30",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-ac",
                "2",
                "-metadata:s:a:0",
                "language=eng",
                "-movflags",
                "+faststart",
            ])
            .arg(&file)
            .output()
            .await
            .unwrap();
        assert!(result.status.success(), "fixture generation failed");
        let result = Command::new(&ffmpeg)
            .args(["-v", "error", "-i"])
            .arg(&file)
            .args([
                "-c",
                "copy",
                "-metadata:s:a:0",
                "language=",
                "-hls_time",
                "1",
                "-hls_playlist_type",
                "vod",
                "-hls_segment_filename",
            ])
            .arg(root.path().join("part-%03d.ts"))
            .arg(root.path().join("live.m3u8"))
            .output()
            .await
            .unwrap();
        assert!(result.status.success(), "HLS fixture generation failed");
        async fn origin(
            State(root): State<PathBuf>,
            Path(name): Path<String>,
            headers: HeaderMap,
        ) -> Response {
            let data = tokio::fs::read(root.join(name)).await.unwrap();
            let size = data.len() as u64;
            let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
            let (start, end) = range_bounds(range, size).unwrap();
            let mut builder = Response::builder()
                .status(if range.is_some() { 206 } else { 200 })
                .header(header::CONTENT_LENGTH, (end - start + 1).to_string())
                .header(header::ACCEPT_RANGES, "bytes");
            if range.is_some() {
                builder =
                    builder.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{size}"));
            }
            builder
                .body(Body::from(data[start as usize..=end as usize].to_vec()))
                .unwrap()
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/:name", get(origin))
            .with_state(root.path().to_path_buf());
        let origin = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let manager = PlaybackManager::new(Config {
            ffmpeg: root.path().join("encoder-must-not-run"),
            ffprobe: PathBuf::from(ffprobe),
            root: root.path().join("sessions"),
            max_sessions: 4,
            ttl: Duration::from_secs(60),
        });
        async fn media(
            State(manager): State<Arc<PlaybackManager>>,
            Path((id, cap, file)): Path<(String, String, String)>,
            headers: HeaderMap,
        ) -> Response {
            manager
                .serve_original(&id, &cap, &file, Method::GET, headers)
                .await
                .unwrap()
                .unwrap()
        }
        let listener = tokio::net::TcpListener::bind(
            std::env::var("VIPTV_ROKU_BIND").unwrap_or_else(|_| "127.0.0.1:0".into()),
        )
        .await
        .unwrap();
        let proxy_base = format!("http://{}", listener.local_addr().unwrap());
        let catalogue = Arc::new(Mutex::new(Vec::<PlaybackResponse>::new()));
        let published = catalogue.clone();
        let app = Router::new()
            .route(
                "/acceptance.json",
                get(move || {
                    let catalogue = published.clone();
                    async move { axum::Json(catalogue.lock().await.clone()) }
                }),
            )
            .route("/media/:id/:cap/:file", get(media))
            .with_state(manager.clone());
        let proxy = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        for (name, format, position) in [("movie.mp4", "mp4", 2.0), ("live.m3u8", "hls", 0.0)] {
            let started = Instant::now();
            let response = manager
                .start_with_selection(
                    format!("{base}/{name}"),
                    HashMap::new(),
                    position,
                    Some(Capabilities {
                        max_width: 1920,
                        max_height: 1080,
                        direct_play: true,
                        ..Default::default()
                    }),
                    false,
                    false,
                    None,
                    TrackSelection {
                        preferred_audio_language: Some("en".into()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            assert_eq!(response.mode, "direct");
            assert_eq!(response.format, format);
            assert_eq!(response.position, position);
            assert_eq!(response.video_mode, "copy");
            let audio = response
                .selected_audio
                .as_ref()
                .expect("selected AAC track");
            assert_eq!(
                audio.language.as_deref(),
                if format == "mp4" { Some("eng") } else { None },
                "HLS fixture explicitly clears language tags; do not invent them"
            );
            let preparation = started.elapsed().as_millis();
            let mut decode = Command::new(&ffmpeg);
            decode
                .args(["-v", "error", "-ss", "2", "-i"])
                .arg(format!("{proxy_base}{}", response.url))
                .args(["-t", "1", "-f", "null", "-"]);
            let result = timeout(Duration::from_secs(15), decode.output())
                .await
                .unwrap()
                .unwrap();
            assert!(
                result.status.success(),
                "proxied fixture decode failed: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            println!(
                "DIRECT_FIXTURE format={format} prepare_ms={preparation} decode_ms={}",
                started.elapsed().as_millis() - preparation
            );
            if std::env::var("VIPTV_ROKU_BIND").is_ok() {
                catalogue.lock().await.push(response);
            } else {
                assert!(manager.stop(&response.id).await);
            }
        }
        if std::env::var("VIPTV_ROKU_BIND").is_ok() {
            println!("ROKU_FIXTURES_READY");
            for _ in 0..180 {
                for response in catalogue.lock().await.iter() {
                    manager.heartbeat(&response.id).await;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            for response in catalogue.lock().await.iter() {
                manager.stop(&response.id).await;
            }
        }
        assert_eq!(manager.active_count().await, 0);
        manager.shutdown().await;
        origin.abort();
        proxy.abort();
    }
}
