//! RFC 8628 section 3.5 errors on the device token poll (backend#44): the
//! polling device must tell "declined" and "expired" apart from "waiting".
use super::*;

fn new_code(db: &Connection) -> Value {
    dispatch(
        db,
        "/auth/device/code",
        None,
        &HeaderMap::new(),
        &json!({"device_name":"Living room TV"}),
        SESSION_TOKEN,
    )
    .unwrap()
    .0
}

fn poll(db: &Connection, code: &Value) -> Result<Value, ApiError> {
    dispatch(
        db,
        "/auth/device/token",
        None,
        &HeaderMap::new(),
        &json!({"device_code":code["device_code"]}),
        SESSION_TOKEN,
    )
    .map(|(output, _)| output)
}

fn deny(db: &Connection, code: &Value) {
    dispatch(
        db,
        "/auth/device/deny",
        Some(owner()),
        &headers(),
        &json!({"user_code":code["user_code"]}),
        SESSION_TOKEN,
    )
    .unwrap();
}

fn expire(db: &Connection, code: &Value, at: i64) {
    db.execute(
        "UPDATE auth_pairings SET expires=?1 WHERE code_hash=?2",
        params![at, hash(code["user_code"].as_str().unwrap())],
    )
    .unwrap();
}

fn assert_poll_error(db: &Connection, code: &Value, expected: &str) {
    let err = poll(db, code).unwrap_err();
    assert_eq!(err.0, StatusCode::BAD_REQUEST);
    assert_eq!(err.1, expected);
}

#[test]
fn pending_code_keeps_answering_authorization_pending() {
    let db = db();
    claim(&db);
    let code = new_code(&db);
    assert_poll_error(&db, &code, "authorization_pending");
    assert_poll_error(&db, &code, "authorization_pending");
}

#[test]
fn denied_code_answers_access_denied_on_every_poll_until_cleanup() {
    let db = db();
    claim(&db);
    let code = new_code(&db);
    assert_poll_error(&db, &code, "authorization_pending");
    deny(&db, &code);
    assert_poll_error(&db, &code, "access_denied");
    // A lost response must not turn the denial back into "waiting".
    assert_poll_error(&db, &code, "access_denied");
    // Still a denial after the code's original expiry, within the window.
    expire(&db, &code, now() - 1);
    assert_poll_error(&db, &code, "access_denied");
}

#[test]
fn denied_code_cannot_be_looked_up_approved_or_denied_again() {
    let db = db();
    claim(&db);
    let code = new_code(&db);
    deny(&db, &code);
    for path in [
        "/auth/device/lookup",
        "/auth/device/approve",
        "/auth/device/deny",
    ] {
        let err = dispatch(
            &db,
            path,
            Some(owner()),
            &headers(),
            &json!({"user_code":code["user_code"]}),
            SESSION_TOKEN,
        )
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST, "{path}");
        assert_eq!(err.1, "Invalid or expired pairing code", "{path}");
    }
    assert_poll_error(&db, &code, "access_denied");
}

#[test]
fn denying_an_approved_code_stops_the_grant() {
    let db = db();
    claim(&db);
    let code = new_code(&db);
    dispatch(
        &db,
        "/auth/device/approve",
        Some(owner()),
        &headers(),
        &json!({"user_code":code["user_code"]}),
        SESSION_TOKEN,
    )
    .unwrap();
    deny(&db, &code);
    assert_poll_error(&db, &code, "access_denied");
}

#[test]
fn expired_code_answers_expired_token_even_after_another_device_sweeps() {
    let db = db();
    claim(&db);
    let code = new_code(&db);
    expire(&db, &code, now() - 1);
    assert_poll_error(&db, &code, "expired_token");
    // Another TV asking for a code sweeps the table; the expired row must
    // survive long enough for this TV's next poll.
    new_code(&db);
    assert_poll_error(&db, &code, "expired_token");
}

#[test]
fn expired_approved_code_answers_expired_token_not_a_session() {
    let db = db();
    claim(&db);
    let code = new_code(&db);
    dispatch(
        &db,
        "/auth/device/approve",
        Some(owner()),
        &headers(),
        &json!({"user_code":code["user_code"]}),
        SESSION_TOKEN,
    )
    .unwrap();
    expire(&db, &code, now() - 1);
    assert_poll_error(&db, &code, "expired_token");
}

#[test]
fn tombstones_are_cleaned_up_after_the_window() {
    let db = db();
    claim(&db);
    let denied = new_code(&db);
    deny(&db, &denied);
    let expired = new_code(&db);
    expire(&db, &denied, now() - PAIRING_TOMBSTONE_SECS);
    expire(&db, &expired, now() - PAIRING_TOMBSTONE_SECS);
    new_code(&db);
    let remaining: i64 = db
        .query_row(
            "SELECT count(*) FROM auth_pairings WHERE code_hash IN(?1,?2)",
            params![
                hash(denied["user_code"].as_str().unwrap()),
                hash(expired["user_code"].as_str().unwrap())
            ],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 0);
    // Gone entirely: an unknown device_code never claims a denial.
    assert_poll_error(&db, &denied, "authorization_pending");
    assert_poll_error(&db, &expired, "authorization_pending");
}

#[test]
fn unknown_device_code_never_answers_access_denied() {
    let db = db();
    claim(&db);
    let err = dispatch(
        &db,
        "/auth/device/token",
        None,
        &HeaderMap::new(),
        &json!({"device_code":"not-a-real-device-code"}),
        SESSION_TOKEN,
    )
    .unwrap_err();
    assert_eq!(err.1, "authorization_pending");
}

#[test]
fn tombstones_do_not_count_against_live_pairing_capacity() {
    let db = db();
    claim(&db);
    db.execute("DELETE FROM auth_pairings", []).unwrap();
    for index in 0..MAX_PAIRINGS {
        db.execute(
            "INSERT INTO auth_pairings(code_hash,device_hash,device_name,expires,denied) VALUES(?1,?2,'dead',?3,1)",
            params![format!("dead-{index}"), format!("dead-device-{index}"), now() + 600],
        )
        .unwrap();
    }
    let live = new_code(&db);
    assert_poll_error(&db, &live, "authorization_pending");
    let total: i64 = db
        .query_row("SELECT count(*) FROM auth_pairings", [], |row| row.get(0))
        .unwrap();
    assert!(total <= MAX_PAIRINGS);
}

#[test]
fn approve_still_issues_a_device_session() {
    let db = db();
    claim(&db);
    let code = new_code(&db);
    assert_poll_error(&db, &code, "authorization_pending");
    dispatch(
        &db,
        "/auth/device/approve",
        Some(owner()),
        &headers(),
        &json!({"user_code":code["user_code"]}),
        SESSION_TOKEN,
    )
    .unwrap();
    let grant = poll(&db, &code).unwrap();
    assert_eq!(identifier(&grant, "account_id").unwrap(), 1);
    // Consumed: the row is gone, so a replay reads as an unknown code.
    assert_poll_error(&db, &code, "authorization_pending");
}

#[tokio::test]
async fn token_errors_carry_a_matching_error_code() {
    for code in ["access_denied", "expired_token", "authorization_pending"] {
        let response = auth_error(ApiError(StatusCode::BAD_REQUEST, code.into()));
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"], code);
        assert_eq!(body["error_code"], code);
    }
}
