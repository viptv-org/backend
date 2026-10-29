use super::*;
use crate::{
    account_api::{self, Error},
    App, ResourceLease,
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
use zeroize::Zeroize;

fn storage(_: rusqlite::Error) -> Error {
    Error::Code("addon_storage_unavailable")
}
fn own(db: &Connection, account: i64, id: i64) -> Result<(), Error> {
    let exists: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM addons WHERE id=?1 AND account_id=?2)",
            params![id, account],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if exists {
        Ok(())
    } else {
        Err(Error::Code("addon_not_found"))
    }
}
pub(super) fn record(
    db: &Connection,
    vault: Option<&crate::secret_store::Vault>,
    account: i64,
    id: i64,
    fixture: bool,
) -> Result<Value, Error> {
    let (row, manifest) = manifest(db, vault, account, id)?;
    let mut value = row;
    match manifest {
        Ok(manifest) => {
            if let Some(path) = super::logo::path(id, &manifest, fixture) {
                value["logo"] = json!(path);
            }
        }
        Err(code) => {
            value["configuration_error_code"] = json!(code);
            value["configuration_error"] = json!(account_api::description(code));
        }
    }
    Ok(value)
}
fn manifest(
    db: &Connection,
    vault: Option<&crate::secret_store::Vault>,
    account: i64,
    id: i64,
) -> Result<(Value, Result<Value, &'static str>), Error> {
    own(db, account, id)?;
    let (name,url,manifest,enabled,version):(String,String,String,bool,i64)=db.query_row("SELECT substr(name,1,256),manifest_url,manifest,enabled,credentials_version FROM addons WHERE id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).map_err(storage)?;
    let value = json!({"id":id,"name":name,"enabled":enabled,"credentials_encrypted":version==1,"manifest_url":null,"logo":null});
    let manifest = credentials_v2::read(db, vault, account, id, url, manifest, version)
        .map(|(_, manifest)| manifest)
        .map_err(|raw| crate::service_errors::provider(raw).unwrap_or("addon_protocol_invalid"));
    Ok((value, manifest))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Revision {
    v: Option<String>,
}
pub(crate) async fn logo(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<i64>,
    query: Result<Query<Revision>, QueryRejection>,
) -> Result<axum::response::Response, Error> {
    use axum::{http::header, response::IntoResponse};
    let vault = app.secret_vault.clone();
    let fixture = app.addons.fixture_transport();
    let cache = app.addons.logos.clone();
    let named = query
        .map_err(|_| Error::Code("addon_logo_unavailable"))?
        .0
        .v
        .filter(|v| !v.is_empty());
    let (account, current) = account_api::shared(app, lease, move |db, account| {
        let (_, manifest) = manifest(db, vault.as_deref(), account, id)?;
        let source = manifest.ok().and_then(|m| super::logo::source(&m, fixture));
        Ok((account, source.map(|url| (super::logo::revision(id, &url), url))))
    })
    .await?;
    let (revision, source) = current.ok_or(Error::Code("addon_logo_unavailable"))?;
    if named.is_some_and(|named| named != revision) {
        return Err(Error::Code("addon_logo_unavailable"));
    }
    let asset = cache
        .load(account, id, source, fixture)
        .await
        .ok_or(Error::Code("addon_logo_unavailable"))?;
    Ok((
        [
            (header::CONTENT_TYPE, asset.content_type),
            (header::CACHE_CONTROL, "private, max-age=86400"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::CONTENT_SECURITY_POLICY, "default-src 'none'; sandbox"),
        ],
        asset.bytes,
    )
        .into_response())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Registration {
    manifest_url: String,
}
impl Drop for Registration {
    fn drop(&mut self) {
        self.manifest_url.zeroize();
    }
}
pub(crate) async fn create(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    body: Result<Json<Registration>, JsonRejection>,
) -> Result<Json<Value>, Error> {
    let Json(input) = body.map_err(|_| Error::Code("invalid_addon_endpoint"))?;
    let account = account_api::shared(app.clone(), lease.clone(), |_, account| Ok(account)).await?;
    let vault = app
        .secret_vault
        .clone()
        .ok_or(Error::Code("secret_store_not_configured"))?;
    let addons = app
        .addons
        .clone()
        .for_account(account)
        .with_protected_fetch();
    let fixture = addons.fixture_transport();
    if input.manifest_url.len() > 4096 {
        return Err(Error::Code("invalid_addon_endpoint"));
    }
    let url = addons
        .manifest_url(&input.manifest_url)
        .map_err(|e| {
            Error::Code(crate::service_errors::addon(&e).unwrap_or("invalid_addon_endpoint"))
        })?
        .to_string();
    let candidate = url.clone();
    let key = vault.clone();
    let expected = account_api::shared(app.clone(), lease.clone(), move |db, account| {
        Ok(credentials_v2::snapshot(db, &key, account, &candidate)?)
    })
    .await?;
    let (url, manifest) = addons.prepare_manifest(&url).await.map_err(|e| {
        Error::Code(crate::service_errors::addon(&e).unwrap_or("addon_unavailable"))
    })?;
    account_api::shared(app, lease, move |db, account| {
        let id = credentials_v2::store_checked(db, &vault, account, &url, &manifest, &expected)?;
        record(db, Some(&vault), account, id, fixture)
    })
    .await
    .map(Json)
}
fn page_size() -> usize {
    50
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Page {
    cursor: Option<String>,
    #[serde(default = "page_size")]
    limit: usize,
}
pub(crate) async fn list(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    query: Result<Query<Page>, QueryRejection>,
) -> Result<Json<Value>, Error> {
    let Query(q) = query.map_err(|_| Error::Code("invalid_catalog_query"))?;
    if !(1..=200).contains(&q.limit) {
        return Err(Error::Code("invalid_catalog_query"));
    }
    let vault = app.secret_vault.clone();
    let fixture = app.addons.fixture_transport();
    account_api::view(app, lease, move |db, account| {
        let after = if let Some(cursor) = q.cursor {
            if cursor.len() > 256 {
                return Err(Error::Code("invalid_cursor"));
            }
            let (owner, id): (i64, i64) = serde_json::from_slice(
                &URL_SAFE_NO_PAD
                    .decode(cursor)
                    .map_err(|_| Error::Code("invalid_cursor"))?,
            )
            .map_err(|_| Error::Code("invalid_cursor"))?;
            if owner != account || id < 0 {
                return Err(Error::Code("invalid_cursor"));
            }
            id
        } else {
            0
        };
        let mut ids = db
            .prepare("SELECT id FROM addons WHERE account_id=?1 AND id>?2 ORDER BY id LIMIT ?3")
            .map_err(storage)?
            .query_map(params![account, after, q.limit + 1], |r| r.get::<_, i64>(0))
            .map_err(storage)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage)?;
        let more = ids.len() > q.limit;
        ids.truncate(q.limit);
        let next = if more {
            Some(
                URL_SAFE_NO_PAD.encode(
                    serde_json::to_vec(&(account, ids.last()))
                        .map_err(|_| Error::Code("invalid_cursor"))?,
                ),
            )
        } else {
            None
        };
        let items = ids
            .into_iter()
            .map(|id| record(db, vault.as_deref(), account, id, fixture))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(json!({"items":items,"next_cursor":next}))
    })
    .await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Patch {
    enabled: bool,
}
pub(crate) async fn update(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<i64>,
    body: Result<Json<Patch>, JsonRejection>,
) -> Result<Json<Value>, Error> {
    let Json(patch) = body.map_err(|_| Error::Code("invalid_addon_configuration"))?;
    let vault = app.secret_vault.clone();
    let fixture = app.addons.fixture_transport();
    account_api::shared(app, lease, move |db, account| {
        own(db, account, id)?;
        db.execute(
            "UPDATE addons SET enabled=?2 WHERE id=?1 AND account_id=?3",
            params![id, patch.enabled, account],
        )
        .map_err(storage)?;
        record(db, vault.as_deref(), account, id, fixture)
    })
    .await
    .map(Json)
}
pub(crate) async fn delete(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<i64>,
) -> Result<Json<Value>, Error> {
    account_api::shared(app, lease, move |db, account| {
        let tx = db.unchecked_transaction().map_err(storage)?;
        own(&tx, account, id)?;
        tx.execute("DELETE FROM addon_credentials_v2 WHERE addon_id=?1", [id])
            .map_err(storage)?;
        tx.execute(
            "DELETE FROM addons WHERE id=?1 AND account_id=?2",
            params![id, account],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(json!({"ok":true}))
    })
    .await
    .map(Json)
}
