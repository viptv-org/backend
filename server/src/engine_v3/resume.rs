use super::*;
use std::time::{Duration, Instant};
use viptv_engine::presentation::Card;

const LOOKUPS_PER_HOUR: u32 = 120;

pub(crate) fn limiter() -> gateway::playback::rate::Limiter {
    gateway::playback::rate::Limiter::new(LOOKUPS_PER_HOUR, Duration::from_secs(60 * 60))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    card: Card,
}

pub(crate) async fn lookup(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    axum::Json(request): axum::Json<Request>,
) -> ApiResult {
    let crate::auth::Principal::Account {
        account_id,
        profile_id: Some(profile),
        ..
    } = lease.principal.clone()
    else {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            crate::MSG_PROFILE_REQUIRED.into(),
        ));
    };
    if !app
        .resume_lookup_limits
        .admit(&format!("{account_id}:{profile}"), Instant::now())
    {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            crate::MSG_RESUME_RATE_LIMITED.into(),
        ));
    }
    let db = app.db.lock().map_err(|_| {
        ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "Database unavailable".into(),
        )
    })?;
    let scoped = app.clone().with_lease(lease);
    let outcome =
        super::scoped_library::resume_outcome_checked_db(&db, &scoped, profile, &request.card)?;
    Ok(axum::Json(json!({
        "position_seconds": outcome.position,
        "reason": outcome.reason,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_admits_one_hundred_and_twenty_lookups_per_profile_per_hour() {
        let limiter = limiter();
        let now = Instant::now();
        for _ in 0..LOOKUPS_PER_HOUR {
            assert!(limiter.admit("1:1", now));
        }
        assert!(!limiter.admit("1:1", now));
        assert!(limiter.admit("1:2", now));
    }
}
