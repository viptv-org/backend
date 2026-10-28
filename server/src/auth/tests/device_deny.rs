use super::*;

fn member_principal(id: i64) -> Principal {
    Principal::Account {
        account_id: id,
        role: "member".into(),
        profile_id: None,
        session_id: None,
    }
}

// Denying leaves a tombstone for the polling device (backend#44), so "gone"
// means no longer usable rather than deleted.
fn pairing_usable(db: &Connection, user_code: &str) -> bool {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM auth_pairings WHERE code_hash=?1 AND denied=0)",
        [hash(user_code)],
        |row| row.get::<_, bool>(0),
    )
    .unwrap()
}

#[test]
fn deny_retires_live_pending_codes_for_any_authenticated_user() {
    let db = db();
    claim(&db);
    let code = dispatch(
        &db,
        "/auth/device/code",
        None,
        &HeaderMap::new(),
        &json!({"device_name":"Owner TV"}),
        SESSION_TOKEN,
    )
    .unwrap()
    .0;
    // Pending, unowned codes are deniable by any authenticated user: the
    // browser user is exactly the person the TV asked to approve or deny.
    dispatch(
        &db,
        "/auth/device/deny",
        Some(owner()),
        &headers(),
        &json!({"user_code": code["user_code"]}),
        SESSION_TOKEN,
    )
    .unwrap();
    assert!(!pairing_usable(&db, code["user_code"].as_str().unwrap()));
}

#[test]
fn deny_refuses_already_approved_pairings_from_foreign_accounts() {
    let db = db();
    claim(&db);
    let member = dispatch(
        &db,
        "/auth/register",
        None,
        &headers(),
        &json!({"username":"denymember","password":"member-long-password"}),
        SESSION_TOKEN,
    )
    .unwrap()
    .0;
    let member_id = identifier(&member, "account_id").unwrap();
    let code = dispatch(
        &db,
        "/auth/device/code",
        None,
        &HeaderMap::new(),
        &json!({"device_name":"Member TV"}),
        SESSION_TOKEN,
    )
    .unwrap()
    .0;
    dispatch(
        &db,
        "/auth/device/approve",
        Some(member_principal(member_id)),
        &headers(),
        &json!({"user_code": code["user_code"]}),
        SESSION_TOKEN,
    )
    .unwrap();
    // A different account cannot deny an already-approved foreign pairing.
    let err = dispatch(
        &db,
        "/auth/device/deny",
        Some(owner()),
        &headers(),
        &json!({"user_code": code["user_code"]}),
        SESSION_TOKEN,
    )
    .unwrap_err();
    assert_eq!(err.0, StatusCode::FORBIDDEN);
    assert!(pairing_usable(&db, code["user_code"].as_str().unwrap()));
    // The owning account can deny its own approved pairing.
    dispatch(
        &db,
        "/auth/device/deny",
        Some(member_principal(member_id)),
        &headers(),
        &json!({"user_code": code["user_code"]}),
        SESSION_TOKEN,
    )
    .unwrap();
    assert!(!pairing_usable(&db, code["user_code"].as_str().unwrap()));
}

#[test]
fn deny_refuses_expired_and_unknown_codes() {
    let db = db();
    claim(&db);
    let code = dispatch(
        &db,
        "/auth/device/code",
        None,
        &HeaderMap::new(),
        &json!({"device_name":"Owner TV"}),
        SESSION_TOKEN,
    )
    .unwrap()
    .0;
    // Expire the pairing, then deny: must be refused, not "success".
    db.execute(
        "UPDATE auth_pairings SET expires=?1 WHERE code_hash=?2",
        params![now() - 1, hash(code["user_code"].as_str().unwrap())],
    )
    .unwrap();
    let err = dispatch(
        &db,
        "/auth/device/deny",
        Some(owner()),
        &headers(),
        &json!({"user_code": code["user_code"]}),
        SESSION_TOKEN,
    )
    .unwrap_err();
    assert_eq!(err.0, StatusCode::BAD_REQUEST);
    assert_eq!(err.1, "Invalid or expired pairing code");
    // Unknown code: refused with the same contract as lookup/approve.
    let err = dispatch(
        &db,
        "/auth/device/deny",
        Some(owner()),
        &headers(),
        &json!({"user_code":"99999999"}),
        SESSION_TOKEN,
    )
    .unwrap_err();
    assert_eq!(err.0, StatusCode::BAD_REQUEST);
    assert_eq!(err.1, "Invalid or expired pairing code");
}
