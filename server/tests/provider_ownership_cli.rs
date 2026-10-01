#![cfg(unix)]
use rusqlite::Connection;
use serde_json::{json, Value};
use std::{os::unix::fs::PermissionsExt, process::Command};

#[test]
fn executable_requires_confirmation_preserves_wal_and_does_not_print_secrets() {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let database = root.path().join("source.sqlite");
    let db = Connection::open(&database).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE auth_accounts(id INTEGER PRIMARY KEY,disabled INTEGER NOT NULL); INSERT INTO auth_accounts VALUES(11,0);
        CREATE TABLE progress(profile_id INTEGER,id TEXT,position REAL); INSERT INTO progress VALUES(7,'tt1234567',300.5);").unwrap();
    viptv_server::provider::init(&db).unwrap();
    db.execute("INSERT INTO providers(id,name,url,username,password) VALUES(1,'Fixture','http://fixture.invalid','private-user','private-source-password')", []).unwrap();
    let binary = env!("CARGO_BIN_EXE_provider-owners");
    let inspected = Command::new(binary)
        .arg("inspect")
        .arg(&database)
        .output()
        .unwrap();
    assert!(inspected.status.success());
    let report: Value = serde_json::from_slice(&inspected.stdout).unwrap();
    assert_eq!(report["unassigned"], json!([1]));
    assert!(!String::from_utf8_lossy(&inspected.stdout).contains("private"));
    assert!(!db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='provider_ownership')",
            [],
            |row| row.get::<_, bool>(0)
        )
        .unwrap());
    let owners = root.path().join("owners.json");
    std::fs::write(&owners, br#"{"1":11}"#).unwrap();
    let backup = root.path().join("backup.sqlite");
    let export = root.path().join("advanced.json");
    let invoke = |confirm: bool| {
        let mut command = Command::new(binary);
        command
            .arg("apply")
            .arg(&database)
            .arg(&owners)
            .arg(&backup)
            .arg(&export)
            .arg("1111111111111111111111111111111111111111");
        if confirm {
            command.arg("--confirm-ownership");
        }
        command.output().unwrap()
    };
    assert!(!invoke(false).status.success());
    assert!(!backup.exists() && !export.exists());
    let applied = invoke(true);
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let report: Value = serde_json::from_slice(&applied.stdout).unwrap();
    assert_eq!(report["assigned_count"], 1);
    assert_eq!(report["ownership"]["assignments"], json!([[1, 11]]));
    for output in [&applied.stdout, &applied.stderr] {
        let text = String::from_utf8_lossy(output);
        assert!(!text.contains("private-source-password") && !text.contains("fixture.invalid"));
    }
    let restored =
        Connection::open_with_flags(backup, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(
        restored
            .query_row(
                "SELECT position FROM progress WHERE profile_id=7",
                [],
                |row| row.get::<_, f64>(0)
            )
            .unwrap(),
        300.5
    );
    assert_eq!(
        restored
            .query_row("SELECT password FROM providers WHERE id=1", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
        "private-source-password"
    );
    assert!(!restored
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='provider_ownership')",
            [],
            |row| row.get::<_, bool>(0)
        )
        .unwrap());
    use base64::Engine;
    let keyring=json!({"active":"test","keys":{"test":base64::engine::general_purpose::STANDARD.encode([7u8;32])}}).to_string();
    let encrypted_backup = root.path().join("before-encryption.sqlite");
    let encrypted_export = root.path().join("before-encryption.json");
    let encrypt = |confirm: bool, key: bool| {
        let mut command = Command::new(binary);
        command
            .env_remove("VIPTV_SECRETS_KEYRING")
            .arg("encrypt")
            .arg(&database)
            .arg(&encrypted_backup)
            .arg(&encrypted_export)
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
    assert!(!encrypted_backup.exists());
    let encrypted = encrypt(true, true);
    assert!(
        encrypted.status.success(),
        "{}",
        String::from_utf8_lossy(&encrypted.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&encrypted.stdout).unwrap()["encrypted_count"],
        1
    );
    for output in [&encrypted.stdout, &encrypted.stderr] {
        let text = String::from_utf8_lossy(output);
        assert!(
            !text.contains("private-source-password")
                && !text.contains("fixture.invalid")
                && !text.contains(&keyring)
        );
    }
    assert_eq!(
        db.query_row("SELECT password FROM providers WHERE id=1", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        ""
    );
}
