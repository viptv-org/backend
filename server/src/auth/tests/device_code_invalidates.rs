//! Issuing a new pairing code invalidates the same device's earlier pending
//! code (backend#42): the superseded code reads as expired on both approve and
//! the token poll, while other devices' codes stay untouched.
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

fn new_code_for_device(db: &Connection, device_code: &str) -> Value {
    dispatch(
        db,
        "/auth/device/code",
        None,
        &HeaderMap::new(),
        &json!({"device_name":"Living room TV","device_code":device_code}),
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

fn approve(db: &Connection, code: &Value) -> Result<Value, ApiError> {
    dispatch(
        db,
        "/auth/device/approve",
        Some(owner()),
        &headers(),
        &json!({"user_code":code["user_code"]}),
        SESSION_TOKEN,
    )
    .map(|(output, _)| output)
}

fn assert_pending(db: &Connection, code: &Value) {
    let err = poll(db, code).unwrap_err();
    assert_eq!(err.0, StatusCode::BAD_REQUEST);
    assert_eq!(err.1, "authorization_pending");
}

fn assert_expired(db: &Connection, code: &Value) {
    let err = poll(db, code).unwrap_err();
    assert_eq!(err.0, StatusCode::BAD_REQUEST);
    assert_eq!(err.1, "expired_token");
}

fn assert_invalid_code(db: &Connection, code: &Value, path: &str) {
    let err = dispatch(
        db,
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

#[test]
fn a_second_code_for_the_same_device_expires_the_first() {
    let db = db();
    claim(&db);
    let first = new_code(&db);
    assert_pending(&db, &first);
    let second = new_code_for_device(&db, first["device_code"].as_str().unwrap());
    // The superseded code is unusable: approve and the token poll both answer
    // as expired, and lookup refuses it with the existing wording.
    assert_expired(&db, &first);
    assert_invalid_code(&db, &first, "/auth/device/approve");
    assert_invalid_code(&db, &first, "/auth/device/deny");
    assert_invalid_code(&db, &first, "/auth/device/lookup");
    // The new code is still live and redeemable end to end.
    assert_pending(&db, &second);
    approve(&db, &second).unwrap();
    let grant = poll(&db, &second).unwrap();
    assert_eq!(identifier(&grant, "account_id").unwrap(), 1);
}

#[test]
fn issuing_a_code_for_one_device_leaves_another_devices_code_alone() {
    let db = db();
    claim(&db);
    let other = new_code(&db);
    let first = new_code(&db);
    let _second = new_code_for_device(&db, first["device_code"].as_str().unwrap());
    // The unrelated device's pending code is still pending and approvable.
    assert_pending(&db, &other);
    approve(&db, &other).unwrap();
    let grant = poll(&db, &other).unwrap();
    assert_eq!(identifier(&grant, "account_id").unwrap(), 1);
}
