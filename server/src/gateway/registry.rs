//! No server-global default or implicit public grants. Every gateway has an
//! account owner; other accounts require an explicit recorded grant.
use crate::secret_store::{SecretBytes, Vault};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::fmt;
use zeroize::Zeroizing;

type Result<T> = std::result::Result<T, &'static str>;
pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS playback_gateways(
        id TEXT PRIMARY KEY, owner_account_id INTEGER NOT NULL REFERENCES auth_accounts(id) ON DELETE CASCADE,
        name TEXT NOT NULL, endpoint TEXT NOT NULL, namespace TEXT NOT NULL,
        priority INTEGER NOT NULL DEFAULT 100, enabled INTEGER NOT NULL DEFAULT 1,
        secret TEXT NOT NULL CHECK(length(secret)<=400000), revision INTEGER NOT NULL DEFAULT 1);
        CREATE INDEX IF NOT EXISTS playback_gateways_owner ON playback_gateways(owner_account_id,id);
        CREATE UNIQUE INDEX IF NOT EXISTS playback_gateways_identity ON playback_gateways(owner_account_id,endpoint,namespace);
        CREATE TABLE IF NOT EXISTS playback_gateway_grants(
        gateway_id TEXT NOT NULL REFERENCES playback_gateways(id) ON DELETE CASCADE,
        account_id INTEGER NOT NULL REFERENCES auth_accounts(id) ON DELETE CASCADE,
        PRIMARY KEY(gateway_id,account_id));
        CREATE INDEX IF NOT EXISTS playback_gateway_grants_account ON playback_gateway_grants(account_id,gateway_id);")
}

fn secret<'de, D: serde::Deserializer<'de>>(
    input: D,
) -> std::result::Result<Zeroizing<String>, D::Error> {
    String::deserialize(input).map(Zeroizing::new)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Registration {
    pub name: String,
    pub endpoint: String,
    pub namespace: String,
    #[serde(default = "priority")]
    pub priority: i64,
    #[serde(deserialize_with = "secret")]
    pub integration_key: Zeroizing<String>,
}
fn priority() -> i64 {
    100
}
impl fmt::Debug for Registration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Registration(<redacted>)")
    }
}
impl Registration {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.integration_key.len() != 68
            || !self.integration_key.starts_with("pgk_")
            || !self.integration_key.as_bytes()[4..]
                .iter()
                .all(u8::is_ascii_hexdigit)
        {
            return Err("invalid_gateway_key");
        }
        if self.name.trim().is_empty()
            || self.name.len() > 128
            || self.name.chars().any(char::is_control)
            || !(0..=10000).contains(&self.priority)
            || self.namespace.is_empty()
            || self.namespace.len() > 128
            || !self
                .namespace
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':'))
        {
            return Err("invalid_gateway_configuration");
        }
        super::client::endpoint(&self.endpoint)?;
        Ok(())
    }
}

#[derive(Clone, Serialize)]
pub(crate) struct Gateway {
    pub id: String,
    pub name: String,
    pub endpoint: String,
    pub namespace: String,
    pub priority: i64,
    pub enabled: bool,
    pub revision: i64,
    pub can_manage: bool,
}
impl fmt::Debug for Gateway {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Gateway")
            .field("id", &self.id)
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Patch {
    pub name: Option<String>,
    pub priority: Option<i64>,
    pub enabled: Option<bool>,
}
pub(crate) struct AuthorizedGateway {
    pub gateway: Gateway,
    pub key: SecretBytes,
}
impl fmt::Debug for AuthorizedGateway {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AuthorizedGateway(<redacted>)")
    }
}
const ALLOWED: &str = "(g.owner_account_id=?1 OR EXISTS(SELECT 1 FROM playback_gateway_grants x WHERE x.gateway_id=g.id AND x.account_id=?1))";

