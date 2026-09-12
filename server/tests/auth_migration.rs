//! Upgrade compatibility: adding household authentication must not rewrite user data.
use rusqlite::Connection;
use std::time::Duration;
use viptv_server::{
    playback::{Config, PlaybackManager},
    App,
};

fn initialize(db: Connection, root: &std::path::Path) -> App {
    let playback = PlaybackManager::new(Config {
        ffmpeg: "missing-test-ffmpeg".into(),
        ffprobe: "missing-test-ffprobe".into(),
        root: root.join("hls"),
        max_sessions: 2,
        ttl: Duration::from_secs(30),
    });
    App::new(db, reqwest::Client::new(), playback).unwrap()
}

#[tokio::test]
async fn additive_auth_upgrade_preserves_existing_profile_history_and_favorites() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("existing.sqlite");
    {
        let db = Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE profiles(id INTEGER PRIMARY KEY,name TEXT NOT NULL);
        CREATE TABLE favorites(profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,id TEXT NOT NULL,type TEXT NOT NULL,name TEXT NOT NULL,poster TEXT,PRIMARY KEY(profile_id,type,id));
        CREATE TABLE progress(profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,id TEXT NOT NULL,type TEXT NOT NULL,name TEXT NOT NULL,poster TEXT,position REAL NOT NULL,duration REAL NOT NULL,updated_at INTEGER NOT NULL,PRIMARY KEY(profile_id,type,id));
        CREATE TABLE auth_accounts(id INTEGER PRIMARY KEY,username TEXT NOT NULL UNIQUE,password_hash TEXT NOT NULL,role TEXT NOT NULL,recovery_hash TEXT NOT NULL,disabled INTEGER NOT NULL DEFAULT 0,created_at INTEGER NOT NULL);
        CREATE TABLE auth_profiles(account_id INTEGER NOT NULL,profile_id INTEGER NOT NULL,PRIMARY KEY(account_id,profile_id));
        INSERT INTO profiles(id,name) VALUES(1,'Default');
        INSERT INTO profiles(id,name) VALUES(42,'Existing Viewer');
        INSERT INTO favorites(profile_id,id,type,name,poster) VALUES(42,'tt-original','movie','Existing favorite','https://images.example/original.jpg');
        INSERT INTO progress(profile_id,id,type,name,poster,position,duration,updated_at) VALUES(42,'tt-original','movie','Existing title',NULL,13.554,8888.0,1700000000);
        INSERT INTO auth_accounts(id,username,password_hash,role,recovery_hash,disabled,created_at) VALUES(7,'existing-owner','kept-password-hash','owner','kept-recovery-hash',0,1600000000);
        INSERT INTO auth_profiles(account_id,profile_id) VALUES(7,42);").unwrap();
    }
    for _ in 0..3 {
        let app = initialize(Connection::open(&path).unwrap(), root.path());
        {
            let db = app.db.lock().unwrap();
            assert_eq!(
                db.query_row("SELECT name FROM profiles WHERE id=42", [], |row| row
                    .get::<_, String>(0))
                    .unwrap(),
                "Existing Viewer"
            );
            let progress = db.query_row("SELECT position,duration,updated_at,name FROM progress WHERE profile_id=42 AND id='tt-original'", [], |row| Ok((row.get::<_, f64>(0)?, row.get::<_, f64>(1)?, row.get::<_, i64>(2)?, row.get::<_, String>(3)?))).unwrap();
            assert_eq!(
                progress,
                (13.554, 8888.0, 1700000000, "Existing title".into())
            );
            assert_eq!(
                db.query_row(
                    "SELECT poster FROM favorites WHERE profile_id=42 AND id='tt-original'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
                "https://images.example/original.jpg"
            );
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM profiles", [], |row| row
                    .get::<_, i64>(0))
                    .unwrap(),
                2
            );
            assert_eq!(
                db.query_row(
                    "SELECT account_id FROM profile_owners WHERE profile_id=42",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
                7
            );
            assert_eq!(
                db.query_row("SELECT username||':'||password_hash||':'||recovery_hash FROM auth_accounts WHERE id=7", [], |row| row.get::<_, String>(0)).unwrap(),
                "existing-owner:kept-password-hash:kept-recovery-hash"
            );
            assert!(!db
                .query_row(
                    "SELECT presentation_complete FROM profiles WHERE id=42",
                    [],
                    |row| row.get::<_, bool>(0)
                )
                .unwrap());
            assert_eq!(
                db.query_row("SELECT MAX(version) FROM schema_migrations", [], |row| row
                    .get::<_, i64>(
                    0
                ))
                .unwrap(),
                4
            );
            assert_eq!(
                db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                    .unwrap(),
                "ok"
            );
        }
        app.playback.shutdown().await;
    }
}

