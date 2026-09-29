//! Explicit, backup-first legacy ownership migration. No production invocation
//! is automatic. The export contains secrets and must remain private.
use crate::provider::v2;
use base64::{engine::general_purpose::STANDARD, Engine};
use rusqlite::{
    backup::{Backup, StepResult},
    types::ValueRef,
    Connection, OpenFlags, TransactionBehavior,
};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{BufWriter, Read, Write},
    path::Path,
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, &'static str>;
pub fn parse_owner_map(data: &[u8]) -> Result<BTreeMap<i64, i64>> {
    if data.len() > 1024 * 1024 {
        return Err("owner_map_too_large");
    }
    struct Owners;
    impl<'de> serde::de::Visitor<'de> for Owners {
        type Value = BTreeMap<i64, i64>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("unique positive provider/account ID pairs")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> std::result::Result<Self::Value, M::Error> {
            let mut owners = BTreeMap::new();
            while let Some((provider, account)) = map.next_entry::<String, i64>()? {
                let provider = provider
                    .parse::<i64>()
                    .ok()
                    .filter(|id| *id > 0)
                    .ok_or_else(|| serde::de::Error::custom("invalid provider id"))?;
                if account <= 0 || owners.insert(provider, account).is_some() {
                    return Err(serde::de::Error::custom(
                        "invalid or duplicate ownership entry",
                    ));
                }
            }
            Ok(owners)
        }
    }
    use serde::Deserializer;
    let mut json = serde_json::Deserializer::from_slice(data);
    let result = json
        .deserialize_map(Owners)
        .map_err(|_| "invalid_owner_map")?;
    json.end().map_err(|_| "invalid_owner_map")?;
    Ok(result)
}
const ADVANCED_TABLES: &[&str] = &[
    "addons",
    "addon_credentials_v2",
    "providers",
    "provider_credentials_v2",
    "provider_live",
    "provider_routes",
    "account_pools",
    "provider_pools",
    "family_settings",
    "family_recovery",
    "family_channels",
    "family_candidates",
    "family_aliases",
    "family_matching_settings",
    "family_match_overrides",
    "family_match_aliases",
    "family_verified_ids",
    "family_provider_groups",
    "family_match_results",
    "family_matching_revision",
    "live_category_rules",
    "live_policy_settings",
    "guide_settings",
    "guide_sources",
    "guide_channels",
    "guide_mappings",
    "guide_rejections",
    "health_settings",
    "candidate_health",
    "health_accounts",
    "catalog_schedule",
];

