//! Account-owned Xtream management. Secrets are write-only, encrypted at creation.
use super::{credentials_v2::Credentials, *};
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
use zeroize::{Zeroize, Zeroizing};

fn enabled() -> bool {
    true
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Input {
    name: String,
    url: String,
    username: String,
    password: String,
    #[serde(default = "enabled")]
    enabled: bool,
    #[serde(default = "enabled")]
    enable_live: bool,
    #[serde(default = "enabled")]
    enable_movies: bool,
    #[serde(default = "enabled")]
    enable_series: bool,
}
impl Drop for Input {
    fn drop(&mut self) {
        self.url.zeroize();
        self.username.zeroize();
        self.password.zeroize();
    }
}
fn storage(_: rusqlite::Error) -> Error {
    Error::Code("provider_storage_unavailable")
}
fn text_valid(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
fn own(db: &Connection, account: i64, id: i64) -> Result<(), Error> {
    let exists:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM providers p JOIN provider_ownership o ON o.provider_id=p.id WHERE p.id=?1 AND o.account_id=?2)",params![id,account],|r|r.get(0)).map_err(storage)?;
    if exists {
        Ok(())
    } else {
        Err(Error::Code("provider_not_found"))
    }
}
fn record(db: &Connection, account: i64, id: i64) -> Result<Value, Error> {
    own(db, account, id)?;
    let mut result=db.query_row("SELECT id,name,enabled,enable_live,enable_movies,enable_series,credentials_version FROM providers WHERE id=?1",[id],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"name":r.get::<_,String>(1)?,"enabled":r.get::<_,bool>(2)?,"enable_live":r.get::<_,bool>(3)?,"enable_movies":r.get::<_,bool>(4)?,"enable_series":r.get::<_,bool>(5)?,"credentials_encrypted":r.get::<_,i64>(6)?==1}))).map_err(storage)?;
    result["refresh"]=refresh_v2::status(db,id)?;
    Ok(result)
}
pub(super) fn managed(db: &Connection, account: i64, id: i64) -> Result<(), Error> {
    own(db, account, id)?;
    let version: i64 = db
        .query_row(
            "SELECT credentials_version FROM providers WHERE id=?1",
            [id],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if version != 1 {
        return Err(Error::Code("provider_encryption_required"));
    }
    Ok(())
}
fn seal(
    vault: &crate::secret_store::Vault,
    account: i64,
    id: i64,
    credentials: &Credentials,
) -> Result<String, Error> {
    let bytes = Zeroizing::new(
        serde_json::to_vec(credentials).map_err(|_| Error::Code("secret_encryption_failed"))?,
    );
    Ok(vault.seal(account, "xtream", &id.to_string(), &bytes)?)
}
pub(super) async fn login(service: &ProviderService, credentials: &Credentials) -> Result<usize, Error> {
    let _permit = service
        .semaphore
        .clone()
        .try_acquire_owned()
        .map_err(|_| Error::Code("provider_checks_busy"))?;
    let mut url = transport_v2::base(&credentials.url, service.fixture_transport())?
        .join("player_api.php")
        .map_err(|_| Error::Code("invalid_provider_endpoint"))?;
    url.query_pairs_mut()
        .append_pair("username", &credentials.username)
        .append_pair("password", &credentials.password);
    let value = service
        .protected_json(url, 256 * 1024)
        .await
        .map_err(|code| provider_error(&code))?;
    let user = &value["user_info"];
    let expiry = user["exp_date"]
        .as_i64()
        .or_else(|| user["exp_date"].as_str().and_then(|s| s.parse().ok()));
    if !(user["auth"] == 1 || user["auth"] == "1" || user["auth"] == true)
        || expiry.is_some_and(|at| at > 0 && at <= crate::util::now())
        || user
            .get("status")
            .is_some_and(|s| s.as_str().is_none_or(|s| !s.eq_ignore_ascii_case("active")))
    {
        return Err(Error::Code("provider_credentials_rejected"));
    }
    let limit = user["max_connections"]
        .as_u64()
        .or_else(|| {
            user["max_connections"]
                .as_str()
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or(0);
    if limit > 1_000_000 {
        return Err(Error::Code("provider_protocol_invalid"));
    }
    Ok(limit as usize)
}
fn provider_error(code: &str) -> Error {
    Error::Code(match code {
        "provider_private_destination" => "provider_private_destination",
        "provider_dns_unavailable" => "provider_dns_unavailable",
        "provider_redirect_rejected" => "provider_redirect_rejected",
        "provider_credentials_rejected" => "provider_credentials_rejected",
        "provider_rate_limited" => "provider_rate_limited",
        "provider_response_too_large" => "provider_response_too_large",
        "provider_protocol_invalid" => "provider_protocol_invalid",
        "provider_timeout" => "provider_timeout",
        _ => "provider_unavailable",
    })
}
pub(crate) async fn create(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    body: Result<Json<Input>, JsonRejection>,
) -> Result<Json<Value>, Error> {
    let Json(value) = body.map_err(|_| Error::Code("invalid_provider_configuration"))?;
    account_api::run(app.clone(), lease.clone(), |_, account| Ok(account)).await?;
    let vault = app
        .secret_vault
        .clone()
        .ok_or(Error::Code("secret_store_not_configured"))?;
    if !text_valid(&value.name, 200)
        || !text_valid(&value.username, 512)
        || !text_valid(&value.password, 2048)
    {
        return Err(Error::Code("invalid_provider_configuration"));
    }
    let base = transport_v2::base(&value.url, app.providers.fixture_transport())?;
    let credentials = Credentials {
        url: base.as_str().trim_end_matches('/').into(),
        username: value.username.clone(),
        password: value.password.clone(),
    };
    let connection_limit = login(&app.providers, &credentials).await?;
    account_api::work(app,lease,move |db,account| {
        let tx=db.unchecked_transaction().map_err(storage)?;
        let count:i64=tx.query_row("SELECT count(*) FROM provider_ownership WHERE account_id=?1",[account],|r|r.get(0)).map_err(storage)?;
        if count>=64 {return Err(Error::Code("too_many_providers"));}
        // Compare decrypted logins only within this account. No cross-tenant
        // duplicate response reveals whether someone else has the subscription.
        let ids=tx.prepare("SELECT p.id FROM providers p JOIN provider_ownership o ON o.provider_id=p.id WHERE o.account_id=?1").map_err(storage)?.query_map([account],|r|r.get::<_,i64>(0)).map_err(storage)?.collect::<rusqlite::Result<Vec<_>>>().map_err(storage)?;
        for id in ids {
            let old=read_credentials(&tx,&vault,account,id)?;
            if old.url.trim_end_matches('/')==credentials.url && old.username==credentials.username {return Err(Error::Code("provider_already_configured"));}
        }
        tx.execute("INSERT INTO providers(name,url,username,password,enabled,enable_live,enable_movies,enable_series,credentials_version,max_connections) VALUES(?1,'','','',?2,?3,?4,?5,1,?6)",params![value.name,value.enabled,value.enable_live,value.enable_movies,value.enable_series,connection_limit]).map_err(storage)?;
        let id=tx.last_insert_rowid();
        tx.execute("INSERT INTO provider_ownership VALUES(?1,?2)",params![id,account]).map_err(storage)?;
        tx.execute("INSERT INTO provider_credentials_v2 VALUES(?1,?2,?3)",params![id,account,seal(&vault,account,id,&credentials)?]).map_err(storage)?;
        v2::live_catalog(&tx,account,None)?;
        refresh_v2::enqueue(&tx,account,id,false)?;
        let result=record(&tx,account,id)?;
        tx.commit().map_err(storage)?;
        Ok(result)
    }).await
}
fn read_credentials(
    db: &Connection,
    vault: &crate::secret_store::Vault,
    account: i64,
    id: i64,
) -> Result<Credentials, Error> {
    own(db, account, id)?;
    let mut provider = db
        .query_row(
            "SELECT name,url,username,password FROM providers WHERE id=?1",
            [id],
            |r| {
                Ok(Provider {
                    id,
                    name: r.get(0)?,
                    url: r.get(1)?,
                    username: r.get(2)?,
                    password: r.get(3)?,
                    sealed: None,
                })
            },
        )
        .map_err(storage)?;
    credentials_v2::read(db, Some(vault), &mut provider)
        .map_err(|_| Error::Code("secret_authentication_failed"))?;
    Ok(Credentials {
        url: provider.url,
        username: provider.username,
        password: provider.password,
    })
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PageQuery {
    cursor: Option<String>,
    #[serde(default = "page_size")]
    limit: usize,
}
fn page_size() -> usize {
    50
}
pub(crate) async fn list(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    query: Result<Query<PageQuery>, QueryRejection>,
) -> Result<Json<Value>, Error> {
    let Query(q) = query.map_err(|_| Error::Code("invalid_catalog_query"))?;
    if !(1..=200).contains(&q.limit) {
        return Err(Error::Code("invalid_catalog_query"));
    }
    account_api::work(app,lease,move |db,account| {
        let after=if let Some(cursor)=q.cursor {if cursor.len()>256 {return Err(Error::Code("invalid_cursor"));} let (owner,id):(i64,i64)=serde_json::from_slice(&URL_SAFE_NO_PAD.decode(cursor).map_err(|_|Error::Code("invalid_cursor"))?).map_err(|_|Error::Code("invalid_cursor"))?;if owner!=account || id<0 {return Err(Error::Code("invalid_cursor"));}id} else {0};
        let mut ids=db.prepare("SELECT provider_id FROM provider_ownership WHERE account_id=?1 AND provider_id>?2 ORDER BY provider_id LIMIT ?3").map_err(storage)?.query_map(params![account,after,q.limit+1],|r|r.get::<_,i64>(0)).map_err(storage)?.collect::<rusqlite::Result<Vec<_>>>().map_err(storage)?;
        let more=ids.len()>q.limit;ids.truncate(q.limit);
        let cursor=if more {Some(URL_SAFE_NO_PAD.encode(serde_json::to_vec(&(account,ids.last())).map_err(|_|Error::Code("invalid_cursor"))?))}else{None};
        let items=ids.into_iter().map(|id|record(db,account,id)).collect::<Result<Vec<_>,_>>()?;
        Ok(json!({"items":items,"next_cursor":cursor}))
    }).await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Patch {
    name: Option<String>,
    enabled: Option<bool>,
    enable_live: Option<bool>,
    enable_movies: Option<bool>,
    enable_series: Option<bool>,
}
pub(crate) async fn update(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<i64>,
    body: Result<Json<Patch>, JsonRejection>,
) -> Result<Json<Value>, Error> {
    let Json(p) = body.map_err(|_| Error::Code("invalid_provider_configuration"))?;
    if p.name.as_ref().is_some_and(|n| !text_valid(n, 200))
        || (p.name.is_none()
            && p.enabled.is_none()
            && p.enable_live.is_none()
            && p.enable_movies.is_none()
            && p.enable_series.is_none())
    {
        return Err(Error::Code("invalid_provider_configuration"));
    }
    account_api::work(app,lease,move |db,account| {
        let tx=db.unchecked_transaction().map_err(storage)?;
        managed(&tx,account,id)?;
        tx.execute("UPDATE providers SET name=COALESCE(?2,name),enabled=COALESCE(?3,enabled),enable_live=COALESCE(?4,enable_live),enable_movies=COALESCE(?5,enable_movies),enable_series=COALESCE(?6,enable_series) WHERE id=?1",params![id,p.name,p.enabled,p.enable_live,p.enable_movies,p.enable_series]).map_err(storage)?;
        v2::live_catalog(&tx,account,None)?;
        if p.enabled.is_some() || p.enable_live.is_some() || p.enable_movies.is_some() || p.enable_series.is_some() {refresh_v2::enqueue(&tx,account,id,true)?;}
        let result=record(&tx,account,id)?;tx.commit().map_err(storage)?;Ok(result)
    }).await
}
pub(crate) async fn delete(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<i64>,
) -> Result<Json<Value>, Error> {
    let gates = app.providers.playback_gates.clone();
    account_api::work(app, lease, move |db, account| {
        let tx = db.unchecked_transaction().map_err(storage)?;
        managed(&tx, account, id)?;
        tx.execute("DELETE FROM provider_pools WHERE provider_id=?1", [id])
            .map_err(storage)?;
        tx.execute("DELETE FROM providers WHERE id=?1", [id])
            .map_err(storage)?;
        v2::live_catalog(&tx, account, None)?;
        tx.commit().map_err(storage)?;
        // Existing permits retain their Arc until playback cleanup, but a deleted
        // connection must not leave an unbounded registry of idle gates.
        if let Ok(mut gates) = gates.lock() {
            gates.remove(&-id);
        }
        Ok(json!({"ok":true}))
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Renewal {
    password: String,
}
impl Drop for Renewal {
    fn drop(&mut self) {
        self.password.zeroize();
    }
}
pub(crate) async fn renew(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<i64>,
    body: Result<Json<Renewal>, JsonRejection>,
) -> Result<Json<Value>, Error> {
    let Json(value) = body.map_err(|_| Error::Code("invalid_provider_configuration"))?;
    if !text_valid(&value.password, 2048) {
        return Err(Error::Code("invalid_provider_configuration"));
    }
    account_api::run(app.clone(), lease.clone(), |_, account| Ok(account)).await?;
    let vault = app
        .secret_vault
        .clone()
        .ok_or(Error::Code("secret_store_not_configured"))?;
    let reading = vault.clone();
    let (mut credentials, prior) =
        account_api::run(app.clone(), lease.clone(), move |db, account| {
            managed(db, account, id)?;
            let prior: String = db
                .query_row(
                    "SELECT secret FROM provider_credentials_v2 WHERE provider_id=?1",
                    [id],
                    |r| r.get(0),
                )
                .map_err(storage)?;
            Ok((read_credentials(db, &reading, account, id)?, prior))
        })
        .await?;
    credentials.password.zeroize();
    credentials.password = value.password.clone();
    let limit = login(&app.providers, &credentials).await?;
    account_api::work(app,lease,move |db,account| {
        let tx=db.unchecked_transaction().map_err(storage)?;managed(&tx,account,id)?;
        let changed=tx.execute("UPDATE provider_credentials_v2 SET secret=?3 WHERE provider_id=?1 AND account_id=?2 AND secret=?4",params![id,account,seal(&vault,account,id,&credentials)?,prior]).map_err(storage)?;
        if changed!=1 {return Err(Error::Code("source_configuration_changed"));}
        tx.execute("UPDATE providers SET max_connections=?2 WHERE id=?1",params![id,limit]).map_err(storage)?;
        tx.execute("DELETE FROM provider_cache WHERE provider_id=?1",[id]).map_err(storage)?;
        refresh_v2::enqueue(&tx,account,id,true)?;
        let result=record(&tx,account,id)?;tx.commit().map_err(storage)?;Ok(result)
    }).await
}