pub(crate) fn list(db: &Connection, account: i64) -> Result<Vec<Gateway>> {
    let mut statement=db.prepare(&format!("SELECT g.id,g.name,g.endpoint,g.namespace,g.priority,g.enabled,g.revision,g.owner_account_id=?1 FROM playback_gateways g WHERE {ALLOWED} ORDER BY g.priority,g.id LIMIT 65")).map_err(|_|"gateway_storage_unavailable")?;
    let rows = statement
        .query_map([account], |row| {
            Ok(Gateway {
                id: row.get(0)?,
                name: row.get(1)?,
                endpoint: row.get(2)?,
                namespace: row.get(3)?,
                priority: row.get(4)?,
                enabled: row.get(5)?,
                revision: row.get(6)?,
                can_manage: row.get(7)?,
            })
        })
        .map_err(|_| "gateway_storage_unavailable")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|_| "gateway_storage_unavailable")?;
    if rows.len() > 64 {
        return Err("too_many_gateways");
    }
    Ok(rows)
}
pub(crate) fn register(
    db: &Connection,
    vault: &Vault,
    account: i64,
    value: Registration,
) -> Result<Gateway> {
    value.validate()?;
    let tx = db
        .unchecked_transaction()
        .map_err(|_| "gateway_storage_unavailable")?;
    let count: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM playback_gateways WHERE owner_account_id=?1",
            [account],
            |row| row.get(0),
        )
        .map_err(|_| "gateway_storage_unavailable")?;
    if count >= 16 {
        return Err("too_many_gateways");
    }
    if list(&tx, account)?.len() >= 64 {
        return Err("too_many_gateways");
    }
    let id = uuid::Uuid::new_v4().to_string();
    let encrypted = vault.seal(
        account,
        "gateway_key",
        &id,
        value.integration_key.as_bytes(),
    )?;
    let endpoint = super::client::endpoint(&value.endpoint)?.to_string();
    let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM playback_gateways WHERE owner_account_id=?1 AND endpoint=?2 AND namespace=?3)",params![account,endpoint,value.namespace],|row|row.get(0)).map_err(|_|"gateway_storage_unavailable")?;
    if exists {
        return Err("gateway_already_configured");
    }
    tx.execute("INSERT INTO playback_gateways(id,owner_account_id,name,endpoint,namespace,priority,secret) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![id,account,value.name.trim(),endpoint,value.namespace,value.priority,encrypted]).map_err(|_|"gateway_storage_unavailable")?;
    tx.commit().map_err(|_| "gateway_storage_unavailable")?;
    list(db, account)?
        .into_iter()
        .find(|gateway| gateway.id == id)
        .ok_or("gateway_not_found")
}
pub(crate) fn authorized(
    db: &Connection,
    vault: &Vault,
    account: i64,
    id: &str,
) -> Result<AuthorizedGateway> {
    let gateway = list(db, account)?
        .into_iter()
        .find(|gateway| gateway.id == id && gateway.enabled)
        .ok_or("gateway_not_found")?;
    let (owner,envelope):(i64,String)=db.query_row(&format!("SELECT g.owner_account_id,g.secret FROM playback_gateways g WHERE g.id=?2 AND g.enabled=1 AND {ALLOWED}"),params![account,id],|row|Ok((row.get(0)?,row.get(1)?))).optional().map_err(|_|"gateway_storage_unavailable")?.ok_or("gateway_not_found")?;
    let key = vault.open(owner, "gateway_key", id, &envelope)?;
    Ok(AuthorizedGateway { gateway, key })
}
pub(crate) fn replace(
    db: &Connection,
    vault: &Vault,
    account: i64,
    id: &str,
    value: Registration,
) -> Result<Gateway> {
    value.validate()?;
    let encrypted = vault.seal(account, "gateway_key", id, value.integration_key.as_bytes())?;
    let endpoint = super::client::endpoint(&value.endpoint)?.to_string();
    let exists:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM playback_gateways WHERE owner_account_id=?1 AND endpoint=?2 AND namespace=?3 AND id!=?4)",params![account,endpoint,value.namespace,id],|row|row.get(0)).map_err(|_|"gateway_storage_unavailable")?;
    if exists {
        return Err("gateway_already_configured");
    }
    let changed=db.execute("UPDATE playback_gateways SET name=?3,endpoint=?4,namespace=?5,priority=?6,secret=?7,revision=revision+1 WHERE id=?2 AND owner_account_id=?1",params![account,id,value.name.trim(),endpoint,value.namespace,value.priority,encrypted]).map_err(|_|"gateway_storage_unavailable")?;
    if changed == 0 {
        return Err("gateway_not_found");
    }
    list(db, account)?
        .into_iter()
        .find(|gateway| gateway.id == id)
        .ok_or("gateway_not_found")
}
pub(crate) fn update(db: &Connection, account: i64, id: &str, value: Patch) -> Result<()> {
    if (value.name.is_none() && value.priority.is_none() && value.enabled.is_none())
        || value
            .priority
            .is_some_and(|value| !(0..=10000).contains(&value))
        || value.name.as_ref().is_some_and(|name| {
            name.trim().is_empty() || name.len() > 128 || name.chars().any(char::is_control)
        })
    {
        return Err("invalid_gateway_configuration");
    }
    let changed=db.execute("UPDATE playback_gateways SET name=COALESCE(?3,name),priority=COALESCE(?4,priority),revision=revision+CASE WHEN ?5 IS NOT NULL AND enabled!=?5 THEN 1 ELSE 0 END,enabled=COALESCE(?5,enabled) WHERE id=?2 AND owner_account_id=?1",params![account,id,value.name.as_deref().map(str::trim),value.priority,value.enabled]).map_err(|_|"gateway_storage_unavailable")?;
    if changed == 0 {
        return Err("gateway_not_found");
    }
    Ok(())
}
pub(crate) fn delete(db: &Connection, account: i64, id: &str) -> Result<()> {
    db.execute(
        "DELETE FROM playback_gateways WHERE id=?2 AND owner_account_id=?1",
        params![account, id],
    )
    .map_err(|_| "gateway_storage_unavailable")?;
    Ok(())
}
pub(crate) fn grant(
    db: &Connection,
    owner: i64,
    id: &str,
    account: i64,
    enabled: bool,
) -> Result<()> {
    let tx = db
        .unchecked_transaction()
        .map_err(|_| "gateway_storage_unavailable")?;
    let owns: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM playback_gateways WHERE id=?2 AND owner_account_id=?1)",
            params![owner, id],
            |row| row.get(0),
        )
        .map_err(|_| "gateway_storage_unavailable")?;
    if !owns {
        return Err("gateway_not_found");
    }
    if enabled {
        let accessible = list(&tx, account)?;
        if accessible.len() >= 64 && !accessible.iter().any(|gateway| gateway.id == id) {
            return Err("too_many_gateways");
        }
        let active: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_accounts WHERE id=?1 AND disabled=0)",
                [account],
                |row| row.get(0),
            )
            .map_err(|_| "gateway_storage_unavailable")?;
        if !active {
            return Err("gateway_account_unavailable");
        }
        tx.execute(
            "INSERT OR IGNORE INTO playback_gateway_grants(gateway_id,account_id) VALUES(?1,?2)",
            params![id, account],
        )
        .map_err(|_| "gateway_storage_unavailable")?;
    } else {
        tx.execute(
            "DELETE FROM playback_gateway_grants WHERE gateway_id=?1 AND account_id=?2",
            params![id, account],
        )
        .map_err(|_| "gateway_storage_unavailable")?;
    }
    tx.commit().map_err(|_| "gateway_storage_unavailable")
}
