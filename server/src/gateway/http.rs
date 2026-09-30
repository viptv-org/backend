use super::registry::{self, Registration};
use crate::{
    account_api::{self, Error},
    app_state::{App, ResourceLease},
};
use axum::{
    extract::{
        rejection::{JsonRejection, QueryRejection},
        Path, Query, State,
    },
    Extension, Json,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::Deserialize;
use serde_json::{json, Value};

pub(crate) async fn list(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
) -> Result<Json<Value>, Error> {
    let configured = app.secret_vault.is_some();
    account_api::work(app, lease, move |db, account| {
        Ok(json!({"items":registry::list(db,account)?,"secret_storage_configured":configured}))
    })
    .await
}
pub(crate) async fn register(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    body: Result<Json<Registration>, JsonRejection>,
) -> Result<Json<Value>, Error> {
    let Json(value) = body.map_err(|_| Error::Code("invalid_gateway_configuration"))?;
    account_api::run(app.clone(), lease.clone(), |_, account| Ok(account)).await?;
    let vault = app
        .secret_vault
        .clone()
        .ok_or(Error::Code("secret_store_not_configured"))?;
    value.validate()?;
    app.gateway_client
        .capabilities(
            &value.endpoint,
            value.integration_key.as_bytes(),
            &value.namespace,
        )
        .await?;
    account_api::work(app, lease, move |db, account| {
        Ok(json!(registry::register(db, &vault, account, value)?))
    })
    .await
}
pub(crate) async fn check(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Error> {
    account_api::run(app.clone(), lease.clone(), |_, account| Ok(account)).await?;
    let vault = app
        .secret_vault
        .clone()
        .ok_or(Error::Code("secret_store_not_configured"))?;
    let target = account_api::run(app.clone(), lease.clone(), move |db, account| {
        Ok(registry::authorized(db, &vault, account, &id)?)
    })
    .await?;
    let capability = app
        .gateway_client
        .capabilities(
            &target.gateway.endpoint,
            target.key.expose(),
            &target.gateway.namespace,
        )
        .await?;
    // Do not return stale success after the account/session was revoked while
    // the gateway request was in flight.
    account_api::work(app, lease, move |db, account| {
        if !registry::list(db, account)?.iter().any(|current| {
            current.id == target.gateway.id
                && current.enabled
                && current.revision == target.gateway.revision
        }) {
            return Err(Error::Code("gateway_not_found"));
        }
        Ok(json!({"ready":capability.ready,"version":capability.version,"available":capability.available}))
    })
    .await
}
pub(crate) async fn replace(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
    body: Result<Json<Registration>, JsonRejection>,
) -> Result<Json<Value>, Error> {
    let Json(value) = body.map_err(|_| Error::Code("invalid_gateway_configuration"))?;
    let checked_id = id.clone();
    account_api::run(app.clone(), lease.clone(), move |db, account| {
        if !registry::list(db, account)?
            .iter()
            .any(|gateway| gateway.id == checked_id && gateway.can_manage)
        {
            return Err(Error::Code("gateway_not_found"));
        }
        Ok(())
    })
    .await?;
    let vault = app
        .secret_vault
        .clone()
        .ok_or(Error::Code("secret_store_not_configured"))?;
    value.validate()?;
    app.gateway_client
        .capabilities(
            &value.endpoint,
            value.integration_key.as_bytes(),
            &value.namespace,
        )
        .await?;
    account_api::work(app, lease, move |db, account| {
        Ok(json!(registry::replace(db, &vault, account, &id, value)?))
    })
    .await
}
pub(crate) async fn update(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
    body: Result<Json<registry::Patch>, JsonRejection>,
) -> Result<Json<Value>, Error> {
    let Json(value) = body.map_err(|_| Error::Code("invalid_gateway_configuration"))?;
    account_api::work(app, lease, move |db, account| {
        registry::update(db, account, &id, value)?;
        Ok(json!({"ok":true}))
    })
    .await
}
pub(crate) async fn delete(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Error> {
    account_api::work(app, lease, move |db, account| {
        registry::delete(db, account, &id)?;
        Ok(json!({"ok":true}))
    })
    .await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Grant {
    account_id: i64,
    enabled: bool,
}
fn page_size() -> usize {
    50
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GrantsQuery {
    cursor: Option<String>,
    #[serde(default = "page_size")]
    limit: usize,
}
pub(crate) async fn grants(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
    query: Result<Query<GrantsQuery>, QueryRejection>,
) -> Result<Json<Value>, Error> {
    let Query(query) = query.map_err(|_| Error::Code("invalid_catalog_query"))?;
    if !(1..=200).contains(&query.limit) {
        return Err(Error::Code("invalid_catalog_query"));
    }
    let principal = lease.principal.clone();
    account_api::work(app, lease, move |db, owner| {
        principal.require_owner()?;
        let after = if let Some(cursor) = query.cursor {
            if cursor.len() > 512 { return Err(Error::Code("invalid_cursor")); }
            let (account, gateway, after): (i64, String, i64) = serde_json::from_slice(
                &URL_SAFE_NO_PAD.decode(cursor).map_err(|_| Error::Code("invalid_cursor"))?
            ).map_err(|_| Error::Code("invalid_cursor"))?;
            if account != owner || gateway != id || after < 0 {
                return Err(Error::Code("invalid_cursor"));
            }
            after
        } else { 0 };
        let mut recipients = registry::grants(db, owner, &id, after, query.limit + 1)?;
        let more = recipients.len() > query.limit;
        recipients.truncate(query.limit);
        let next = if more {
            Some(URL_SAFE_NO_PAD.encode(serde_json::to_vec(&(owner, &id, recipients.last()))
                .map_err(|_| Error::Code("invalid_cursor"))?))
        } else { None };
        Ok(json!({"items":recipients.into_iter().map(|account| json!({"account_id":account,"enabled":true})).collect::<Vec<_>>(),"next_cursor":next}))
    }).await
}
pub(crate) async fn grant(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
    body: Result<Json<Grant>, JsonRejection>,
) -> Result<Json<Value>, Error> {
    let Json(value) = body.map_err(|_| Error::Code("invalid_gateway_configuration"))?;
    let principal = lease.principal.clone();
    account_api::work(app, lease, move |db, owner| {
        principal.require_owner()?;
        registry::grant(db, owner, &id, value.account_id, value.enabled)?;
        Ok(json!({"ok":true}))
    })
    .await
}
