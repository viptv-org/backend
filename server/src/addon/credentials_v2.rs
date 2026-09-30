//! Preserve addon identities while encrypting token-bearing URLs and manifests.
use super::*;
use crate::secret_store::Vault;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    url: String,
    manifest: Value,
}
impl Drop for Payload {
    fn drop(&mut self) {
        self.url.zeroize();
    }
}
fn storage(_: rusqlite::Error) -> &'static str {
    "addon_storage_unavailable"
}
pub(super) fn scope_legacy(db: &Connection) -> Result<(), &'static str> {
    let columns = db
        .prepare("PRAGMA table_info(addons)")
        .map_err(storage)?
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(storage)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(storage)?;
    if columns.iter().any(|c| c == "account_id") {
        return Ok(());
    }
    if columns.is_empty()
        || columns.iter().any(|c| {
            ![
                "id",
                "name",
                "manifest_url",
                "enabled",
                "manifest",
                "priority",
            ]
            .contains(&c.as_str())
        })
    {
        return Err("addon_ownership_migration_required");
    }
    let priority = if columns.iter().any(|c| c == "priority") {
        "priority"
    } else {
        "0"
    };
    db.execute_batch(&format!("ALTER TABLE addons RENAME TO addons_legacy_v2;
      CREATE TABLE addons(id INTEGER PRIMARY KEY,name TEXT NOT NULL,manifest_url TEXT NOT NULL,enabled INTEGER NOT NULL DEFAULT 1,manifest TEXT NOT NULL,priority INTEGER NOT NULL DEFAULT 0,account_id INTEGER NOT NULL DEFAULT 0,UNIQUE(account_id,manifest_url));
      INSERT INTO addons(id,name,manifest_url,enabled,manifest,priority) SELECT id,name,manifest_url,enabled,manifest,{priority} FROM addons_legacy_v2;
      DROP TABLE addons_legacy_v2;")).map_err(storage)
}
pub(crate) fn init(db: &Connection) -> Result<(), &'static str> {
    let columns = db
        .prepare("PRAGMA table_info(addons)")
        .map_err(storage)?
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(storage)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(storage)?;
    if !columns.iter().any(|c| c == "account_id") {
        return Err("addon_ownership_migration_required");
    }
    if !columns.iter().any(|c| c == "credentials_version") {
        db.execute_batch(
            "ALTER TABLE addons ADD COLUMN credentials_version INTEGER NOT NULL DEFAULT 0;",
        )
        .map_err(storage)?;
    }
    if !columns.iter().any(|c| c == "credentials_revision") {
        db.execute_batch(
            "ALTER TABLE addons ADD COLUMN credentials_revision TEXT NOT NULL DEFAULT '';",
        )
        .map_err(storage)?;
    }
    db.execute_batch("CREATE TABLE IF NOT EXISTS addon_credentials_v2(addon_id INTEGER PRIMARY KEY REFERENCES addons(id) ON DELETE CASCADE,account_id INTEGER NOT NULL,secret TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS addon_identity_v2(id INTEGER PRIMARY KEY AUTOINCREMENT);
        CREATE TABLE IF NOT EXISTS addon_encryption_accounts_v2(account_id INTEGER PRIMARY KEY);
        INSERT OR IGNORE INTO addon_identity_v2(id) SELECT id FROM addons;").map_err(storage)?;
    db.execute(
        "UPDATE addons SET credentials_revision=lower(hex(randomblob(16))) WHERE credentials_version=1 AND credentials_revision=''",
        [],
    )
    .map_err(storage)?;
    Ok(())
}
pub(super) fn read(
    db: &Connection,
    vault: Option<&Vault>,
    account: i64,
    id: i64,
    url: String,
    manifest: String,
    version: i64,
) -> Result<(String, Value), &'static str> {
    read_inner(db, vault, account, id, url, manifest, version, false)
}

