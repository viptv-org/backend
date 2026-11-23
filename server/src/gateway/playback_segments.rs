use super::*;
use viptv_engine::{identity::ExternalId, presentation::Card};

const BUDGET: Duration = Duration::from_secs(8);
const BODY_LIMIT: usize = 64 * 1024;
const LOOKUPS_PER_HOUR: u32 = 120;
const MAX_IDS: usize = 32;

pub(crate) async fn segments(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Error> {
    let entry = app.gateway_playbacks.snapshot(&id, &lease)?;
    validate_entry(&app, &entry).await?;
    let card = entry
        .proof
        .metadata_proof
        .as_ref()
        .map(|proof| proof.card.clone());
    Ok(Json(bounded_answer(&app, &lease, card.as_ref()).await))
}

pub(crate) fn limiter() -> rate::Limiter {
    rate::Limiter::new(LOOKUPS_PER_HOUR, Duration::from_secs(60 * 60))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TitleRequest {
    card: Card,
}

pub(crate) async fn by_title(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    request: axum::extract::Request,
) -> Result<Json<Value>, Error> {
    let crate::auth::Principal::Account {
        account_id,
        profile_id: Some(profile_id),
        ..
    } = lease.principal
    else {
        return Err(ApiError(StatusCode::FORBIDDEN, crate::MSG_PROFILE_REQUIRED.into()).into());
    };
    if !app
        .segment_lookup_limits
        .admit(&format!("{account_id}:{profile_id}"), Instant::now())
    {
        return Err(Error::Code("segments_rate_limited"));
    }
    let body = control_body(request, BODY_LIMIT)
        .await
        .map_err(|_| Error::Code("invalid_segments_request"))?;
    let request: TitleRequest =
        serde_json::from_slice(&body).map_err(|_| Error::Code("invalid_segments_request"))?;
    if !plausible_ids(&request.card) {
        return Err(Error::Code("invalid_segments_request"));
    }
    Ok(Json(bounded_answer(&app, &lease, Some(&request.card)).await))
}

fn plausible_ids(card: &Card) -> bool {
    let ids: Vec<&ExternalId> = card
        .identity
        .ids
        .iter()
        .chain(card.parent_identity.iter().flat_map(|parent| parent.ids.iter()))
        .collect();
    ids.len() <= MAX_IDS
        && ids.iter().all(|id| {
            token(&id.namespace, 32, &['_', '-']) && token(&id.value, 128, &['_', '-', '.', ':'])
        })
}

fn token(value: &str, max: usize, extra: &[char]) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || extra.contains(&c))
}

async fn bounded_answer(app: &App, lease: &ResourceLease, card: Option<&Card>) -> Value {
    tokio::time::timeout(
        BUDGET,
        crate::engine_v3::segments::answer(app, lease, card),
    )
    .await
    .unwrap_or_else(|_| crate::engine_v3::segments::empty())
}
