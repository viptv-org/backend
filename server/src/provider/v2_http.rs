//! Account settings endpoints, independent of a selected viewing profile.
//! Unassigned legacy sources are never inferred to belong to the caller.
use super::v2;
use crate::{
    app_state::{App, ResourceLease},
    auth, kids, ApiError,
};
use axum::{
    extract::{
        rejection::{JsonRejection, QueryRejection},
        Query, State,
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

pub(crate) enum Error {
    Auth(ApiError),
    Code(&'static str),
}
impl From<&'static str> for Error {
    fn from(code: &'static str) -> Self {
        Self::Code(code)
    }
}
impl From<ApiError> for Error {
    fn from(error: ApiError) -> Self {
        Self::Auth(error)
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let code = match self {
            Self::Auth(error) => return error.into_response(),
            Self::Code(code) => code,
        };
        let (status, message) = match code {
            "invalid_cursor" => (
                StatusCode::BAD_REQUEST,
                "This page token is no longer valid. Reload the list.",
            ),
            "invalid_matches_query" => (
                StatusCode::BAD_REQUEST,
                "Check the IPTV filters and choose a page size from 1 to 200.",
            ),
            "invalid_match_request" => (
                StatusCode::BAD_REQUEST,
                "Choose a valid movie or series match.",
            ),
            "catalog_unavailable" => (
                StatusCode::NOT_FOUND,
                "This live playlist is unavailable in your account.",
            ),
            "stream_candidate_not_found" => (
                StatusCode::NOT_FOUND,
                "This stream is unavailable in your IPTV accounts.",
            ),
            "account_session_required" => (
                StatusCode::FORBIDDEN,
                "Manage IPTV settings from an account session, not a paired TV.",
            ),
            _ => (
                StatusCode::SERVICE_UNAVAILABLE,
                "IPTV data is temporarily unavailable. Try again.",
            ),
        };
        (status, Json(json!({"error":message,"error_code":code}))).into_response()
    }
}
async fn work(
    app: App,
    lease: ResourceLease,
    action: impl FnOnce(&rusqlite::Connection, i64) -> Result<Value, Error> + Send + 'static,
) -> Result<Json<Value>, Error> {
    tokio::task::spawn_blocking(move || {
        let db = app
            .db
            .lock()
            .map_err(|_| Error::Code("provider_storage_unavailable"))?;
        lease.validate(&db)?;
        if matches!(&lease.principal, auth::Principal::Account { role, .. } if role == "device") {
            return Err(Error::Code("account_session_required"));
        }
        kids::require_parent(&db, &lease.principal)?;
        action(
            &db,
            lease
                .principal
                .account_id()
                .ok_or_else(auth::unauthorized)?,
        )
    })
    .await
    .map_err(|_| Error::Code("provider_storage_unavailable"))?
    .map(Json)
}
fn page_size() -> usize {
    50
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MatchesQuery {
    provider_id: Option<i64>,
    kind: Option<String>,
    #[serde(default)]
    search: String,
    cursor: Option<String>,
    #[serde(default = "page_size")]
    limit: usize,
}
pub(crate) async fn matches(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    query: Result<Query<MatchesQuery>, QueryRejection>,
) -> Result<Json<Value>, Error> {
    let Query(q) = query.map_err(|_| Error::Code("invalid_matches_query"))?;
    work(app, lease, move |db, account| {
        let page = v2::matches_page(
            db,
            account,
            v2::MatchFilter {
                provider_id: q.provider_id,
                kind: q.kind,
                search: q.search,
            },
            q.cursor.as_deref(),
            q.limit,
        )?;
        Ok(json!({"items":page.items,"next_cursor":page.next_cursor}))
    })
    .await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MatchRequest {
    vod_id: String,
    metadata_id: String,
    #[serde(rename = "type")]
    kind: String,
}
pub(crate) async fn override_match(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    body: Result<Json<MatchRequest>, JsonRejection>,
) -> Result<Json<Value>, Error> {
    let Json(value) = body.map_err(|_| Error::Code("invalid_match_request"))?;
    work(app, lease, move |db, account| {
        v2::override_match(db, account, &value.vod_id, &value.metadata_id, &value.kind)?;
        Ok(json!({"ok":true}))
    })
    .await
}
pub(crate) async fn live_default(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
) -> Result<Json<Value>, Error> {
    work(app, lease, |db, account| {
        Ok(json!({"catalog_id":v2::live_catalog(db, account, None)?}))
    })
    .await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DefaultRequest {
    catalog_id: i64,
}
pub(crate) async fn set_live_default(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    body: Result<Json<DefaultRequest>, JsonRejection>,
) -> Result<Json<Value>, Error> {
    let Json(value) = body.map_err(|_| Error::Code("catalog_unavailable"))?;
    work(app, lease, move |db, account| {
        v2::set_live_default(db, account, value.catalog_id)?;
        Ok(json!({"catalog_id":value.catalog_id}))
    })
    .await
}
