//! Native metainfo only: ten seconds across DNS, pinned connections, redirects and body.
//! The HTTP/1 parser bounds its header buffer before allocation; no proxy or cookie jar.
use crate::account_api::Error;
use http_body_util::{BodyExt, Empty};
use hyper::{body::Bytes, Request};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::io::{AsyncRead, AsyncWrite};
use url::{Host, Url};

const BODY: usize = 4_194_304;
const HEADERS: usize = 32768;
fn invalid() -> Error {
    Error::Code("native_metainfo_invalid")
}
trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
struct Connection(tokio::task::JoinHandle<()>);
impl Drop for Connection {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(super) async fn fetch(
    target: &str,
    headers: &BTreeMap<String, String>,
) -> Result<Vec<u8>, Error> {
    fetch_with_policy(target, headers, false).await
}
async fn fetch_with_policy(
    raw: &str,
    headers: &BTreeMap<String, String>,
    fixture: bool,
) -> Result<Vec<u8>, Error> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut target = Url::parse(raw).map_err(|_| invalid())?;
        let origin = target.origin();
        for hop in 0..=3 {
            crate::source_http::validate(&target, fixture).map_err(|_| invalid())?;
            let host = match target.host().ok_or_else(invalid)? {
                Host::Domain(value) => value.to_owned(),
                Host::Ipv4(value) => value.to_string(),
                Host::Ipv6(value) => value.to_string(),
            };
            let port = target.port_or_known_default().ok_or_else(invalid)?;
            let addresses = tokio::net::lookup_host((host.as_str(), port))
                .await
                .map_err(|_| invalid())?
                .take(17)
                .collect::<Vec<_>>();
            if addresses.is_empty()
                || addresses.len() > 16
                || addresses.iter().any(|a| {
                    !torrent_policy::network::globally_routable(a.ip())
                        && !(fixture && a.ip().is_loopback())
                })
            {
                return Err(invalid());
            }
            // Connection uses only the already vetted addresses; no second DNS resolution.
            let socket = tokio::net::TcpStream::connect(addresses.as_slice())
                .await
                .map_err(|_| invalid())?;
            let stream: Box<dyn Io> = if target.scheme() == "https" {
                let roots = tokio_rustls::rustls::RootCertStore::from_iter(
                    webpki_roots::TLS_SERVER_ROOTS.iter().cloned(),
                );
                let config = tokio_rustls::rustls::ClientConfig::builder_with_provider(Arc::new(
                    tokio_rustls::rustls::crypto::ring::default_provider(),
                ))
                .with_safe_default_protocol_versions()
                .map_err(|_| invalid())?
                .with_root_certificates(roots)
                .with_no_client_auth();
                let name = tokio_rustls::rustls::pki_types::ServerName::try_from(host.clone())
                    .map_err(|_| invalid())?;
                Box::new(
                    tokio_rustls::TlsConnector::from(Arc::new(config))
                        .connect(name, socket)
                        .await
                        .map_err(|_| invalid())?,
                )
            } else {
                Box::new(socket)
            };
            let (mut sender, connection) = hyper::client::conn::http1::Builder::new()
                .max_buf_size(HEADERS)
                .handshake(hyper_util::rt::TokioIo::new(stream))
                .await
                .map_err(|_| invalid())?;
            let _connection = Connection(tokio::spawn(async move {
                let _ = connection.await;
            }));
            let path = match target.query() {
                Some(q) => format!("{}?{q}", target.path()),
                None => target.path().to_owned(),
            };
            let authority = &target[url::Position::BeforeHost..url::Position::AfterPort];
            let mut request = Request::builder()
                .method("GET")
                .uri(path)
                .header("host", authority)
                .header("accept-encoding", "identity")
                .header("connection", "close");
            let mut budget = 128 + authority.len();
            if target.origin() == origin {
                for (key, value) in headers {
                    budget += key.len() + value.len() + 4;
                    if budget > HEADERS {
                        return Err(invalid());
                    }
                    if !matches!(key.as_str(), "host" | "accept-encoding" | "connection") {
                        let mut header =
                            hyper::header::HeaderValue::from_str(value).map_err(|_| invalid())?;
                        header.set_sensitive(true);
                        request = request.header(key, header);
                    }
                }
            }
            let response = sender
                .send_request(request.body(Empty::<Bytes>::new()).map_err(|_| invalid())?)
                .await
                .map_err(|_| invalid())?;
            let response_headers = response.headers();
            if response_headers
                .iter()
                .map(|(key, value)| key.as_str().len() + value.as_bytes().len() + 4)
                .sum::<usize>()
                > HEADERS
                || response_headers
                    .get_all("content-encoding")
                    .iter()
                    .any(|value| value != "identity")
            {
                return Err(invalid());
            }
            if matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
                if hop == 3 {
                    return Err(invalid());
                }
                let location = response_headers
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(invalid)?;
                let next = target.join(location).map_err(|_| invalid())?;
                if target.scheme() == "https" && next.scheme() != "https" {
                    return Err(invalid());
                }
                target = next;
                continue;
            }
            if !response.status().is_success()
                || response_headers
                    .get("content-length")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .is_some_and(|n| n > BODY as u64)
            {
                return Err(invalid());
            }
            let mut body = response.into_body();
            let mut data = Vec::new();
            while let Some(frame) = body.frame().await {
                let frame = frame.map_err(|_| invalid())?;
                if let Some(bytes) = frame.data_ref() {
                    if data.len().saturating_add(bytes.len()) > BODY {
                        return Err(invalid());
                    }
                    data.extend_from_slice(bytes);
                } else if frame.is_trailers() {
                    return Err(invalid());
                }
            }
            return Ok(data);
        }
        Err(invalid())
    })
    .await
    .map_err(|_| Error::Code("native_metainfo_timeout"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn identity_headers_body_redirects_and_credentials_are_bounded() {
        use axum::{routing::get, Router};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route(
                        "/ok",
                        get(|headers: axum::http::HeaderMap| async move {
                            assert_eq!(headers["accept-encoding"], "identity");
                            b"fixture".to_vec()
                        }),
                    )
                    .route(
                        "/compressed",
                        get(|| async { ([("content-encoding", "gzip")], "private") }),
                    )
                    .route(
                        "/headers",
                        get(|| async { ([("x-fixture", "x".repeat(32768))], "private") }),
                    )
                    .route("/large", get(|| async { vec![0; BODY + 1] }))
                    .route(
                        "/loop",
                        get(|| async {
                            (axum::http::StatusCode::FOUND, [("location", "/loop")], "")
                        }),
                    ),
            )
            .await
            .unwrap();
        });
        assert_eq!(
            fetch_with_policy(&format!("{base}/ok"), &BTreeMap::new(), true)
                .await
                .unwrap_or_else(|_| panic!("metainfo fixture failed")),
            b"fixture"
        );
        for path in ["compressed", "headers", "large", "loop"] {
            assert!(
                fetch_with_policy(&format!("{base}/{path}"), &BTreeMap::new(), true)
                    .await
                    .is_err()
            );
        }
        assert!(fetch(&format!("{base}/ok"), &BTreeMap::new())
            .await
            .is_err());
        task.abort();
        let _ = task.await;
    }

    #[tokio::test]
    async fn cross_origin_metainfo_redirect_never_forwards_source_credentials() {
        use axum::{routing::get, Router};
        let destination = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let final_url = format!("http://{}/final", destination.local_addr().unwrap());
        let receiving = tokio::spawn(async move {
            axum::serve(
                destination,
                Router::new().route(
                    "/final",
                    get(|headers: axum::http::HeaderMap| async move {
                        for key in ["authorization", "cookie", "x-api-key", "referer", "origin"] {
                            assert!(headers.get(key).is_none());
                        }
                        b"fixture".to_vec()
                    }),
                ),
            )
            .await
            .unwrap();
        });
        let initial = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let initial_url = format!("http://{}/initial", initial.local_addr().unwrap());
        let redirecting = tokio::spawn(async move {
            axum::serve(
                initial,
                Router::new().route(
                    "/initial",
                    get(move |headers: axum::http::HeaderMap| {
                        let target = final_url.clone();
                        async move {
                            assert_eq!(headers["authorization"], "Bearer synthetic");
                            (axum::http::StatusCode::FOUND, [("location", target)], "")
                        }
                    }),
                ),
            )
            .await
            .unwrap();
        });
        let headers = BTreeMap::from([
            ("authorization".into(), "Bearer synthetic".into()),
            ("cookie".into(), "synthetic=fixture".into()),
            ("x-api-key".into(), "synthetic".into()),
        ]);
        assert_eq!(
            fetch_with_policy(&initial_url, &headers, true)
                .await
                .unwrap_or_else(|_| panic!("metainfo fixture failed")),
            b"fixture"
        );
        receiving.abort();
        redirecting.abort();
        let _ = receiving.await;
        let _ = redirecting.await;
    }

    #[tokio::test]
    async fn stalled_response_observes_one_ten_second_total_deadline() {
        use axum::{routing::get, Router};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/stall", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/stall",
                    get(|| async { std::future::pending::<Vec<u8>>().await }),
                ),
            )
            .await
            .unwrap();
        });
        let began = std::time::Instant::now();
        assert!(matches!(
            fetch_with_policy(&url, &BTreeMap::new(), true).await,
            Err(Error::Code("native_metainfo_timeout"))
        ));
        assert!(began.elapsed() >= Duration::from_secs(9));
        assert!(began.elapsed() < Duration::from_secs(12));
        task.abort();
        let _ = task.await;
    }
}
