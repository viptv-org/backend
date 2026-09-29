//! Encrypted Xtream tuples. Ownership/record binding prevents ciphertext swaps.
use super::*;
use crate::secret_store::Vault;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Credentials {
    url: String,
    username: String,
    password: String,
}
impl Drop for Credentials {
    fn drop(&mut self) {
        self.url.zeroize();
        self.username.zeroize();
        self.password.zeroize();
    }
}
pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    let columns = db
        .prepare("PRAGMA table_info(providers)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !columns.iter().any(|name| name == "credentials_version") {
        db.execute_batch(
            "ALTER TABLE providers ADD COLUMN credentials_version INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS provider_credentials_v2(
        provider_id INTEGER PRIMARY KEY REFERENCES providers(id) ON DELETE CASCADE,
        account_id INTEGER NOT NULL, secret TEXT NOT NULL);",
    )
}
pub(super) fn sealed(db: &Connection, id: i64) -> Result<bool, String> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM providers WHERE id=?1 AND credentials_version<>0) OR EXISTS(SELECT 1 FROM provider_credentials_v2 WHERE provider_id=?1)",
        [id],
        |r| r.get(0),
    )
    .map_err(db_error)
}
pub(super) fn read(
    db: &Connection,
    vault: Option<&Vault>,
    provider: &mut Provider,
) -> Result<(), String> {
    let stored: Option<(i64, String)> = db
        .query_row(
            "SELECT account_id,secret FROM provider_credentials_v2 WHERE provider_id=?1",
            [provider.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(db_error)?;
    let version: i64 = db
        .query_row(
            "SELECT credentials_version FROM providers WHERE id=?1",
            [provider.id],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    let Some((account, envelope)) = stored else {
        return if version == 0 {
            Ok(())
        } else {
            Err("invalid_secret_envelope".into())
        };
    };
    if version != 1 {
        return Err("invalid_secret_envelope".into());
    }
    let owned:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM provider_ownership WHERE provider_id=?1 AND account_id=?2)",params![provider.id,account],|r|r.get(0)).map_err(db_error)?;
    if !owned
        || !provider.url.is_empty()
        || !provider.username.is_empty()
        || !provider.password.is_empty()
    {
        return Err("source_configuration_changed".into());
    }
    let vault = vault.ok_or("secret_store_not_configured")?;
    let bytes = vault.open(account, "xtream", &provider.id.to_string(), &envelope)?;
    let credentials: Credentials =
        serde_json::from_slice(bytes.expose()).map_err(|_| "invalid_secret_envelope")?;
    provider.url = credentials.url.to_string();
    provider.username = credentials.username.to_string();
    provider.password = credentials.password.to_string();
    provider.sealed = Some((account, envelope));
    Ok(())
}

/// Only the backup-first, explicitly approved offline migration calls this.
pub(crate) fn encrypt_legacy(
    tx: &rusqlite::Transaction<'_>,
    vault: &Vault,
) -> Result<usize, &'static str> {
    init(tx).map_err(|_| "provider_storage_unavailable")?;
    if !super::v2::inspect_ownership(tx)?.unassigned.is_empty() {
        return Err("legacy_provider_owner_map_required");
    }
    let ids = tx
        .prepare("SELECT id FROM providers ORDER BY id")
        .map_err(|_| "provider_storage_unavailable")?
        .query_map([], |r| r.get::<_, i64>(0))
        .map_err(|_| "provider_storage_unavailable")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|_| "provider_storage_unavailable")?;
    let mut count = 0;
    for id in ids {
        let (account,url,username,password):(i64,String,String,String)=tx.query_row("SELECT o.account_id,p.url,p.username,p.password FROM providers p JOIN provider_ownership o ON o.provider_id=p.id WHERE p.id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).map_err(|_|"provider_storage_unavailable")?;
        let credentials = Credentials {
            url,
            username,
            password,
        };
        let version: i64 = tx
            .query_row(
                "SELECT credentials_version FROM providers WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .map_err(|_| "provider_storage_unavailable")?;
        let prior: Option<String> = tx
            .query_row(
                "SELECT secret FROM provider_credentials_v2 WHERE provider_id=?1 AND account_id=?2",
                params![id,account],
                |r| r.get(0),
            )
            .optional()
            .map_err(|_| "provider_storage_unavailable")?;
        if let Some(prior) = prior {
            if version != 1 {
                return Err("invalid_secret_envelope");
            }
            // Re-run verifies that saved data can still be unlocked; no plaintext fallback.
            vault.open(account, "xtream", &id.to_string(), &prior)?;
            if !credentials.url.is_empty()
                || !credentials.username.is_empty()
                || !credentials.password.is_empty()
            {
                return Err("source_configuration_changed");
            }
            continue;
        }
        if version != 0 {
            return Err("invalid_secret_envelope");
        }
        if super::base_url(&credentials.url).is_err()
            || credentials.username.is_empty()
            || credentials.password.is_empty()
        {
            return Err("invalid_legacy_provider_credentials");
        }
        let bytes = Zeroizing::new(
            serde_json::to_vec(&credentials).map_err(|_| "secret_encryption_failed")?,
        );
        let envelope = vault.seal(account, "xtream", &id.to_string(), &bytes)?;
        tx.execute(
            "INSERT INTO provider_credentials_v2 VALUES(?1,?2,?3)",
            params![id, account, envelope],
        )
        .map_err(|_| "provider_storage_unavailable")?;
        tx.execute(
            "UPDATE providers SET url='',username='',password='',credentials_version=1 WHERE id=?1",
            [id],
        )
        .map_err(|_| "provider_storage_unavailable")?;
        // Cached provider payloads may embed upstream URLs/credentials.
        tx.execute("DELETE FROM provider_cache WHERE provider_id=?1", [id])
            .map_err(|_| "provider_storage_unavailable")?;
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    #[tokio::test]
    async fn encrypted_readers_preserve_http_identity_and_fail_closed() {
        let app = crate::auth_integration_tests::fixture();
        let mut service = app.providers.for_account(1);
        let vault=Arc::new(Vault::from_json(&json!({"active":"test","keys":{"test":base64::engine::general_purpose::STANDARD.encode([7u8;32])}}).to_string()).unwrap());
        let id=service.add(json!({"name":"Encrypted fixture","url":"http://fixture.invalid/base","username":"private-user","password":"private-password"})).unwrap()["id"].as_i64().unwrap();
        app.db
            .lock()
            .unwrap()
            .execute("INSERT INTO provider_ownership VALUES(?1,1)", [id])
            .unwrap();
        let old = service.provider(id).unwrap();
        {
            let db = app.db.lock().unwrap();
            let tx = db.unchecked_transaction().unwrap();
            assert_eq!(encrypt_legacy(&tx, &vault).unwrap(), 1);
            tx.commit().unwrap();
            assert!(old.ensure_current(&db).is_err());
            // Restart schema initialization must not reinterpret encrypted rows.
            crate::provider::init(&db).unwrap();
        }
        assert_eq!(
            service.provider(id).err().unwrap(),
            "secret_store_not_configured"
        );
        service.vault = Some(vault.clone());
        let decrypted = service.provider(id).unwrap();
        assert_eq!(decrypted.url, "http://fixture.invalid/base");
        assert_eq!(decrypted.username, "private-user");
        assert_eq!(decrypted.password, "private-password");
        let mut reopened =
            ProviderService::new(app.db.clone(), service.client.clone()).for_account(1);
        reopened.vault = Some(vault.clone());
        assert_eq!(reopened.provider(id).unwrap().password, "private-password");
        decrypted.ensure_current(&app.db.lock().unwrap()).unwrap();
        let permit = service
            .acquire_playback_for_kind(id, "movie")
            .await
            .unwrap();
        assert_eq!(
            service
                .acquire_playback_for_kind(id, "movie")
                .await
                .err()
                .unwrap(),
            "Provider connection limit reached"
        );
        drop(permit);
        assert!(service.acquire_playback_for_kind(id, "movie").await.is_ok());
        assert!(service.for_account(2).provider(id).is_err());
        assert_eq!(
            service.update(id, json!({"enabled":false})).unwrap_err(),
            "client_update_required"
        );
        {
            let db = app.db.lock().unwrap();
            db.execute(
                "UPDATE provider_ownership SET account_id=2 WHERE provider_id=?1",
                [id],
            )
            .unwrap();
            assert!(decrypted.ensure_current(&db).is_err());
        }
        assert!(service.for_account(2).provider(id).is_err());
        app.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE provider_ownership SET account_id=1 WHERE provider_id=?1",
                [id],
            )
            .unwrap();
        app.db
            .lock()
            .unwrap()
            .execute(
                "DELETE FROM provider_credentials_v2 WHERE provider_id=?1",
                [id],
            )
            .unwrap();
        assert_eq!(
            service.provider(id).err().unwrap(),
            "invalid_secret_envelope"
        );
    }
}