fn open(path: &Path, writable: bool) -> Result<Connection> {
    let flags = if writable {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    } else {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    };
    let db = Connection::open_with_flags(path, flags).map_err(|_| "database_unavailable")?;
    db.busy_timeout(Duration::from_secs(5))
        .map_err(|_| "database_unavailable")?;
    if writable {
        db.execute_batch("PRAGMA foreign_keys=ON")
            .map_err(|_| "database_unavailable")?;
    }
    Ok(db)
}
pub fn inspect(database: &Path) -> Result<Value> {
    let db = open(database, false)?;
    serde_json::to_value(v2::inspect_ownership(&db)?).map_err(|_| "report_unavailable")
}
pub fn inspect_addons(database: &Path) -> Result<Value> {
    crate::addon::credentials_v2::ownership(&open(database, false)?)
}
pub fn apply_addon_owners(
    database: &Path,
    owners: &BTreeMap<i64, i64>,
    backup_path: &Path,
    export_path: &Path,
    source_revision: &str,
) -> Result<Value> {
    migrate(database, backup_path, export_path, source_revision, |tx| {
        crate::addon::credentials_v2::assign_legacy(tx, owners)?;
        Ok(
            json!({"assigned_addon_count":owners.len(),"ownership":crate::addon::credentials_v2::ownership(tx)?}),
        )
    })
}
fn private_file(path: &Path) -> Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let parent = std::fs::metadata(
            path.parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new(".")),
        )
        .map_err(|_| "artifact_directory_unavailable")?;
        // Prevent another local user replacing the inode between create_new
        // and SQLite opening the destination by path.
        if parent.permissions().mode() & 0o077 != 0 || parent.uid() != unsafe { libc::geteuid() } {
            return Err("artifact_directory_must_be_private_and_owned");
        }
    }
    let mut options = OpenOptions::new();
    options.write(true).read(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|_| "artifact_path_must_be_new_and_writable")
}
fn sync_parent(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        File::open(
            path.parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new(".")),
        )
        .and_then(|directory| directory.sync_all())
        .map_err(|_| "artifact_directory_sync_failed")?;
    }
    Ok(())
}
fn checksum(file: &mut File) -> Result<String> {
    let mut hash = Sha256::new();
    let mut bytes = [0u8; 65536];
    loop {
        let count = file.read(&mut bytes).map_err(|_| "backup_unreadable")?;
        if count == 0 {
            break;
        }
        hash.update(&bytes[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn write_json(writer: &mut impl Write, value: &impl serde::Serialize) -> Result<()> {
    serde_json::to_writer(writer, value).map_err(|_| "export_write_failed")
}
fn bytes(writer: &mut impl Write, data: &[u8]) -> Result<()> {
    writer.write_all(data).map_err(|_| "export_write_failed")
}
fn export(db: &Connection, file: &mut File, revision: &str, backup_hash: &str) -> Result<()> {
    let mut writer = BufWriter::new(file);
    bytes(
        &mut writer,
        b"{\"schema_version\":1,\"contains_secrets\":true,\"source_revision\":",
    )?;
    write_json(&mut writer, &revision)?;
    bytes(&mut writer, b",\"backup_sha256\":")?;
    write_json(&mut writer, &backup_hash)?;
    bytes(&mut writer, b",\"tables\":{")?;
    let mut first_table = true;
    for &table in ADVANCED_TABLES {
        let exists: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                [table],
                |row| row.get(0),
            )
            .map_err(|_| "export_read_failed")?;
        if !exists {
            continue;
        }
        if !first_table {
            bytes(&mut writer, b",")?;
        }
        first_table = false;
        write_json(&mut writer, &table)?;
        bytes(&mut writer, b":[")?;
        // The identifier comes exclusively from the static table allowlist.
        let mut statement = db
            .prepare(&format!("SELECT * FROM \"{table}\" ORDER BY rowid"))
            .map_err(|_| "export_read_failed")?;
        let columns: Vec<_> = statement
            .column_names()
            .iter()
            .map(|name| name.to_string())
            .collect();
        let mut rows = statement.query([]).map_err(|_| "export_read_failed")?;
        let mut first_row = true;
        while let Some(row) = rows.next().map_err(|_| "export_read_failed")? {
            let mut value = Map::new();
            for (index, name) in columns.iter().enumerate() {
                let field = match row.get_ref(index).map_err(|_| "export_read_failed")? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(value) => json!(value),
                    ValueRef::Real(value) => Value::Number(
                        serde_json::Number::from_f64(value).ok_or("export_invalid_number")?,
                    ),
                    ValueRef::Text(value) => Value::String(
                        std::str::from_utf8(value)
                            .map_err(|_| "export_invalid_text")?
                            .into(),
                    ),
                    ValueRef::Blob(value) => {
                        json!({"encoding":"base64","data":STANDARD.encode(value)})
                    }
                };
                value.insert(name.clone(), field);
            }
            if !first_row {
                bytes(&mut writer, b",")?;
            }
            first_row = false;
            write_json(&mut writer, &value)?;
        }
        bytes(&mut writer, b"]")?;
    }
    bytes(&mut writer, b"}}\n")?;
    writer.flush().map_err(|_| "export_write_failed")
}

/// Caller must explicitly approve the owner map and stop normal writers first.
/// BEGIN IMMEDIATE additionally excludes racing writers across snapshot/export/
/// assignment. The SQLite backup API captures WAL contents; no raw DB copy.
pub fn apply(
    database: &Path,
    owners: &BTreeMap<i64, i64>,
    backup_path: &Path,
    export_path: &Path,
    source_revision: &str,
) -> Result<Value> {
    migrate(
        database,
        backup_path,
        export_path,
        source_revision,
        |transaction| {
            v2::init_in_transaction(transaction)?;
            v2::assign_legacy(transaction, owners)?;
            for account in owners.values().collect::<std::collections::BTreeSet<_>>() {
                v2::live_catalog(transaction, *account, None)?;
            }
            Ok(
                json!({"assigned_count":owners.len(),"ownership":v2::inspect_ownership(transaction)?}),
            )
        },
    )
}

/// Offline only. Artifacts retain old plaintext and must stay private.
pub fn encrypt(
    database: &Path,
    backup_path: &Path,
    export_path: &Path,
    source_revision: &str,
) -> Result<Value> {
    let vault =
        crate::secret_store::Vault::from_environment()?.ok_or("secret_store_not_configured")?;
    encrypt_with_vault(database, backup_path, export_path, source_revision, &vault)
}
fn encrypt_with_vault(
    database: &Path,
    backup_path: &Path,
    export_path: &Path,
    source_revision: &str,
    vault: &crate::secret_store::Vault,
) -> Result<Value> {
    let report = migrate(
        database,
        backup_path,
        export_path,
        source_revision,
        |transaction| {
            v2::init_in_transaction(transaction)?;
            let count = crate::provider::credentials_v2::encrypt_legacy(transaction, vault)?;
            Ok(json!({"encrypted_count":count}))
        },
    )?;
    compact_encrypted(database)?;
    Ok(report)
}
pub fn encrypt_addons(
    database: &Path,
    backup_path: &Path,
    export_path: &Path,
    source_revision: &str,
) -> Result<Value> {
    let vault =
        crate::secret_store::Vault::from_environment()?.ok_or("secret_store_not_configured")?;
    encrypt_addons_with_vault(database, backup_path, export_path, source_revision, &vault)
}
fn encrypt_addons_with_vault(
    database: &Path,
    backup_path: &Path,
    export_path: &Path,
    source_revision: &str,
    vault: &crate::secret_store::Vault,
) -> Result<Value> {
    let report = migrate(database, backup_path, export_path, source_revision, |tx| {
        Ok(json!({"encrypted_addon_count":crate::addon::credentials_v2::encrypt_legacy(tx,vault)?}))
    })?;
    compact_encrypted(database)?;
    Ok(report)
}
fn compact_encrypted(database: &Path) -> Result<()> {
    // Logical updates alone leave old credentials in SQLite pages/WAL. Require
    // offline checkpoint and compaction before reporting encryption success.
    let db = open(database, true).map_err(|_| "encryption_committed_cleanup_required")?;
    for vacuum in [true, false] {
        let busy: i64 = db
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))
            .map_err(|_| "encryption_committed_cleanup_required")?;
        if busy != 0 {
            return Err("encryption_committed_cleanup_required");
        }
        if vacuum {
            db.execute_batch("PRAGMA secure_delete=ON; VACUUM;")
                .map_err(|_| "encryption_committed_cleanup_required")?;
            // VACUUM may renumber implicit rowids. FTS uses provider_vod.rowid,
            // not its stable public TEXT id, so regenerate the derived index.
            let indexed:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='provider_vod_search_v2')",[],|r|r.get(0)).map_err(|_|"encryption_committed_cleanup_required")?;
            if indexed {
                db.execute(
                    "INSERT INTO provider_vod_search_v2(provider_vod_search_v2) VALUES('rebuild')",
                    [],
                )
                .map_err(|_| "encryption_committed_cleanup_required")?;
            }
        }
    }
    Ok(())
}