// Explicitly offline only: encryption must inspect the original plaintext.
#[allow(clippy::too_many_arguments)]
fn read_inner(
    db: &Connection,
    vault: Option<&Vault>,
    account: i64,
    id: i64,
    url: String,
    manifest: String,
    version: i64,
    offline_legacy: bool,
) -> Result<(String, Value), &'static str> {
    let stored: Option<(i64, String)> = db
        .query_row(
            "SELECT account_id,secret FROM addon_credentials_v2 WHERE addon_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(storage)?;
    if version == 0 && stored.is_none() {
        if !offline_legacy {
            return Err("source_credentials_migration_required");
        }
        return Ok((
            url,
            serde_json::from_str(&manifest).map_err(|_| "Invalid stored manifest")?,
        ));
    }
    if version != 1 || url != format!("sealed:addon:{id}") || manifest != "{}" {
        return Err("invalid_secret_envelope");
    }
    let Some((owner, envelope)) = stored else {
        return Err("invalid_secret_envelope");
    };
    if owner != account {
        return Err("secret_authentication_failed");
    }
    let bytes = vault.ok_or("secret_store_not_configured")?.open_addon(
        account,
        &id.to_string(),
        &envelope,
    )?;
    let mut payload: Payload =
        serde_json::from_slice(bytes.expose()).map_err(|_| "invalid_secret_envelope")?;
    Ok((
        std::mem::take(&mut payload.url),
        std::mem::take(&mut payload.manifest),
    ))
}
fn seal(
    vault: &Vault,
    account: i64,
    id: i64,
    url: &str,
    manifest: &Value,
) -> Result<String, &'static str> {
    let bytes = Zeroizing::new(
        serde_json::to_vec(&json!({"url":url,"manifest":manifest}))
            .map_err(|_| "secret_encryption_failed")?,
    );
    vault.seal_addon(account, &id.to_string(), &bytes)
}
pub(super) fn store(
    db: &Connection,
    vault: &Vault,
    account: i64,
    url: &str,
    manifest: &Value,
) -> Result<i64, &'static str> {
    store_inner(db, vault, account, url, manifest, None)
}
pub(super) type Snapshot = Option<(i64, String)>;
pub(super) fn snapshot(
    db: &Connection,
    vault: &Vault,
    account: i64,
    url: &str,
) -> Result<Snapshot, &'static str> {
    let target = url::Url::parse(url).map_err(|_| "invalid_addon_endpoint")?;
    let ids = db
        .prepare("SELECT id FROM addons WHERE account_id=?1 ORDER BY id")
        .map_err(storage)?
        .query_map([account], |r| r.get::<_, i64>(0))
        .map_err(storage)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(storage)?;
    for id in ids {
        let (old_url,manifest,version,revision):(String,String,i64,String)=db.query_row("SELECT manifest_url,manifest,credentials_version,credentials_revision FROM addons WHERE id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).map_err(storage)?;
        let old_url = read(db, Some(vault), account, id, old_url, manifest, version)?.0;
        if url::Url::parse(&old_url).ok().as_ref() == Some(&target) {
            if version == 0 {
                return Err("addon_encryption_required");
            }
            return Ok(Some((id, revision)));
        }
    }
    Ok(None)
}
pub(super) fn store_checked(
    db: &Connection,
    vault: &Vault,
    account: i64,
    url: &str,
    manifest: &Value,
    expected: &Snapshot,
) -> Result<i64, &'static str> {
    store_inner(db, vault, account, url, manifest, Some(expected))
}
fn store_inner(
    db: &Connection,
    vault: &Vault,
    account: i64,
    url: &str,
    manifest: &Value,
    expected: Option<&Snapshot>,
) -> Result<i64, &'static str> {
    if account <= 0 {
        return Err("account_session_required");
    }
    let tx = db.unchecked_transaction().map_err(storage)?;
    let active: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM auth_accounts WHERE id=?1 AND disabled=0)",
            [account],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if !active {
        return Err("account_session_required");
    }
    let current = snapshot(&tx, vault, account, url)?;
    if expected.is_some_and(|expected| expected != &current) {
        return Err("addon_configuration_changed");
    }
    let existing = current.map(|(id, _)| id);
    let id = if let Some(id) = existing {
        id
    } else {
        // Never reuse a deleted encrypted addon ID for a different source.
        tx.execute(
            "INSERT OR IGNORE INTO addon_identity_v2(id) SELECT id FROM addons",
            [],
        )
        .map_err(storage)?;
        tx.execute("INSERT INTO addon_identity_v2 DEFAULT VALUES", [])
            .map_err(storage)?;
        let id = tx.last_insert_rowid();
        tx.execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id,credentials_version) VALUES(?1,?2,?3,'{}',?4,1)",params![id,manifest["name"].as_str().ok_or("Invalid addon manifest")?,format!("sealed:addon:{id}"),account]).map_err(storage)?;
        id
    };
    tx.execute("INSERT INTO addon_credentials_v2(addon_id,account_id,secret) VALUES(?1,?2,?3) ON CONFLICT(addon_id) DO UPDATE SET account_id=excluded.account_id,secret=excluded.secret",params![id,account,seal(vault,account,id,url,manifest)?]).map_err(storage)?;
    tx.execute(
        "UPDATE addons SET name=?2,manifest_url=?3,manifest='{}',credentials_version=1,credentials_revision=?4 WHERE id=?1",
        params![
            id,
            manifest["name"].as_str().ok_or("Invalid addon manifest")?,
            format!("sealed:addon:{id}"),uuid::Uuid::new_v4().to_string()
        ],
    )
    .map_err(storage)?;
    tx.execute(
        "INSERT OR IGNORE INTO addon_encryption_accounts_v2 VALUES(?1)",
        [account],
    )
    .map_err(storage)?;
    tx.commit().map_err(storage)?;
    Ok(id)
}
pub(crate) fn encrypt_legacy(
    tx: &rusqlite::Transaction<'_>,
    vault: &Vault,
) -> Result<usize, &'static str> {
    init(tx)?;
    let rows = tx
        .prepare("SELECT id FROM addons ORDER BY id")
        .map_err(storage)?
        .query_map([], |r| r.get::<_, i64>(0))
        .map_err(storage)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(storage)?;
    let mut count = 0;
    for id in rows {
        let (account,url,manifest,version):(i64,String,String,i64)=tx.query_row("SELECT account_id,manifest_url,manifest,credentials_version FROM addons WHERE id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).map_err(storage)?;
        let owned: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_accounts WHERE id=?1)",
                [account],
                |r| r.get(0),
            )
            .map_err(storage)?;
        if account <= 0 || !owned {
            return Err("addon_owner_assignment_required");
        }
        tx.execute(
            "INSERT OR IGNORE INTO addon_encryption_accounts_v2 VALUES(?1)",
            [account],
        )
        .map_err(storage)?;
        let (url, manifest) =
            read_inner(tx, Some(vault), account, id, url, manifest, version, true)?;
        if version == 1 {
            continue;
        }
        validate_url(&url).map_err(|_| "invalid_legacy_addon_url")?;
        if !manifest.is_object() {
            return Err("Invalid stored manifest");
        }
        tx.execute(
            "INSERT INTO addon_credentials_v2(addon_id,account_id,secret) VALUES(?1,?2,?3)",
            params![id, account, seal(vault, account, id, &url, &manifest)?],
        )
        .map_err(storage)?;
        tx.execute(
            "UPDATE addons SET manifest_url=?2,manifest='{}',credentials_version=1,credentials_revision=?3 WHERE id=?1",
            params![id, format!("sealed:addon:{id}"),uuid::Uuid::new_v4().to_string()],
        )
        .map_err(storage)?;
        count += 1;
    }
    Ok(count)
}

