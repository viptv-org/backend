#![cfg(unix)]
//! Exercise the shipped offline command, never a server or production database.
use base64::{engine::general_purpose::STANDARD, Engine};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

const REVISION: &str = "1111111111111111111111111111111111111111";
const PRIVATE_VALUES: &[&str] = &[
    "http://fixture.invalid/private-input",
    "private-source-password",
    "private-organizer-token",
    "private-session-access",
];

struct Fixture {
    root: tempfile::TempDir,
    db: Connection,
    keyring: String,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let db = Connection::open(root.path().join("source.sqlite")).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
            CREATE TABLE auth_accounts(id INTEGER PRIMARY KEY,disabled INTEGER NOT NULL); INSERT INTO auth_accounts VALUES(11,0);
            CREATE TABLE profiles(id INTEGER PRIMARY KEY,name TEXT); INSERT INTO profiles VALUES(71,'Synthetic');
            CREATE TABLE progress(profile_id INTEGER,id TEXT,position REAL,source_addon_id TEXT,source_fingerprint TEXT);
            INSERT INTO progress VALUES(71,'tt1234567',123.5,'iptv:31','exact-fingerprint'),(71,'family:old',9.5,'iptv:31','historical-fingerprint');
            CREATE TABLE favorites(profile_id INTEGER,id TEXT); INSERT INTO favorites VALUES(71,'family:old');
            CREATE TABLE queue(profile_id INTEGER,id TEXT); INSERT INTO queue VALUES(71,'tt1234567');
            CREATE TABLE auth_sessions(id TEXT,access_hash TEXT); INSERT INTO auth_sessions VALUES('session-fixture','private-session-access');
            CREATE TABLE family_channels(id TEXT PRIMARY KEY,data TEXT NOT NULL);
            CREATE TABLE family_aliases(live_id TEXT PRIMARY KEY,channel_id TEXT REFERENCES family_channels(id));
            INSERT INTO family_channels VALUES('family:old','{\"token\":\"private-organizer-token\"}');
            INSERT INTO family_aliases VALUES('iptv:31:7','family:old');").unwrap();
        viptv_server::provider::init(&db).unwrap();
        db.execute_batch("INSERT INTO providers(id,name,url,username,password) VALUES(31,'Fixture','http://fixture.invalid/private-input','fixture-user','private-source-password');
            INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES('iptv:31:7',31,'7','Raw channel');
            INSERT INTO provider_vod(id,provider_id,stream_id,kind,name,normalized,extension) VALUES('vod:31:8',31,'8','movie','Fixture movie','fixture movie','mp4');
            INSERT INTO provider_matches VALUES('vod:31:8','tt1234567','movie');").unwrap();
        viptv_server::migration_v2::apply(
            &root.path().join("source.sqlite"),
            &BTreeMap::from([(31, 11)]),
            &root.path().join("owned.sqlite"),
            &root.path().join("owned.json"),
            REVISION,
        )
        .unwrap();
        let fixture = Self {
            root,
            db,
            keyring: json!({"active":"fixture","keys":{"fixture":STANDARD.encode([7u8;32])}})
                .to_string(),
        };
        let output = Command::new(env!("CARGO_BIN_EXE_provider-owners"))
            .env_remove("VIPTV_SECRETS_KEYRING")
            .env("VIPTV_SECRETS_KEYRING", &fixture.keyring)
            .arg("encrypt")
            .arg(fixture.database())
            .arg(fixture.path("sealed.sqlite"))
            .arg(fixture.path("sealed.json"))
            .arg(REVISION)
            .arg("--confirm-encryption")
            .output()
            .unwrap();
        fixture.assert_private_output(&output);
        assert!(
            output.status.success(),
            "Synthetic encryption preparation failed"
        );
        fixture
    }
    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }
    fn database(&self) -> PathBuf {
        self.path("source.sqlite")
    }
    fn retire(&self, backup: &Path, export: &Path, confirm: bool, keyring: Option<&str>) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_provider-owners"));
        command.env_remove("VIPTV_SECRETS_KEYRING");
        if let Some(keyring) = keyring {
            command.env("VIPTV_SECRETS_KEYRING", keyring);
        }
        command
            .arg("retire")
            .arg(self.database())
            .arg(backup)
            .arg(export)
            .arg(REVISION);
        if confirm {
            command.arg("--confirm-retirement");
        }
        let output = command.output().unwrap();
        self.assert_private_output(&output);
        if let Some(keyring) = keyring {
            for bytes in [&output.stdout, &output.stderr] {
                assert!(
                    !String::from_utf8_lossy(bytes).contains(keyring),
                    "Supplied keyring leaked in CLI output"
                );
            }
        }
        output
    }
    fn assert_private_output(&self, output: &Output) {
        for bytes in [&output.stdout, &output.stderr] {
            let text = String::from_utf8_lossy(bytes);
            assert!(
                !text.contains(&self.keyring),
                "Keyring leaked in CLI output"
            );
            for secret in PRIVATE_VALUES {
                assert!(
                    !text.contains(secret),
                    "Private source data leaked in CLI output"
                );
            }
        }
    }
    fn assert_unretired(&self, expected: &Value) {
        assert!(exists(&self.db, "family_channels") && exists(&self.db, "family_aliases"));
        assert!(!exists(&self.db, "retired_features_v2"));
        assert!(
            preserved(&self.db) == *expected,
            "Refusal changed preserved data"
        );
    }
}
fn exists(db: &Connection, table: &str) -> bool {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [table],
        |r| r.get(0),
    )
    .unwrap()
}
fn preserved(db: &Connection) -> Value {
    let mut result = serde_json::Map::new();
    // Static fixture-owned identifiers, not user/CLI input.
    for table in [
        "auth_accounts",
        "profiles",
        "progress",
        "favorites",
        "queue",
        "auth_sessions",
        "providers",
        "provider_ownership",
        "provider_credentials_v2",
        "provider_live",
        "provider_vod",
        "provider_matches",
        "account_media_settings",
    ] {
        let mut statement = db
            .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
            .unwrap();
        let rows = statement
            .query_map([], |row| {
                (0..row.as_ref().column_count())
                    .map(|column| {
                        Ok(match row.get_ref(column)? {
                            rusqlite::types::ValueRef::Null => Value::Null,
                            rusqlite::types::ValueRef::Integer(value) => json!(value),
                            rusqlite::types::ValueRef::Real(value) => json!(value),
                            rusqlite::types::ValueRef::Text(value) => {
                                json!(std::str::from_utf8(value).unwrap())
                            }
                            rusqlite::types::ValueRef::Blob(value) => json!(STANDARD.encode(value)),
                        })
                    })
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        result.insert(table.into(), json!(rows));
    }
    Value::Object(result)
}
fn assert_private_file(path: &Path) {
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn retirement_cli_refuses_missing_confirmation_keyring_and_unsafe_artifacts_without_mutation() {
    let fixture = Fixture::new();
    let before = preserved(&fixture.db);
    for (confirm, keyring, error) in [
        (false, Some(fixture.keyring.as_str()), "usage:"),
        (true, None, "secret_store_not_configured"),
    ] {
        let backup = fixture.path(if confirm {
            "missing-key.sqlite"
        } else {
            "unconfirmed.sqlite"
        });
        let export = backup.with_extension("json");
        let output = fixture.retire(&backup, &export, confirm, keyring);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains(error));
        assert!(!backup.exists() && !export.exists());
        fixture.assert_unretired(&before);
    }
    let unsafe_directory = fixture.path("unsafe");
    fs::create_dir(&unsafe_directory).unwrap();
    fs::set_permissions(&unsafe_directory, fs::Permissions::from_mode(0o755)).unwrap();
    let backup = unsafe_directory.join("backup.sqlite");
    let export = unsafe_directory.join("export.json");
    let output = fixture.retire(&backup, &export, true, Some(&fixture.keyring));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("artifact_directory_must_be_private_and_owned"));
    assert!(!backup.exists() && !export.exists());
    fixture.assert_unretired(&before);
    let existing = fixture.path("existing.sqlite");
    fs::write(&existing, b"existing preservation artifact").unwrap();
    fs::set_permissions(&existing, fs::Permissions::from_mode(0o600)).unwrap();
    let output = fixture.retire(
        &existing,
        &fixture.path("existing.json"),
        true,
        Some(&fixture.keyring),
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("artifact_path_must_be_new_and_writable")
    );
    assert_eq!(
        fs::read(existing).unwrap(),
        b"existing preservation artifact"
    );
    assert!(!fixture.path("existing.json").exists());
    fixture.assert_unretired(&before);
    let wrong =
        json!({"active":"fixture","keys":{"fixture":STANDARD.encode([8u8;32])}}).to_string();
    let backup = fixture.path("wrong-key.sqlite");
    let export = fixture.path("wrong-key.json");
    let output = fixture.retire(&backup, &export, true, Some(&wrong));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("secret_authentication_failed"));
    assert_private_file(&backup);
    assert_private_file(&export);
    fixture.assert_unretired(&before);
}

