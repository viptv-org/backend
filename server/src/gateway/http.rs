use super::registry::{self, Registration};
use crate::{
    account_api::{self, Error},
    app_state::{App, ResourceLease},
};
use axum::{
    extract::{rejection::JsonRejection, Path, State},
    Extension, Json,
};
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
        Ok(json!({"ready":capability.ready,"version":capability.version}))
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