#[test]
fn ambiguous_historical_profile_grants_fail_without_reassigning_or_losing_history() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("PRAGMA foreign_keys=ON;
        CREATE TABLE profiles(id INTEGER PRIMARY KEY,name TEXT NOT NULL);
        CREATE TABLE favorites(profile_id INTEGER NOT NULL,id TEXT NOT NULL,type TEXT NOT NULL,name TEXT NOT NULL,poster TEXT,PRIMARY KEY(profile_id,type,id));
        CREATE TABLE progress(profile_id INTEGER NOT NULL,id TEXT NOT NULL,type TEXT NOT NULL,name TEXT NOT NULL,poster TEXT,position REAL NOT NULL,duration REAL NOT NULL,updated_at INTEGER NOT NULL,PRIMARY KEY(profile_id,type,id));
        CREATE TABLE auth_accounts(id INTEGER PRIMARY KEY,username TEXT NOT NULL UNIQUE,password_hash TEXT NOT NULL,role TEXT NOT NULL,recovery_hash TEXT NOT NULL,disabled INTEGER NOT NULL DEFAULT 0,created_at INTEGER NOT NULL);
        CREATE TABLE auth_profiles(account_id INTEGER NOT NULL,profile_id INTEGER NOT NULL,PRIMARY KEY(account_id,profile_id));
        INSERT INTO profiles VALUES(42,'Shared historical profile');
        INSERT INTO favorites VALUES(42,'tt-kept','movie','Kept favorite',NULL);
        INSERT INTO progress VALUES(42,'tt-kept','movie','Kept progress',NULL,13.554,8888,1700000000);
        INSERT INTO auth_accounts VALUES(1,'first','hash','member','recovery',0,0);
        INSERT INTO auth_accounts VALUES(2,'second','hash','member','recovery',0,0);
        INSERT INTO auth_profiles VALUES(1,42);
        INSERT INTO auth_profiles VALUES(2,42);")
        .unwrap();

    let error = viptv_server::auth::init(&db).unwrap_err().to_string();
    assert!(error.contains("ambiguous legacy account ownership"));
    assert_eq!(
        db.query_row("SELECT name FROM profiles WHERE id=42", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "Shared historical profile"
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM favorites WHERE profile_id=42",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row(
            "SELECT position FROM progress WHERE profile_id=42",
            [],
            |row| row.get::<_, f64>(0)
        )
        .unwrap(),
        13.554
    );
    assert!(!db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='profile_owners')",
            [],
            |row| row.get::<_, bool>(0)
        )
        .unwrap());
    assert!(!db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_migrations')",
            [],
            |row| row.get::<_, bool>(0)
        )
        .unwrap());
    let columns = db
        .prepare("PRAGMA table_info(profiles)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(columns, vec!["id", "name"]);
}

#[test]
fn inconsistent_preexisting_profile_owner_rolls_back_without_partial_dml() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "PRAGMA foreign_keys=ON;
         CREATE TABLE profiles(id INTEGER PRIMARY KEY,name TEXT NOT NULL);
         CREATE TABLE auth_accounts(id INTEGER PRIMARY KEY,username TEXT NOT NULL UNIQUE,name TEXT NOT NULL DEFAULT '',password_hash TEXT NOT NULL,role TEXT NOT NULL,recovery_hash TEXT NOT NULL,disabled INTEGER NOT NULL DEFAULT 0,created_at INTEGER NOT NULL);
         CREATE TABLE auth_profiles(account_id INTEGER NOT NULL,profile_id INTEGER NOT NULL,PRIMARY KEY(account_id,profile_id));
         CREATE TABLE profile_owners(profile_id INTEGER PRIMARY KEY,account_id INTEGER NOT NULL,created_at INTEGER NOT NULL);
         INSERT INTO profiles VALUES(42,'Kept');
         INSERT INTO auth_accounts VALUES(1,'first','First','hash','member','recovery-1',0,0);
         INSERT INTO auth_accounts VALUES(2,'second','Second','hash','member','recovery-2',0,0);
         INSERT INTO auth_profiles VALUES(1,42);
         INSERT INTO profile_owners VALUES(42,2,123);",
    )
    .unwrap();

    let error = viptv_server::auth::init(&db).unwrap_err().to_string();
    assert!(error.contains("inconsistent account ownership"));
    assert_eq!(
        db.query_row(
            "SELECT account_id||':'||created_at FROM profile_owners WHERE profile_id=42",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "2:123"
    );
    assert!(!db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_migrations')",
            [],
            |row| row.get::<_, bool>(0)
        )
        .unwrap());
    let columns = db
        .prepare("PRAGMA table_info(profiles)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(columns, vec!["id", "name"]);
}