#[test]
fn confirmed_retirement_cli_preserves_ids_history_and_private_snapshots_then_retries_idempotently()
{
    let fixture = Fixture::new();
    let before = preserved(&fixture.db);
    let backup = fixture.path("before-retirement.sqlite");
    let export = fixture.path("before-retirement.json");
    let output = fixture.retire(&backup, &export, true, Some(&fixture.keyring));
    assert!(
        output.status.success(),
        "Confirmed synthetic retirement failed"
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["retirement_version"], 2);
    assert_eq!(report["schema_version"], 1);
    assert_eq!(
        report["removed_tables"],
        json!(["family_aliases", "family_channels"])
    );
    assert!(!exists(&fixture.db, "family_channels") && !exists(&fixture.db, "family_aliases"));
    assert!(
        preserved(&fixture.db) == before,
        "Retirement changed preserved identities/history"
    );
    let marker: (i64, String) = fixture
        .db
        .query_row(
            "SELECT version,source_revision FROM retired_features_v2 WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(marker, (2, REVISION.to_owned()));
    assert_private_file(&backup);
    assert_private_file(&export);
    let snapshot = Connection::open_with_flags(&backup, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert!(exists(&snapshot, "family_channels") && exists(&snapshot, "family_aliases"));
    assert!(
        preserved(&snapshot) == before,
        "Backup lost preserved identities/history"
    );
    assert_eq!(
        snapshot
            .query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert_eq!(
        snapshot
            .query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "delete"
    );
    let exported: Value = serde_json::from_slice(&fs::read(&export).unwrap()).unwrap();
    assert_eq!(exported["schema_version"], 1);
    assert_eq!(exported["source_revision"], REVISION);
    assert_eq!(exported["contains_secrets"], true);
    let digest = format!("{:x}", Sha256::digest(fs::read(&backup).unwrap()));
    assert_eq!(exported["backup_sha256"], digest);
    assert_eq!(report["backup_sha256"], digest);
    assert_eq!(exported["tables"]["family_channels"][0]["id"], "family:old");
    assert!(exported["tables"]["family_channels"][0]["data"]
        .as_str()
        .unwrap()
        .contains("private-organizer-token"));
    let retry_backup = fixture.path("after-retirement.sqlite");
    let retry_export = fixture.path("after-retirement.json");
    let retried = fixture.retire(&retry_backup, &retry_export, true, Some(&fixture.keyring));
    assert!(retried.status.success());
    let report: Value = serde_json::from_slice(&retried.stdout).unwrap();
    assert_eq!(report["removed_tables"], json!([]));
    assert_eq!(report["retirement_version"], 2);
    assert_private_file(&retry_backup);
    assert_private_file(&retry_export);
    assert!(
        preserved(&fixture.db) == before,
        "Fresh-path retry changed preserved data"
    );
}
