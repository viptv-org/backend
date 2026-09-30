//! Shared in-process fixtures for the `cfg(test)` router test modules.
use crate::{router, App};
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use rusqlite::Connection;
use serde_json::Value;
use tower::ServiceExt;

pub(crate) fn vault() -> std::sync::Arc<crate::secret_store::Vault> {
    use base64::Engine;
    std::sync::Arc::new(crate::secret_store::Vault::from_json(
        &serde_json::json!({"active":"fixture","keys":{"fixture":base64::engine::general_purpose::STANDARD.encode([7u8;32])}}).to_string()
    ).unwrap())
}

pub(crate) fn configure_vault(app: &mut App) {
    let vault = vault();
    app.secret_vault = Some(vault.clone());
    app.providers.vault = Some(vault.clone());
    app.addons.vault = Some(vault);
}

/// Explicit fixture conversion, preserving identities and ownership. Never
/// called by a request or production runtime; legacy tests opt out.
pub(crate) fn encrypt_fixture_sources(app: &App) {
    let db = app.db.lock().unwrap();
    let vault = app.secret_vault.as_ref().unwrap();
    encrypt_fixture_db(&db, vault);
}
pub(crate) fn encrypt_fixture_db(db: &Connection, vault: &crate::secret_store::Vault) {
    use rusqlite::params;
    let providers = db.prepare("SELECT p.id,o.account_id,p.url,p.username,p.password FROM providers p JOIN provider_ownership o ON o.provider_id=p.id WHERE credentials_version=0").unwrap()
        .query_map([], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?))).unwrap().collect::<Result<Vec<_>,_>>().unwrap();
    for (id, account, url, username, password) in providers {
        let secret = vault
            .seal(
                account,
                "xtream",
                &id.to_string(),
                serde_json::json!({"url":url,"username":username,"password":password})
                    .to_string()
                    .as_bytes(),
            )
            .unwrap();
        db.execute(
            "INSERT INTO provider_credentials_v2 VALUES(?1,?2,?3)",
            params![id, account, secret],
        )
        .unwrap();
        db.execute(
            "UPDATE providers SET url='',username='',password='',credentials_version=1 WHERE id=?1",
            [id],
        )
        .unwrap();
        let cache = db
            .prepare("SELECT cache_key,payload FROM provider_cache WHERE provider_id=?1")
            .unwrap()
            .query_map([id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        use sha2::{Digest, Sha256};
        for (key, payload) in cache {
            let record = format!("{id}:{:x}", Sha256::digest(key.as_bytes()));
            let secret = vault
                .seal(account, "xtream-cache", &record, payload.as_bytes())
                .unwrap();
            db.execute(
                "UPDATE provider_cache SET payload=?3 WHERE provider_id=?1 AND cache_key=?2",
                params![id, key, secret],
            )
            .unwrap();
        }
    }
    let addons = db.prepare("SELECT id,account_id,manifest_url,manifest FROM addons WHERE credentials_version=0 AND account_id>0").unwrap()
        .query_map([], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?))).unwrap().collect::<Result<Vec<_>,_>>().unwrap();
    for (id, account, url, manifest) in addons {
        let manifest: Value = serde_json::from_str(&manifest).unwrap();
        let secret = vault
            .seal_addon(
                account,
                &id.to_string(),
                serde_json::json!({"url":url,"manifest":manifest})
                    .to_string()
                    .as_bytes(),
            )
            .unwrap();
        db.execute(
            "INSERT INTO addon_credentials_v2 VALUES(?1,?2,?3)",
            params![id, account, secret],
        )
        .unwrap();
        db.execute("UPDATE addons SET manifest_url=?2,manifest='{}',credentials_version=1,credentials_revision='fixture' WHERE id=?1",params![id,format!("sealed:addon:{id}")]).unwrap();
    }
}

/// `App` over `db` whose media tools can never run, so playback endpoints
/// fail fast instead of spawning processes or touching the filesystem.
pub(crate) fn app_with_db(db: Connection) -> App {
    App::new(db, reqwest::Client::new()).unwrap()
}

/// In-memory `App` with unavailable media tools.
pub(crate) fn app() -> App {
    app_with_db(Connection::open_in_memory().unwrap())
}

/// Bearer-authenticated JSON request against a fresh router for `app`.
pub(crate) async fn request(
    app: &App,
    token: &str,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = router(app.clone(), None)
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