pub(crate) fn ownership(db: &Connection) -> Result<Value, &'static str> {
    let columns = db
        .prepare("PRAGMA table_info(addons)")
        .map_err(storage)?
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(storage)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(storage)?;
    if columns.is_empty() {
        return Ok(json!({"unassigned":[],"assignments":[]}));
    }
    let sql = if columns.iter().any(|c| c == "account_id") {
        "SELECT id,account_id FROM addons ORDER BY id"
    } else {
        "SELECT id,0 FROM addons ORDER BY id"
    };
    let mut unassigned = vec![];
    let mut assignments = vec![];
    for row in db
        .prepare(sql)
        .map_err(storage)?
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
        .map_err(storage)?
    {
        let (id, account) = row.map_err(storage)?;
        if account <= 0 {
            unassigned.push(id);
        } else {
            assignments.push((id, account));
        }
    }
    Ok(json!({"unassigned":unassigned,"assignments":assignments}))
}
pub(crate) fn assign_legacy(
    tx: &rusqlite::Transaction<'_>,
    owners: &std::collections::BTreeMap<i64, i64>,
) -> Result<(), &'static str> {
    scope_legacy(tx)?;
    init(tx)?;
    let report = ownership(tx)?;
    let unassigned = report["unassigned"]
        .as_array()
        .ok_or("addon_storage_unavailable")?;
    if unassigned.len() != owners.len()
        || unassigned
            .iter()
            .any(|id| id.as_i64().is_none_or(|id| !owners.contains_key(&id)))
    {
        return Err("addon_owner_assignment_required");
    }
    for (&id, &account) in owners {
        let active: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_accounts WHERE id=?1 AND disabled=0)",
                [account],
                |r| r.get(0),
            )
            .map_err(storage)?;
        if !active {
            return Err("invalid_legacy_addon_owner");
        }
        tx.execute(
            "UPDATE addons SET account_id=?2 WHERE id=?1 AND account_id<=0",
            params![id, account],
        )
        .map_err(storage)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    fn vault() -> Arc<Vault> {
        Arc::new(Vault::from_json(&json!({"active":"fixture","keys":{"fixture":base64::engine::general_purpose::STANDARD.encode([7u8;32])}}).to_string()).unwrap())
    }
    #[test]
    fn legacy_initialization_never_assigns_secrets_to_the_first_owner() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE auth_accounts(id INTEGER PRIMARY KEY,role TEXT,disabled INTEGER NOT NULL);INSERT INTO auth_accounts VALUES(1,'owner',0);
          CREATE TABLE addons(id INTEGER PRIMARY KEY,name TEXT NOT NULL,manifest_url TEXT UNIQUE NOT NULL,enabled INTEGER NOT NULL DEFAULT 1,manifest TEXT NOT NULL);
          INSERT INTO addons VALUES(7,'Legacy','https://fixture.invalid/private-token/manifest.json',1,'{}');").unwrap();
        let db = Arc::new(Mutex::new(db));
        let addons = Addons::new(db.clone(), reqwest::Client::new()).unwrap();
        assert_eq!(addons.for_account(1).list().unwrap(), json!([]));
        assert_eq!(
            ownership(&db.lock().unwrap()).unwrap()["unassigned"],
            json!([7])
        );
    }
    #[tokio::test]
    async fn encrypted_addons_are_redacted_bound_to_owner_and_never_downgrade() {
        let app = crate::auth_integration_tests::fixture();
        let vault = vault();
        let mut addons = app.addons.clone().for_account(1);
        addons.vault = Some(vault.clone());
        let url = "https://fixture.invalid/private-addon-token/manifest.json";
        let manifest = json!({"id":"fixture","name":"Fixture","resources":["catalog"],"logo":"https://art.invalid/private-addon-token","catalogs":[],"extra":"x".repeat(300_000)});
        let id = store(&app.db.lock().unwrap(), &vault, 1, url, &manifest).unwrap();
        let entries = addons.entries().unwrap();
        assert_eq!(entries[0], (id, url.to_owned(), manifest.clone()));
        let public = addons.list().unwrap();
        assert_eq!(public[0]["manifest_url"], Value::Null);
        assert_eq!(public[0]["credentials_encrypted"], true);
        assert!(!public.to_string().contains("private-addon-token"));
        let before =
            crate::sources::source_configuration(&app.db.lock().unwrap(), &format!("addon:{id}"))
                .unwrap();
        assert_eq!(
            store(&app.db.lock().unwrap(), &vault, 1, url, &manifest).unwrap(),
            id
        );
        assert_ne!(
            before,
            crate::sources::source_configuration(&app.db.lock().unwrap(), &format!("addon:{id}"))
                .unwrap()
        );
        let foreign = addons.clone().for_account(2);
        assert!(foreign.entries().unwrap().is_empty());
        foreign.delete(id).unwrap();
        assert_eq!(addons.entries().unwrap().len(), 1);
        addons.vault = None;
        assert_eq!(addons.entries().unwrap_err(), "secret_store_not_configured");
        assert_eq!(
            addons
                .add("https://fixture.invalid/another/manifest.json")
                .await
                .unwrap_err(),
            "secret_store_not_configured"
        );
        addons.vault = Some(vault.clone());
        addons.delete(id).unwrap();
        assert_eq!(
            app.db
                .lock()
                .unwrap()
                .query_row("SELECT count(*) FROM addon_credentials_v2", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        addons.vault = None;
        assert_eq!(
            addons.add(url).await.unwrap_err(),
            "secret_store_not_configured"
        );
        let new_id = store(&app.db.lock().unwrap(), &vault, 1, url, &manifest).unwrap();
        assert!(new_id > id);
        app.db
            .lock()
            .unwrap()
            .execute("UPDATE addons SET account_id=2 WHERE id=?1", [new_id])
            .unwrap();
        let mut foreign = addons.for_account(2);
        foreign.vault = Some(vault);
        assert_eq!(
            foreign.entries().unwrap_err(),
            "secret_authentication_failed"
        );
    }
    #[tokio::test]
    async fn network_install_uses_encrypted_storage_and_reinstall_preserves_id() {
        use axum::{routing::get, Json, Router};
        let app = crate::auth_integration_tests::fixture();
        let mut addons = app.addons.clone().for_account(1);
        addons.vault = Some(vault());
        addons.allow_test_loopback = true;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/private-addon-token/manifest.json",
            listener.local_addr().unwrap()
        );
        let server = tokio::spawn(async move {
            axum::serve(listener,Router::new().route("/private-addon-token/manifest.json",get(||async {Json(json!({"id":"fixture","name":"Fixture","resources":["stream"],"types":["movie"],"logo":"http://art.invalid/private-addon-token"}))}))).await.unwrap();
        });
        let first = addons.add(&url).await.unwrap();
        let second = addons.add(&url).await.unwrap();
        assert_eq!(first["id"], second["id"]);
        assert_eq!(first["manifest_url"], Value::Null);
        let db = app.db.lock().unwrap();
        let (stored, manifest): (String, String) = db
            .query_row("SELECT manifest_url,manifest FROM addons", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert!(!stored.contains("private-addon-token"));
        assert_eq!(manifest, "{}");
        server.abort();
    }
    #[tokio::test]
    async fn legacy_addon_reinstallation_does_not_bypass_backup_first_migration() {
        let app = crate::auth_integration_tests::fixture();
        let vault = vault();
        let db = app.db.lock().unwrap();
        let manifest = json!({"id":"fixture","name":"Fixture","resources":[]});
        db.execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id) VALUES(7,'Fixture','https://fixture.invalid/private-token/manifest.json',?1,1)",[manifest.to_string()]).unwrap();
        assert_eq!(
            store(
                &db,
                &vault,
                1,
                "https://fixture.invalid/private-token/manifest.json",
                &manifest
            )
            .unwrap_err(),
            "source_credentials_migration_required"
        );
        let tx = db.unchecked_transaction().unwrap();
        assert_eq!(encrypt_legacy(&tx, &vault).unwrap(), 1);
        tx.commit().unwrap();
        assert_eq!(
            store(
                &db,
                &vault,
                1,
                "https://fixture.invalid/private-token/manifest.json",
                &manifest
            )
            .unwrap(),
            7
        );
        db.execute("DELETE FROM addon_credentials_v2 WHERE addon_id=7", [])
            .unwrap();
        drop(db);
        let mut addons = app.addons.clone().for_account(1);
        addons.vault = Some(vault);
        assert_eq!(addons.entries().unwrap_err(), "invalid_secret_envelope");
    }
}
