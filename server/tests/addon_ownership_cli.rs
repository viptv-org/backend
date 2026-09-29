#![cfg(unix)]
use base64::Engine;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::{os::unix::fs::PermissionsExt, process::Command};

#[test]
fn addon_cli_requires_reviewed_ownership_and_encryption_confirmation() {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let database = root.path().join("source.sqlite");
    let db = Connection::open(&database).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL;CREATE TABLE auth_accounts(id INTEGER PRIMARY KEY,disabled INTEGER NOT NULL);INSERT INTO auth_accounts VALUES(11,0);CREATE TABLE addons(id INTEGER PRIMARY KEY,name TEXT NOT NULL,manifest_url TEXT UNIQUE NOT NULL,enabled INTEGER NOT NULL DEFAULT 1,manifest TEXT NOT NULL);").unwrap();
    db.execute("INSERT INTO addons VALUES(7,'Fixture','https://fixture.invalid/private-addon-token/manifest.json',1,?1)",[json!({"id":"fixture","name":"Fixture","resources":[],"logo":"https://art.invalid/private-addon-token"}).to_string()]).unwrap();
    let binary = env!("CARGO_BIN_EXE_provider-owners");
    let inspected = Command::new(binary)
        .arg("inspect-addons")
        .arg(&database)
        .output()
        .unwrap();
    assert!(inspected.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&inspected.stdout).unwrap()["unassigned"],
        json!([7])
    );
    assert!(!String::from_utf8_lossy(&inspected.stdout).contains("private-addon-token"));
    let owners = root.path().join("owners.json");
    std::fs::write(&owners, br#"{"7":11}"#).unwrap();
    let assigned = Command::new(binary)
        .arg("apply-addons")
        .arg(&database)
        .arg(&owners)
        .arg(root.path().join("owned-before.sqlite"))
        .arg(root.path().join("owned-before.json"))
        .arg("1111111111111111111111111111111111111111")
        .arg("--confirm-ownership")
        .output()
        .unwrap();
    assert!(
        assigned.status.success(),
        "{}",
        String::from_utf8_lossy(&assigned.stderr)
    );
    let backup = root.path().join("before-encryption.sqlite");
    let export = root.path().join("before-encryption.json");
    let keyring=json!({"active":"fixture","keys":{"fixture":base64::engine::general_purpose::STANDARD.encode([7u8;32])}}).to_string();
    let encrypt = |confirm: bool, key: bool| {
        let mut command = Command::new(binary);
        command
            .env_remove("VIPTV_SECRETS_KEYRING")
            .arg("encrypt-addons")
            .arg(&database)
            .arg(&backup)
            .arg(&export)
            .arg("1111111111111111111111111111111111111111");
        if confirm {
            command.arg("--confirm-encryption");
        }
        if key {
            command.env("VIPTV_SECRETS_KEYRING", &keyring);
        }
        command.output().unwrap()
    };
    assert!(!encrypt(false, true).status.success());
    assert!(!encrypt(true, false).status.success());
    assert!(!backup.exists());
    let encrypted = encrypt(true, true);
    assert!(
        encrypted.status.success(),
        "{}",
        String::from_utf8_lossy(&encrypted.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&encrypted.stdout).unwrap()["encrypted_addon_count"],
        1
    );
    for output in [&encrypted.stdout, &encrypted.stderr] {
        let text = String::from_utf8_lossy(output);
        assert!(!text.contains("private-addon-token") && !text.contains(&keyring));
    }
    assert_eq!(
        db.query_row("SELECT manifest_url FROM addons WHERE id=7", [], |r| r
            .get::<_, String>(
            0
        ))
        .unwrap(),
        "sealed:addon:7"
    );
    assert!(std::fs::read_to_string(export)
        .unwrap()
        .contains("private-addon-token"));
    assert_eq!(
        std::fs::metadata(backup).unwrap().permissions().mode() & 0o777,
        0o600
    );
}
