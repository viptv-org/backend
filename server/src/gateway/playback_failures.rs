use super::{control_body, rate::Limiter, Error};
use crate::{
    app_state::{App, ResourceLease},
    gateway::diagnostics::tag,
    ApiError,
};
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension,
};
use serde::Deserialize;
use std::time::{Duration, Instant};

pub(crate) const BODY_LIMIT: usize = 1024;
const REPORTS_PER_WINDOW: u32 = 30;
const WINDOW: Duration = Duration::from_secs(60);
const MAX_POSITION_MS: u64 = 31 * 24 * 60 * 60 * 1000;

#[derive(Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Cause {
    Renderer,
    Undecodable,
    Stall,
    StartTimeout,
    OpenFailed,
}

impl Cause {
    fn name(self) -> &'static str {
        match self {
            Self::Renderer => "renderer",
            Self::Undecodable => "undecodable",
            Self::Stall => "stall",
            Self::StartTimeout => "start_timeout",
            Self::OpenFailed => "open_failed",
        }
    }
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct Report {
    cause: Cause,
    operation: Option<String>,
    status: Option<u16>,
    position_ms: Option<u64>,
    first_frame: bool,
    playback_id: Option<String>,
}

fn bounded(value: &str, max: usize, extra: &[char]) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || extra.contains(&c))
}

fn parse(body: &[u8]) -> Option<Report> {
    let report: Report = serde_json::from_slice(body).ok()?;
    let valid = report
        .operation
        .as_deref()
        .map_or(true, |op| bounded(op, 64, &['_', '.', '-']))
        && report.status.map_or(true, |s| (100..=599).contains(&s))
        && report.position_ms.map_or(true, |p| p <= MAX_POSITION_MS)
        && report
            .playback_id
            .as_deref()
            .map_or(true, |id| bounded(id, 128, &['_', '.', '-', ':']));
    valid.then_some(report)
}

pub(crate) fn limiter() -> Limiter {
    Limiter::new(REPORTS_PER_WINDOW, WINDOW)
}

pub(crate) async fn report(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    request: axum::extract::Request,
) -> Result<Response, Error> {
    let crate::auth::Principal::Account {
        account_id,
        profile_id: Some(profile_id),
        ..
    } = &lease.principal
    else {
        return Err(ApiError(StatusCode::FORBIDDEN, crate::MSG_PROFILE_REQUIRED.into()).into());
    };
    if !app
        .playback_failure_limits
        .admit(&lease.principal.key(), Instant::now())
    {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    let body = control_body(request, BODY_LIMIT)
        .await
        .map_err(|_| Error::Code("invalid_playback_failure"))?;
    let report = parse(&body).ok_or(Error::Code("invalid_playback_failure"))?;
    let session_tag = report
        .playback_id
        .as_deref()
        .and_then(|id| app.gateway_playbacks.snapshot(id, &lease).ok())
        .map(|entry| tag(&entry.id))
        .unwrap_or_default();
    tracing::warn!(
        account_tag = %tag(&account_id.to_string()),
        profile_tag = %tag(&profile_id.to_string()),
        %session_tag,
        cause = report.cause.name(),
        operation = report.operation.as_deref(),
        status = report.status,
        position_ms = report.position_ms,
        first_frame = report.first_frame,
        "Playback failure reported"
    );
    Ok(StatusCode::NO_CONTENT.into_response())
}
