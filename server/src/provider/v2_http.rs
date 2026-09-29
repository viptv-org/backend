//! Account settings endpoints, independent of a selected viewing profile.
//! Unassigned legacy sources are never inferred to belong to the caller.
use super::v2;
use crate::{
    account_api::{work, Error},
    app_state::{App, ResourceLease},
};
use axum::{
    extract::{
        rejection::{JsonRejection, QueryRejection},
        Query, State,
    },
    Extension, Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

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
