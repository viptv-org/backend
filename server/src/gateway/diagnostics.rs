use axum::{extract::Request, middleware::Next, response::Response};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    future::Future,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use tracing::Instrument;

static SEQUENCE: AtomicU64 = AtomicU64::new(1);
tokio::task_local! {
    pub(crate) static TRACE: String;
    pub(crate) static STAGES: RefCell<Vec<(&'static str, Duration)>>;
}

pub(crate) async fn timed<T>(stage: &'static str, future: impl Future<Output = T>) -> T {
    let started = Instant::now();
    let output = future.await;
    let elapsed = started.elapsed();
    let _ = STAGES.try_with(|stages| stages.borrow_mut().push((stage, elapsed)));
    output
}

pub(crate) fn timed_sync<T>(stage: &'static str, work: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let output = work();
    let elapsed = started.elapsed();
    let _ = STAGES.try_with(|stages| stages.borrow_mut().push((stage, elapsed)));
    output
}

pub(crate) fn tag(value: &str) -> String {
    Sha256::digest(value.as_bytes())[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn operation(path: &str) -> &'static str {
    let path = path.strip_prefix("/api").unwrap_or(path);
    if path == "/playback" {
        "playback_start"
    } else if path.starts_with("/playback/") && path.ends_with("/heartbeat") {
        "playback_heartbeat"
    } else if path.starts_with("/playback/") {
        "playback_session"
    } else if path.starts_with("/streams") {
        "source_discovery"
    } else if path.starts_with("/meta/") {
        "meta_detail"
    } else {
        "api_other"
    }
}

pub(crate) async fn observe(request: Request, next: Next) -> Response {
    let op = operation(request.uri().path());
    let session = request
        .uri()
        .path()
        .split('/')
        .skip_while(|part| *part != "playback")
        .nth(1)
        .map(tag)
        .unwrap_or_default();
    let method = request.method().as_str().to_owned();
    let trace = format!(
        "{}-{}",
        crate::util::now(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let event_trace = trace.clone();
    let started = Instant::now();
    let span = tracing::info_span!("playback_api", %trace, operation = op, session_tag = %session, %method);
    STAGES
        .scope(
            RefCell::new(Vec::new()),
            async move {
                let response = TRACE
                    .scope(
                        trace,
                        async move {
                            let response = next.run(request).await;
                            let status = response.status().as_u16();
                            let elapsed_ms = started.elapsed().as_millis() as u64;
                            let slowest = STAGES.with(|stages| {
                                stages
                                    .borrow()
                                    .iter()
                                    .max_by_key(|(_, took)| *took)
                                    .map(|(stage, took)| (*stage, took.as_millis() as u64))
                            });
                            if status >= 400 {
                                match slowest {
                                    Some((stage, stage_ms)) => tracing::warn!(operation=op,trace=%event_trace,session_tag=%session,status, elapsed_ms, slowest_stage=stage, slowest_stage_ms=stage_ms, "API request failed"),
                                    None => tracing::warn!(operation=op,trace=%event_trace,session_tag=%session,status, elapsed_ms, "API request failed"),
                                }
                            } else if op != "api_other" {
                                match slowest {
                                    Some((stage, stage_ms)) => tracing::info!(operation=op,trace=%event_trace,session_tag=%session,status, elapsed_ms, slowest_stage=stage, slowest_stage_ms=stage_ms, "API request completed"),
                                    None => tracing::info!(operation=op,trace=%event_trace,session_tag=%session,status, elapsed_ms, "API request completed"),
                                }
                            }
                            response
                        }
                        .instrument(span),
                    )
                    .await;
                response
            },
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Write,
        sync::{Arc, Mutex},
    };
    use tower::ServiceExt;
    use tracing::instrument::WithSubscriber;

    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    #[tokio::test]
    async fn failed_heartbeat_emits_correlated_status_without_payload() {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer = Capture(bytes.clone());
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let app = axum::Router::new()
            .route(
                "/playback/:id/heartbeat",
                axum::routing::post(|| async { axum::http::StatusCode::BAD_GATEWAY }),
            )
            .layer(axum::middleware::from_fn(observe));
        let request = Request::builder()
            .method("POST")
            .uri("/playback/private-token/heartbeat?password=never-print")
            .header("authorization", "Bearer never-print")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = app
            .oneshot(request)
            .with_subscriber(subscriber)
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 502);
        let logs = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        for fact in [
            "playback_heartbeat",
            "502",
            "elapsed_ms",
            "trace",
            "session_tag",
        ] {
            assert!(logs.contains(fact), "missing diagnostic {fact}");
        }
        for secret in ["private-token", "never-print", "authorization", "password="] {
            assert!(!logs.contains(secret));
        }
    }
    #[test]
    fn diagnostic_fields_do_not_include_paths_queries_or_credentials() {
        assert_eq!(
            operation("/api/playback/private-token/heartbeat"),
            "playback_heartbeat"
        );
        assert_eq!(operation("/api/something/private-password"), "api_other");
        let value = tag("private-token");
        assert_eq!(value.len(), 16);
        assert!(!value.contains("private-token"));
    }
}