fn migrate(
    database: &Path,
    backup_path: &Path,
    export_path: &Path,
    source_revision: &str,
    action: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<Value>,
) -> Result<Value> {
    if source_revision.len() != 40 || !source_revision.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("source_revision_required");
    }
    let mut db = open(database, true)?;
    let transaction = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| "database_busy_stop_writers")?;
    let reader = open(database, false)?;
    let mut backup_file = private_file(backup_path)?;
    let mut destination = open(backup_path, true)?;
    {
        let backup = Backup::new(&reader, &mut destination).map_err(|_| "backup_failed")?;
        let started = Instant::now();
        loop {
            if matches!(
                backup.step(256).map_err(|_| "backup_failed")?,
                StepResult::Done
            ) {
                break;
            }
            if started.elapsed() > Duration::from_secs(120) {
                return Err("backup_timed_out");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let integrity: String = destination
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(|_| "backup_integrity_failed")?;
    if integrity != "ok" {
        return Err("backup_integrity_failed");
    }
    destination
        .execute_batch("PRAGMA journal_mode=DELETE")
        .map_err(|_| "backup_failed")?;
    destination.close().map_err(|_| "backup_failed")?;
    backup_file.sync_all().map_err(|_| "backup_sync_failed")?;
    let backup_hash = checksum(&mut backup_file)?;
    let mut exported = private_file(export_path)?;
    export(&reader, &mut exported, source_revision, &backup_hash)?;
    exported.sync_all().map_err(|_| "export_sync_failed")?;
    sync_parent(backup_path)?;
    sync_parent(export_path)?;
    // No source mutation occurs until both private artifacts are durable.
    let mut report = action(&transaction)?;
    transaction
        .commit()
        .map_err(|_| "migration_commit_failed")?;
    report["schema_version"] = json!(1);
    report["backup_sha256"] = json!(backup_hash);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    const REVISION: &str = "1111111111111111111111111111111111111111";
    #[test]
    fn addon_ownership_and_encryption_preserve_ids_and_require_explicit_mapping() {
        let root = tempfile::tempdir().unwrap();
        let source = fixture(root.path());
        let database = root.path().join("source.sqlite");
        source.execute_batch("CREATE TABLE addons(id INTEGER PRIMARY KEY,name TEXT NOT NULL,manifest_url TEXT UNIQUE NOT NULL,enabled INTEGER NOT NULL DEFAULT 1,manifest TEXT NOT NULL);").unwrap();
        let manifest=json!({"id":"fixture","name":"Fixture","resources":[],"logo":"https://art.invalid/addon-private-token"}).to_string();
        source.execute("INSERT INTO addons VALUES(7,'Fixture','https://fixture.invalid/addon-private-token/manifest.json',1,?1)",[manifest.clone()]).unwrap();
        assert_eq!(inspect_addons(&database).unwrap()["unassigned"], json!([7]));
        assert!(apply_addon_owners(
            &database,
            &BTreeMap::new(),
            &root.path().join("rejected.sqlite"),
            &root.path().join("rejected.json"),
            REVISION
        )
        .is_err());
        assert!(!source
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('addons') WHERE name='account_id')",
                [],
                |r| r.get::<_, bool>(0)
            )
            .unwrap());
        let mapped = apply_addon_owners(
            &database,
            &BTreeMap::from([(7, 11)]),
            &root.path().join("owners.sqlite"),
            &root.path().join("owners.json"),
            REVISION,
        )
        .unwrap();
        assert_eq!(mapped["ownership"]["assignments"], json!([[7, 11]]));
        let vault = crate::secret_store::Vault::from_json(
            &json!({"active":"fixture","keys":{"fixture":STANDARD.encode([7u8;32])}}).to_string(),
        )
        .unwrap();
        v2::init(&source).unwrap();
        // Model a stale derived index; cleanup must rebuild from authoritative IDs.
        source
            .execute(
                "INSERT INTO provider_vod_search_v2(provider_vod_search_v2) VALUES('delete-all')",
                [],
            )
            .unwrap();
        let report = encrypt_addons_with_vault(
            &database,
            &root.path().join("before.sqlite"),
            &root.path().join("before.json"),
            REVISION,
            &vault,
        )
        .unwrap();
        assert_eq!(report["encrypted_addon_count"], 1);
        assert_eq!(source.query_row("SELECT count(*) FROM provider_vod_search_v2 WHERE provider_vod_search_v2 MATCH 'Fixture'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
        assert!(!report.to_string().contains("addon-private-token"));
        let before = open(&root.path().join("before.sqlite"), false).unwrap();
        assert_eq!(
            before
                .query_row("SELECT manifest FROM addons WHERE id=7", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            manifest
        );
        assert!(std::fs::read_to_string(root.path().join("before.json"))
            .unwrap()
            .contains("addon-private-token"));
        assert_eq!(
            source
                .query_row("SELECT account_id FROM addons WHERE id=7", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            11
        );
        assert_eq!(
            source
                .query_row("SELECT manifest_url FROM addons WHERE id=7", [], |r| r
                    .get::<_, String>(
                    0
                ))
                .unwrap(),
            "sealed:addon:7"
        );
        assert_eq!(
            source
                .query_row(
                    "SELECT position FROM progress WHERE profile_id=71",
                    [],
                    |r| r.get::<_, f64>(0)
                )
                .unwrap(),
            123.5
        );
        for path in [&database, &root.path().join("source.sqlite-wal")] {
            assert!(!std::fs::read(path)
                .unwrap_or_default()
                .windows(b"addon-private-token".len())
                .any(|v| v == b"addon-private-token"));
        }
        assert_eq!(
            encrypt_addons_with_vault(
                &database,
                &root.path().join("again.sqlite"),
                &root.path().join("again.json"),
                REVISION,
                &vault
            )
            .unwrap()["encrypted_addon_count"],
            0
        );
    }
    #[test]
    fn encryption_rolls_back_every_provider_when_later_credentials_are_invalid() {
        let root = tempfile::tempdir().unwrap();
        let source = fixture(root.path());
        let database = root.path().join("source.sqlite");
        apply(
            &database,
            &BTreeMap::from([(1, 11), (2, 22)]),
            &root.path().join("owners.sqlite"),
            &root.path().join("owners.json"),
            REVISION,
        )
        .unwrap();
        source
            .execute("UPDATE providers SET username='' WHERE id=2", [])
            .unwrap();
        let vault = crate::secret_store::Vault::from_json(
            &json!({"active":"test","keys":{"test":STANDARD.encode([7u8;32])}}).to_string(),
        )
        .unwrap();
        assert_eq!(
            encrypt_with_vault(
                &database,
                &root.path().join("before.sqlite"),
                &root.path().join("before.json"),
                REVISION,
                &vault
            )
            .unwrap_err(),
            "invalid_legacy_provider_credentials"
        );
        assert_eq!(
            source
                .query_row("SELECT count(*) FROM provider_credentials_v2", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            source
                .query_row("SELECT password FROM providers WHERE id=1", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "fixture-password"
        );
        assert_eq!(
            source
                .query_row(
                    "SELECT credentials_version FROM providers WHERE id=1",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert!(root.path().join("before.sqlite").exists());
        assert!(root.path().join("before.json").exists());
    }
    #[test]
    fn encryption_is_backup_first_preserves_identity_and_compacts_old_plaintext() {
        let root = tempfile::tempdir().unwrap();
        let source = fixture(root.path());
        let database = root.path().join("source.sqlite");
        apply(
            &database,
            &BTreeMap::from([(1, 11), (2, 22)]),
            &root.path().join("owners.sqlite"),
            &root.path().join("owners.json"),
            REVISION,
        )
        .unwrap();
        source
            .execute(
                "INSERT INTO provider_cache VALUES(1,'old',9999999999,'fixture-password')",
                [],
            )
            .unwrap();
        let vault = crate::secret_store::Vault::from_json(
            &json!({"active":"test","keys":{"test":STANDARD.encode([7u8;32])}}).to_string(),
        )
        .unwrap();
        let backup = root.path().join("plaintext.sqlite");
        let export = root.path().join("plaintext.json");
        let report = encrypt_with_vault(&database, &backup, &export, REVISION, &vault).unwrap();
        assert_eq!(report["encrypted_count"], 2);
        assert!(!report.to_string().contains("fixture-password"));
        assert!(std::fs::read_to_string(&export)
            .unwrap()
            .contains("fixture-password"));
        let before = open(&backup, false).unwrap();
        assert_eq!(
            before
                .query_row("SELECT password FROM providers WHERE id=1", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "fixture-password"
        );
        assert_eq!(source.query_row("SELECT count(*) FROM providers WHERE url='' AND username='' AND password='' AND credentials_version=1",[],|r|r.get::<_,i64>(0)).unwrap(),2);
        assert_eq!(
            source
                .query_row(
                    "SELECT position FROM progress WHERE profile_id=71",
                    [],
                    |r| r.get::<_, f64>(0)
                )
                .unwrap(),
            123.5
        );
        assert_eq!(
            source
                .query_row(
                    "SELECT metadata_id FROM provider_matches WHERE vod_id='vod:1:1'",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "tt1234567"
        );
        assert_eq!(
            source
                .query_row("SELECT count(*) FROM provider_cache", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        for path in [&database, &root.path().join("source.sqlite-wal")] {
            let bytes = std::fs::read(path).unwrap_or_default();
            assert!(!bytes
                .windows(b"fixture-password".len())
                .any(|v| v == b"fixture-password"));
            assert!(!bytes
                .windows(b"fixture-user".len())
                .any(|v| v == b"fixture-user"));
        }
        let second = encrypt_with_vault(
            &database,
            &root.path().join("again.sqlite"),
            &root.path().join("again.json"),
            REVISION,
            &vault,
        )
        .unwrap();
        assert_eq!(second["encrypted_count"], 0);
        let wrong = crate::secret_store::Vault::from_json(
            &json!({"active":"test","keys":{"test":STANDARD.encode([8u8;32])}}).to_string(),
        )
        .unwrap();
        assert_eq!(
            encrypt_with_vault(
                &database,
                &root.path().join("wrong.sqlite"),
                &root.path().join("wrong.json"),
                REVISION,
                &wrong
            )
            .unwrap_err(),
            "secret_authentication_failed"
        );
    }
    fn fixture(root: &Path) -> Connection {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let db = Connection::open(root.join("source.sqlite")).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
            CREATE TABLE auth_accounts(id INTEGER PRIMARY KEY,disabled INTEGER NOT NULL); INSERT INTO auth_accounts VALUES(11,0),(22,0),(33,1);
            CREATE TABLE profiles(id INTEGER PRIMARY KEY,name TEXT); INSERT INTO profiles VALUES(71,'Fixture profile');
            CREATE TABLE progress(profile_id INTEGER,id TEXT,position REAL); INSERT INTO progress VALUES(71,'tt1234567',123.5);
            CREATE TABLE auth_sessions(id TEXT,access_hash TEXT); INSERT INTO auth_sessions VALUES('private-session','private-auth-hash');").unwrap();
        crate::provider::init(&db).unwrap();
        for id in [1, 2] {
            db.execute("INSERT INTO providers(id,name,url,username,password) VALUES(?1,'Fixture','http://fixture.invalid','fixture-user','fixture-password')", [id]).unwrap();
        }
        db.execute(
            "INSERT INTO provider_routes(provider_id,warp) VALUES(1,1)",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO provider_vod(id,provider_id,stream_id,kind,name,normalized,extension) VALUES('vod:1:1',1,'1','movie','Fixture movie','fixture movie','mp4')", []).unwrap();
        db.execute(
            "INSERT INTO provider_matches VALUES('vod:1:1','tt1234567','movie')",
            [],
        )
        .unwrap();
        db
    }
    #[test]
    fn backup_export_and_assignment_preserve_identity_and_viewing_history() {
        let root = tempfile::tempdir().unwrap();
        let source = fixture(root.path());
        let database = root.path().join("source.sqlite");
        let before = inspect(&database).unwrap();
        assert_eq!(before["unassigned"], json!([1, 2]));
        assert!(!before.to_string().contains("fixture-password"));
        let backup = root.path().join("backup.sqlite");
        let export = root.path().join("advanced.json");
        let report = apply(
            &database,
            &BTreeMap::from([(1, 11), (2, 22)]),
            &backup,
            &export,
            REVISION,
        )
        .unwrap();
        assert_eq!(report["assigned_count"], 2);
        assert_eq!(
            inspect(&database).unwrap()["assignments"],
            json!([[1, 11], [2, 22]])
        );
        let restored = open(&backup, false).unwrap();
        assert_eq!(
            v2::inspect_ownership(&restored).unwrap().unassigned,
            vec![1, 2]
        );
        for db in [&source, &restored] {
            assert_eq!(
                db.query_row(
                    "SELECT position FROM progress WHERE profile_id=71 AND id='tt1234567'",
                    [],
                    |row| row.get::<_, f64>(0)
                )
                .unwrap(),
                123.5
            );
            assert_eq!(
                db.query_row(
                    "SELECT metadata_id FROM provider_matches WHERE vod_id='vod:1:1'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
                "tt1234567"
            );
            assert_eq!(
                db.query_row("SELECT password FROM providers WHERE id=1", [], |row| {
                    row.get::<_, String>(0)
                })
                .unwrap(),
                "fixture-password"
            );
        }
        let document: Value = serde_json::from_slice(&std::fs::read(&export).unwrap()).unwrap();
        assert_eq!(document["source_revision"], REVISION);
        assert_eq!(document["backup_sha256"], report["backup_sha256"]);
        assert_eq!(document["contains_secrets"], true);
        assert_eq!(document["tables"]["provider_routes"][0]["warp"], 1);
        assert_eq!(document["tables"]["providers"].as_array().unwrap().len(), 2);
        assert!(document["tables"].get("auth_sessions").is_none());
        assert!(document["tables"].get("progress").is_none());
        assert!(!document.to_string().contains("private-auth-hash"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [&backup, &export] {
                assert_eq!(
                    std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
        }
        assert!(
            apply(&database, &BTreeMap::new(), &backup, &export, REVISION).is_err(),
            "artifacts must never be overwritten"
        );
    }
    #[test]
    fn invalid_owner_map_rolls_back_schema_and_keeps_private_artifacts() {
        let root = tempfile::tempdir().unwrap();
        let _source = fixture(root.path());
        let database = root.path().join("source.sqlite");
        let result = apply(
            &database,
            &BTreeMap::from([(1, 11), (2, 33)]),
            &root.path().join("backup.sqlite"),
            &root.path().join("advanced.json"),
            REVISION,
        );
        assert_eq!(result, Err("invalid_legacy_provider_owner"));
        assert_eq!(inspect(&database).unwrap()["unassigned"], json!([1, 2]));
        assert!(root.path().join("backup.sqlite").exists());
        assert!(root.path().join("advanced.json").exists());
        let db = open(&database, false).unwrap();
        assert!(!db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='provider_ownership')",
                [],
                |row| row.get::<_, bool>(0)
            )
            .unwrap());
    }
    #[test]
    fn duplicate_or_nonpositive_owner_ids_are_rejected() {
        for input in [
            r#"{"1":11,"1":22}"#,
            r#"{"1":11,"01":22}"#,
            r#"{"0":11}"#,
            r#"{"1":0}"#,
            r#"{"1":11} {}"#,
        ] {
            assert!(parse_owner_map(input.as_bytes()).is_err());
        }
        assert_eq!(
            parse_owner_map(br#"{"1":11,"2":22}"#).unwrap(),
            BTreeMap::from([(1, 11), (2, 22)])
        );
    }
}
