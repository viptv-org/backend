//! The per-account session cap bounds stored grants without ever refusing a
//! sign-in for the account itself (backend#34), and expired credentials never
//! count toward it (app#97).
use super::*;

fn pair_device(db: &Connection, name: &str) -> Result<Value, ApiError> {
    let code = dispatch(
        db,
        "/auth/device/code",
        None,
        &HeaderMap::new(),
        &json!({"device_name":name}),
        SESSION_TOKEN,
    )?
    .0;
    dispatch(
        db,
        "/auth/device/approve",
        Some(owner()),
        &headers(),
        &json!({"user_code":code["user_code"]}),
        SESSION_TOKEN,
    )?;
    dispatch(
        db,
        "/auth/device/token",
        None,
        &HeaderMap::new(),
        &json!({"device_code":code["device_code"]}),
        SESSION_TOKEN,
    )
    .map(|(grant, _)| grant)
}

fn login(db: &Connection) -> Result<(Value, Option<(String, String)>), ApiError> {
    dispatch(
        db,
        "/auth/login",
        None,
        &headers(),
        &json!({"username":"owner","password":"a-long-owner-password"}),
        SESSION_TOKEN,
    )
}

fn seed(
    db: &Connection,
    id: &str,
    kind: &str,
    created_at: i64,
    refresh_expires: i64,
) {
    db.execute(
        "INSERT INTO auth_sessions(id,account_id,access_hash,refresh_hash,csrf_hash,profile_id,kind,device_name,access_expires,refresh_expires,created_at) VALUES(?1,1,?2,?3,?4,NULL,?5,'fixture',?6,?7,?8)",
        params![
            id,
            format!("access-{id}"),
            format!("refresh-{id}"),
            format!("csrf-{id}"),
            kind,
            now() + ACCESS,
            refresh_expires,
            created_at
        ],
    )
    .unwrap();
}

fn account_sessions(db: &Connection) -> i64 {
    db.query_row(
        "SELECT count(*) FROM auth_sessions WHERE account_id=1",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

fn exists(db: &Connection, id: &str) -> bool {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM auth_sessions WHERE id=?1)",
        [id],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn repeated_device_pairings_never_lock_the_account_out_of_sign_in() {
    let db = db();
    claim(&db);
    let mut last = Value::Null;
    for index in 0..MAX_SESSIONS_PER_ACCOUNT + 8 {
        last = pair_device(&db, &format!("Living room TV {index}"))
            .unwrap_or_else(|error| panic!("pairing {index:02} was refused: {error:?}"));
    }
    assert!(
        account_sessions(&db) <= MAX_SESSIONS_PER_ACCOUNT,
        "stored {} sessions",
        account_sessions(&db)
    );
    // The newest grant is the surviving one; only the oldest went.
    assert!(exists(&db, last["session_id"].as_str().unwrap()));
    // The account can still sign in on a browser, which is the only way to
    // list and revoke the stored TVs once the TV itself is gone.
    let (_, cookies) = login(&db).unwrap();
    assert!(cookies.is_some());
    assert!(account_sessions(&db) <= MAX_SESSIONS_PER_ACCOUNT);
    assert!(db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM auth_events WHERE kind='sessions_evicted' AND account_id=1)",
            [],
            |row| row.get::<_, bool>(0)
        )
        .unwrap());
}

#[test]
fn expired_sessions_never_count_against_the_account_cap() {
    let db = db();
    claim(&db);
    db.execute("DELETE FROM auth_sessions", []).unwrap();
    for index in 0..MAX_SESSIONS_PER_ACCOUNT {
        let kind = if index % 2 == 0 { "device" } else { "browser" };
        seed(
            &db,
            &format!("expired-{index:02}"),
            kind,
            index,
            now() - 1,
        );
    }
    // Both kinds of sign-in are admitted: expired rows are not sessions any
    // more, whichever kind left them behind.
    session(&db, 1, None, "device", "New TV").unwrap();
    session(&db, 1, None, "browser", "").unwrap();
    assert_eq!(account_sessions(&db), 2);
}

#[test]
fn a_sign_in_at_the_cap_evicts_the_oldest_session_of_its_own_kind_first() {
    let db = db();
    claim(&db);
    db.execute("DELETE FROM auth_sessions", []).unwrap();
    for index in 0..20 {
        seed(&db, &format!("device-{index:02}"), "device", index, DEVICE_EXPIRY);
    }
    for index in 0..12 {
        seed(
            &db,
            &format!("browser-{index:02}"),
            "browser",
            100 + index,
            now() + REFRESH,
        );
    }
    session(&db, 1, None, "browser", "").unwrap();
    assert_eq!(account_sessions(&db), MAX_SESSIONS_PER_ACCOUNT);
    // A browser sign-in prunes browser history, never a paired TV.
    assert!(!exists(&db, "browser-00"));
    assert!(exists(&db, "browser-01"));
    for index in 0..20 {
        assert!(exists(&db, &format!("device-{index:02}")));
    }
}

#[test]
fn a_device_sign_in_at_the_cap_falls_back_to_the_oldest_browser_session() {
    let db = db();
    claim(&db);
    db.execute("DELETE FROM auth_sessions", []).unwrap();
    for index in 0..MAX_SESSIONS_PER_ACCOUNT {
        seed(
            &db,
            &format!("browser-{index:02}"),
            "browser",
            index,
            now() + REFRESH,
        );
    }
    let grant = session(&db, 1, None, "device", "Living room TV").unwrap().0;
    assert_eq!(account_sessions(&db), MAX_SESSIONS_PER_ACCOUNT);
    assert!(exists(&db, grant["session_id"].as_str().unwrap()));
    assert!(!exists(&db, "browser-00"));
    assert!(exists(&db, "browser-01"));
}