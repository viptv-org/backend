use super::*;

fn ready(root: &Path) -> (Connection, crate::secret_store::Vault) {
    let db = fixture(root);
    db.execute_batch("CREATE TABLE family_channels(id TEXT PRIMARY KEY,data TEXT NOT NULL);
        CREATE TABLE family_aliases(live_id TEXT PRIMARY KEY,channel_id TEXT REFERENCES family_channels(id));
        INSERT INTO family_channels VALUES('family:old','{\"secret_fixture\":\"private-organizer-token\"}');
        INSERT INTO family_aliases VALUES('iptv:1:1','family:old');").unwrap();
    apply(
        &root.join("source.sqlite"),
        &BTreeMap::from([(1, 11), (2, 22)]),
        &root.join("owned.sqlite"),
        &root.join("owned.json"),
        REVISION,
    )
    .unwrap();
    let vault = crate::secret_store::Vault::from_json(
        &json!({"active":"retirement","keys":{"retirement":STANDARD.encode([4u8;32])}}).to_string(),
    )
    .unwrap();
    encrypt_with_vault(
        &root.join("source.sqlite"),
        &root.join("sealed.sqlite"),
        &root.join("sealed.json"),
        REVISION,
        &vault,
    )
    .unwrap();
    (db, vault)
}
fn exists(db: &Connection, table: &str) -> bool {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [table],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn retirement_is_export_first_and_keeps_accounts_profiles_history_catalogs_matches_and_secrets() {
    let root = tempfile::tempdir().unwrap();
    let (db, vault) = ready(root.path());
    let report = retire_with_vault(
        &root.path().join("source.sqlite"),
        &root.path().join("before-retirement.sqlite"),
        &root.path().join("before-retirement.json"),
        REVISION,
        &vault,
    )
    .unwrap();
    assert_eq!(report["retirement_version"], 2);
    assert!(!report.to_string().contains("private-organizer-token"));
    let exported: Value =
        serde_json::from_reader(File::open(root.path().join("before-retirement.json")).unwrap())
            .unwrap();
    assert_eq!(exported["schema_version"], 1);
    assert_eq!(exported["contains_secrets"], true);
    assert_eq!(exported["source_revision"], REVISION);
    assert_eq!(exported["tables"]["family_channels"][0]["id"], "family:old");
    assert!(exported["tables"]["family_channels"][0]["data"]
        .as_str()
        .unwrap()
        .contains("private-organizer-token"));
    let before = open(&root.path().join("before-retirement.sqlite"), false).unwrap();
    assert!(exists(&before, "family_channels"));
    assert!(exists(&before, "provider_routes"));
    for table in RETIRED_TABLES {
        assert!(!exists(&db, table), "{table}");
    }
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
        v2::inspect_ownership(&db).unwrap().assignments,
        vec![(1, 11), (2, 22)]
    );
    assert_eq!(
        db.query_row(
            "SELECT access_hash FROM auth_sessions WHERE id='private-session'",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "private-auth-hash"
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM provider_credentials_v2", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        2
    );
    assert_eq!(
        db.query_row("SELECT version FROM retired_features_v2", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    let again = retire_with_vault(
        &root.path().join("source.sqlite"),
        &root.path().join("after.sqlite"),
        &root.path().join("after.json"),
        REVISION,
        &vault,
    )
    .unwrap();
    assert_eq!(again["removed_tables"], json!([]));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for name in ["before-retirement.sqlite", "before-retirement.json"] {
            assert_eq!(
                std::fs::metadata(root.path().join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
}

#[test]
fn missing_ownership_and_plaintext_are_refused_without_deleting_retired_configuration() {
    let root = tempfile::tempdir().unwrap();
    let db = fixture(root.path());
    let vault = crate::secret_store::Vault::from_json(
        &json!({"active":"retirement","keys":{"retirement":STANDARD.encode([4u8;32])}}).to_string(),
    )
    .unwrap();
    assert_eq!(
        retire_with_vault(
            &root.path().join("source.sqlite"),
            &root.path().join("unowned.sqlite"),
            &root.path().join("unowned.json"),
            REVISION,
            &vault
        )
        .unwrap_err(),
        "retirement_ownership_required"
    );
    assert!(exists(&db, "provider_routes"));
    assert!(!exists(&db, "retired_features_v2"));
    apply(
        &root.path().join("source.sqlite"),
        &BTreeMap::from([(1, 11), (2, 22)]),
        &root.path().join("owned.sqlite"),
        &root.path().join("owned.json"),
        REVISION,
    )
    .unwrap();
    assert_eq!(
        retire_with_vault(
            &root.path().join("source.sqlite"),
            &root.path().join("plaintext.sqlite"),
            &root.path().join("plaintext.json"),
            REVISION,
            &vault
        )
        .unwrap_err(),
        "retirement_encryption_required"
    );
    assert!(exists(&db, "provider_routes"));
    assert!(!exists(&db, "retired_features_v2"));
}

#[test]
fn bad_keys_existing_artifacts_and_unreviewed_foreign_dependencies_roll_back_retirement() {
    let root = tempfile::tempdir().unwrap();
    let (db, vault) = ready(root.path());
    let wrong = crate::secret_store::Vault::from_json(
        &json!({"active":"retirement","keys":{"retirement":STANDARD.encode([5u8;32])}}).to_string(),
    )
    .unwrap();
    assert_eq!(
        retire_with_vault(
            &root.path().join("source.sqlite"),
            &root.path().join("wrong.sqlite"),
            &root.path().join("wrong.json"),
            REVISION,
            &wrong
        )
        .unwrap_err(),
        "secret_authentication_failed"
    );
    assert!(exists(&db, "family_channels"));
    assert_eq!(
        retire_with_vault(
            &root.path().join("source.sqlite"),
            &root.path().join("wrong.sqlite"),
            &root.path().join("existing.json"),
            REVISION,
            &vault
        )
        .unwrap_err(),
        "artifact_path_must_be_new_and_writable"
    );
    db.execute_batch("CREATE TABLE unreviewed_extension(id INTEGER PRIMARY KEY,family_id TEXT REFERENCES family_channels(id));INSERT INTO unreviewed_extension VALUES(1,'family:old');").unwrap();
    assert_eq!(
        retire_with_vault(
            &root.path().join("source.sqlite"),
            &root.path().join("dependency.sqlite"),
            &root.path().join("dependency.json"),
            REVISION,
            &vault
        )
        .unwrap_err(),
        "retirement_dependency_requires_review"
    );
    assert!(exists(&db, "family_channels"));
    assert!(exists(&db, "family_aliases"));
    assert!(exists(&db, "provider_routes"));
    assert!(!exists(&db, "retired_features_v2"));
}

#[tokio::test]
async fn normal_boot_never_recreates_retired_tables() {
    let root = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let path = root.path().join("boot.sqlite");
    let app = crate::test_support::app_with_db(Connection::open(&path).unwrap());
    app.db.lock().unwrap().execute_batch("CREATE TABLE family_channels(id TEXT PRIMARY KEY,data TEXT NOT NULL);INSERT INTO family_channels VALUES('family:old','{}');").unwrap();
    drop(app);
    let vault = crate::secret_store::Vault::from_json(
        &json!({"active":"retirement","keys":{"retirement":STANDARD.encode([4u8;32])}}).to_string(),
    )
    .unwrap();
    retire_with_vault(
        &path,
        &root.path().join("boot-before.sqlite"),
        &root.path().join("boot-before.json"),
        REVISION,
        &vault,
    )
    .unwrap();
    let app = crate::test_support::app_with_db(Connection::open(path).unwrap());
    for table in RETIRED_TABLES {
        assert!(!exists(&app.db.lock().unwrap(), table));
    }
}
