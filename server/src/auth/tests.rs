use super::*;
const SESSION_TOKEN: &str = "account-session-fixture-token";
fn db() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("PRAGMA foreign_keys=ON;CREATE TABLE profiles(id INTEGER PRIMARY KEY,name TEXT NOT NULL);INSERT INTO profiles VALUES(1,'Default');CREATE TABLE favorites(profile_id INTEGER,id TEXT);INSERT INTO favorites VALUES(1,'kept');").unwrap();
    init(&db).unwrap();
    db
}
fn headers() -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(header::HOST, HeaderValue::from_static("tv.example"));
    h.insert(
        header::ORIGIN,
        HeaderValue::from_static("https://tv.example"),
    );
    h.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {SESSION_TOKEN}")).unwrap(),
    );
    h
}
fn claim(db: &Connection) -> (Value, Option<(String, String)>) {
    let result = dispatch(
        db,
        "/auth/register",
        None,
        &headers(),
        &json!({"username":"owner","password":"a-long-owner-password"}),
        SESSION_TOKEN,
    )
    .unwrap();
    db.execute("UPDATE auth_accounts SET role='owner' WHERE id=1", [])
        .unwrap();
    if !db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM profiles WHERE id=1)",
            [],
            |row| row.get::<_, bool>(0),
        )
        .unwrap()
    {
        create_profile(db, 1, &json!({"name":"Owner","avatar_style":"critters"})).unwrap();
    }
    db.execute("UPDATE profiles SET presentation_complete=1 WHERE id=1", [])
        .unwrap();
    db.execute(
        "INSERT OR IGNORE INTO profile_owners(profile_id,account_id,created_at) VALUES(1,1,0)",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT OR IGNORE INTO auth_profiles(account_id,profile_id) VALUES(1,1)",
        [],
    )
    .unwrap();
    result
}
#[test]
fn device_grants_persist_while_access_tokens_expire_and_browser_logins_rotate() {
    let db = db();
    claim(&db);
    let (grant, _) = session(&db, 1, Some(1), "device", "Living room").unwrap();
    let sid = grant["session_id"].as_str().unwrap();
    let (access, refresh): (i64, i64) = db
        .query_row(
            "SELECT access_expires,refresh_expires FROM auth_sessions WHERE id=?1",
            [sid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(access <= now() + ACCESS);
    assert_eq!(refresh, DEVICE_EXPIRY);
    for _ in 0..MAX_SESSIONS_PER_ACCOUNT + 2 {
        session(&db, 1, None, "browser", "").unwrap();
    }
    assert!(db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM auth_sessions WHERE id=?1)",
            [sid],
            |r| r.get::<_, bool>(0)
        )
        .unwrap());
}
fn owner() -> Principal {
    Principal::Account {
        account_id: 1,
        role: "owner".into(),
        profile_id: Some(1),
        session_id: None,
    }
}
#[tokio::test]
async fn http_middleware_rejects_unauthenticated_and_cookie_csrf() {
    use axum::body::Body;
    use tower::ServiceExt;
    let app = crate::test_support::app();
    let (data, cookies) = claim(&app.db.lock().unwrap());
    let access = cookies.unwrap().0;
    let router = router_with_auth(app.clone()).with_state(app.clone());
    for path in ["/auth/register", "/auth/login", "/auth/recover"] {
        let out = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(out.status(), StatusCode::FORBIDDEN, "missing Origin {path}");
    }
    for (path, method, cookie_value, csrf_value, expected) in [
        ("/auth/status", "GET", None, None, StatusCode::OK),
        ("/auth/me", "GET", None, None, StatusCode::UNAUTHORIZED),
        (
            "/auth/profile",
            "POST",
            Some(access.as_str()),
            None,
            StatusCode::FORBIDDEN,
        ),
        (
            "/auth/profile",
            "POST",
            Some(access.as_str()),
            Some("bad"),
            StatusCode::FORBIDDEN,
        ),
        (
            "/auth/profile",
            "POST",
            Some(access.as_str()),
            data["csrf_token"].as_str(),
            StatusCode::OK,
        ),
    ] {
        let mut b = Request::builder()
            .header(header::HOST, "tv.example")
            .header(header::ORIGIN, "https://tv.example")
            .uri(path)
            .method(method)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(c) = cookie_value {
            b = b.header(header::COOKIE, format!("viptv_session={c}"));
        }
        if let Some(c) = csrf_value {
            b = b.header("x-csrf-token", c);
        }
        let out = router
            .clone()
            .oneshot(b.body(Body::from("{\"profile_id\":1}")).unwrap())
            .await
            .unwrap();
        assert_eq!(out.status(), expected, "{method} {path}");
    }
    let device = session(&app.db.lock().unwrap(), 1, Some(1), "device", "TV")
        .unwrap()
        .0;
    for path in ["/accounts", "/admin/devices", "/auth/sessions"] {
        let out = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", device["access_token"].as_str().unwrap()),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(out.status(), StatusCode::FORBIDDEN, "device {path}");
    }
    for expected in [StatusCode::OK, StatusCode::UNAUTHORIZED] {
        let out = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/device/refresh")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({"refresh_token":device["refresh_token"]}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(out.status(), expected);
    }
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM auth_sessions WHERE id=?1",
                [device["session_id"].as_str().unwrap()],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    for path in [
        "/accounts",
        "/onboarding",
        "/auth/sessions",
        "/admin/devices",
    ] {
        let out = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header(header::COOKIE, format!("viptv_session={access}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(out.status(), StatusCode::OK, "owner {path}");
    }
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE auth_sessions SET access_expires=0", [])
        .unwrap();
    let out = router
        .oneshot(
            Request::builder()
                .uri("/auth/me")
                .header(header::COOKIE, format!("viptv_session={access}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(out.status(), StatusCode::UNAUTHORIZED);
}
#[test]
fn offline_owner_bootstrap_is_single_use_and_claims_only_unowned_history() {
    let mut db = db();
    let recovery = create_owner_offline(
        &mut db,
        "administrator",
        "Administrator",
        "offline-owner-password",
    )
    .unwrap();
    assert!(recovery.len() >= 32);
    assert_eq!(
        db.query_row(
            "SELECT role FROM auth_accounts WHERE username='administrator'",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "owner"
    );
    assert_eq!(
        db.query_row(
            "SELECT account_id FROM profile_owners WHERE profile_id=1",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert!(
        create_owner_offline(&mut db, "second-admin", "Second", "second-owner-password").is_err()
    );
    let member = dispatch(
        &db,
        "/auth/register",
        None,
        &headers(),
        &json!({"username":"public-member","password":"public-member-password"}),
        SESSION_TOKEN,
    )
    .unwrap()
    .0;
    assert_eq!(
        db.query_row(
            "SELECT role FROM auth_accounts WHERE id=?1",
            [identifier(&member, "account_id").unwrap()],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "member"
    );
}
#[test]
fn public_registration_is_zero_profile_and_argon2() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE profiles(id INTEGER PRIMARY KEY,name TEXT NOT NULL);CREATE TABLE favorites(profile_id INTEGER,id TEXT);").unwrap();
    init(&db).unwrap();
    let (out, cookies) = dispatch(
        &db,
        "/auth/register",
        None,
        &headers(),
        &json!({"username":"viewer","name":"Viewer","password":"a-long-viewer-password"}),
        SESSION_TOKEN,
    )
    .unwrap();
    assert!(cookies.is_some());
    assert!(out["profile_id"].is_null());
    assert!(list_profiles(&db, 1).unwrap().is_empty());
    let ph: String = db
        .query_row("SELECT password_hash FROM auth_accounts", [], |r| r.get(0))
        .unwrap();
    assert!(ph.starts_with("$argon2id$"));
}
#[test]
fn secrets_hashed_and_refresh_rotates() {
    let db = db();
    let (v, c) = claim(&db);
    let (access, refresh) = c.unwrap();
    let stored: (String, String, String) = db
        .query_row(
            "SELECT access_hash,refresh_hash,csrf_hash FROM auth_sessions",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(stored.0, hash(&access));
    assert_eq!(stored.1, hash(&refresh));
    assert_eq!(stored.2, hash(v["csrf_token"].as_str().unwrap()));
    let mut h = HeaderMap::new();
    h.insert(header::HOST, HeaderValue::from_static("tv.example"));
    h.insert(
        header::ORIGIN,
        HeaderValue::from_static("https://tv.example"),
    );
    h.insert(
        header::COOKIE,
        HeaderValue::from_str(&format!("viptv_refresh={refresh}")).unwrap(),
    );
    assert!(dispatch(&db, "/auth/refresh", None, &h, &json!({}), SESSION_TOKEN).is_err());
    h.insert(
        "x-csrf-token",
        HeaderValue::from_str(v["csrf_token"].as_str().unwrap()).unwrap(),
    );
    let rotated = dispatch(&db, "/auth/refresh", None, &h, &json!({}), SESSION_TOKEN).unwrap();
    assert_ne!(rotated.1.unwrap().0, access);
    assert!(dispatch(&db, "/auth/refresh", None, &h, &json!({}), SESSION_TOKEN).is_err());
}
#[test]
fn recovery_is_single_use_and_revokes_sessions() {
    let db = db();
    let (v, _) = claim(&db);
    let payload = json!({"username":"owner","recovery_code":v["recovery_code"],"password":"replacement-long-password"});
    let out = dispatch(
        &db,
        "/auth/recover",
        None,
        &HeaderMap::new(),
        &payload,
        SESSION_TOKEN,
    )
    .unwrap();
    assert_ne!(v["recovery_code"], out.0["recovery_code"]);
    assert!(dispatch(
        &db,
        "/auth/recover",
        None,
        &HeaderMap::new(),
        &payload,
        SESSION_TOKEN
    )
    .is_err());
    assert_eq!(
        db.query_row("SELECT count(*) FROM auth_sessions", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(dispatch(
        &db,
        "/auth/login",
        None,
        &HeaderMap::new(),
        &json!({"username":"owner","password":"a-long-owner-password"}),
        SESSION_TOKEN
    )
    .is_err());
    assert!(dispatch(
        &db,
        "/auth/login",
        None,
        &HeaderMap::new(),
        &json!({"username":"owner","password":"replacement-long-password"}),
        SESSION_TOKEN
    )
    .is_ok());
}
#[test]
fn refresh_replay_revokes_only_its_family_and_devices_are_not_admins() {
    let db = db();
    claim(&db);
    let first = session(&db, 1, Some(1), "device", "TV").unwrap().0;
    let other = session(&db, 1, Some(1), "device", "Other").unwrap().0;
    let old = json!({"refresh_token":first["refresh_token"]});
    let next = dispatch(
        &db,
        "/auth/device/refresh",
        None,
        &HeaderMap::new(),
        &old,
        SESSION_TOKEN,
    )
    .unwrap()
    .0;
    assert_eq!(first["session_id"], next["session_id"]);
    assert_eq!(
        dispatch(
            &db,
            "/auth/device/refresh",
            None,
            &HeaderMap::new(),
            &old,
            SESSION_TOKEN
        )
        .unwrap_err()
        .1,
        "Refresh token reuse detected"
    );
    assert!(dispatch(
        &db,
        "/auth/device/refresh",
        None,
        &HeaderMap::new(),
        &json!({"refresh_token":next["refresh_token"]}),
        SESSION_TOKEN
    )
    .is_err());
    assert!(dispatch(
        &db,
        "/auth/device/refresh",
        None,
        &HeaderMap::new(),
        &json!({"refresh_token":other["refresh_token"]}),
        SESSION_TOKEN
    )
    .is_ok());
    let device = Principal::Account {
        account_id: 1,
        role: "device".into(),
        profile_id: Some(1),
        session_id: None,
    };
    assert!(!device.is_owner());
    assert!(device.require_owner().is_err());
    assert!(dispatch(
        &db,
        "/auth/accounts",
        Some(device),
        &headers(),
        &json!({"username":"evil","password":"long-password-for-evil"}),
        SESSION_TOKEN
    )
    .is_err());
}
#[test]
fn configured_https_origin_is_pinned_without_forwarded_host_trust() {
    for bad in [
        "http://tv.example",
        "https://tv.example/path",
        "https://user:pass@tv.example",
        "https://tv.example?x=1",
        "https://tv.example#frag",
        "",
    ] {
        assert!(parse_origin(bad).is_err(), "{bad}");
    }
    let pin = parse_origin("https://tv.example:8443").unwrap();
    let mut h = HeaderMap::new();
    h.insert(header::HOST, HeaderValue::from_static("internal:3000"));
    h.insert(
        header::ORIGIN,
        HeaderValue::from_static("https://tv.example:8443"),
    );
    assert!(check_origin(&h, std::slice::from_ref(&pin)).is_ok());
    assert!(check_origin(&h, &[]).is_err());
    h.insert(
        "x-forwarded-host",
        HeaderValue::from_static("tv.example:8443"),
    );
    assert!(check_origin(&h, &[]).is_err());
    h.insert(
        header::ORIGIN,
        HeaderValue::from_static("https://tv.example"),
    );
    assert!(check_origin(&h, std::slice::from_ref(&pin)).is_err());
    // Additional configured origins are accepted: a reverse-proxy hostname
    // serving the same bundle must not be rejected as cross-site.
    let mirror = parse_origin("https://watch.example").unwrap();
    h.insert(
        header::ORIGIN,
        HeaderValue::from_static("https://watch.example"),
    );
    assert!(check_origin(&h, &[pin.clone(), mirror.clone()]).is_ok());
    assert!(check_origin(&h, &[mirror]).is_ok());
    assert!(check_origin(&h, &[pin]).is_err());
}
#[test]
fn origins_cookie_flags_and_public_allowlist() {
    let mut h = HeaderMap::new();
    h.insert(header::HOST, HeaderValue::from_static("tv.example"));
    h.insert(
        header::ORIGIN,
        HeaderValue::from_static("https://tv.example"),
    );
    h.insert(header::HOST, HeaderValue::from_static("tv.example"));
    h.insert(
        header::ORIGIN,
        HeaderValue::from_static("https://evil.example"),
    );
    assert!(origin(&h).is_err());
    h.insert(
        header::ORIGIN,
        HeaderValue::from_static("https://tv.example"),
    );
    assert!(origin(&h).is_ok());
    h.insert("sec-fetch-site", HeaderValue::from_static("cross-site"));
    assert!(origin(&h).is_err());
    let r = response(json!({}), Some(("access".into(), "refresh".into())));
    for c in r.headers().get_all(header::SET_COOKIE) {
        let c = c.to_str().unwrap();
        assert!(c.contains("HttpOnly; Secure; SameSite=Strict"));
        assert!(!c.contains(".."));
    }
    assert!(public("/auth/login", &Method::POST));
    assert!(!public("/auth/accounts", &Method::POST));
    assert!(!public("/auth/device/approve", &Method::POST));
    assert!(!public("/auth/login/extra", &Method::POST));
}
#[test]
fn owner_management_does_not_bypass_profile_grants() {
    let db = db();
    claim(&db);
    db.execute("DELETE FROM profile_owners WHERE account_id=1", [])
        .unwrap();
    assert!(owner().is_owner());
    assert!(!owner().can_profile(&db, 1).unwrap());
    assert!(owner().require_profile(&db, 1).is_err());
}
#[test]
fn password_snapshot_revalidation_and_last_owner_protection() {
    let db = db();
    claim(&db);
    let payload = json!({"username":"owner","password":"a-long-owner-password"});
    let prepared = prepare_auth(
        "/auth/login",
        &payload,
        login_snapshot(&db, "/auth/login", &payload).unwrap(),
    )
    .unwrap();
    assert!(prepared.login_valid);
    db.execute(
        "UPDATE auth_accounts SET password_hash='changed-concurrently' WHERE id=1",
        [],
    )
    .unwrap();
    assert!(dispatch_prepared(&db, "/auth/login", None, &headers(), &payload, &prepared).is_err());
    let unknown = prepare_auth(
        "/auth/login",
        &json!({"username":"unknown","password":"unknown-password"}),
        None,
    )
    .unwrap();
    assert!(!unknown.login_valid);
    for payload in [
        json!({"account_id":1,"role":"member"}),
        json!({"account_id":1,"disabled":true}),
    ] {
        assert!(dispatch(
            &db,
            "/auth/accounts/update",
            Some(owner()),
            &headers(),
            &payload,
            SESSION_TOKEN
        )
        .is_err());
    }
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM auth_accounts WHERE role='owner' AND disabled=0",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}
#[test]
fn rate_limits_are_database_local_and_init_leaves_no_transaction() {
    let first = db();
    let second = db();
    assert!(first.is_autocommit());
    init(&first).unwrap();
    assert!(first.is_autocommit());
    rate(&first, "login", 1).unwrap();
    assert!(rate(&first, "login", 1).is_err());
    assert!(rate(&second, "login", 1).is_ok());
    for _ in 0..100 {
        rate(&first, "auth:register:global", 100).unwrap();
    }
    assert_eq!(
        rate(&first, "auth:register:global", 100).unwrap_err().0,
        StatusCode::TOO_MANY_REQUESTS
    );
    for _ in 0..5 {
        rate(&second, "auth/register:subject-a", 5).unwrap();
    }
    assert_eq!(
        rate(&second, "auth/register:subject-a", 5).unwrap_err().0,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert!(rate(&second, "auth/register:subject-b", 5).is_ok());
    first
        .execute_batch("BEGIN; CREATE TABLE addon_init_fixture(id INTEGER); COMMIT;")
        .unwrap();
}
#[test]
fn tab_csrf_tokens_survive_refresh_and_revoke_with_family() {
    let db = db();
    let (issued, cookies) = claim(&db);
    let sid = issued["session_id"].as_str().unwrap();
    let first = issue_csrf(&db, sid).unwrap();
    let second = issue_csrf(&db, sid).unwrap();
    let mut h = headers();
    h.insert(header::HOST, HeaderValue::from_static("tv.example"));
    h.insert(
        header::ORIGIN,
        HeaderValue::from_static("https://tv.example"),
    );
    h.insert(
        header::COOKIE,
        HeaderValue::from_str(&format!("viptv_refresh={}", cookies.unwrap().1)).unwrap(),
    );
    h.insert("x-csrf-token", HeaderValue::from_str(&first).unwrap());
    let rotated = dispatch(&db, "/auth/refresh", None, &h, &json!({}), SESSION_TOKEN).unwrap();
    assert_eq!(rotated.0["session_id"], sid);
    h.insert("x-csrf-token", HeaderValue::from_str(&second).unwrap());
    let primary: String = db
        .query_row(
            "SELECT csrf_hash FROM auth_sessions WHERE id=?1",
            [sid],
            |r| r.get(0),
        )
        .unwrap();
    assert!(csrf_for_session(&db, &h, sid, &primary).is_ok());
    db.execute("DELETE FROM auth_sessions WHERE id=?1", [sid])
        .unwrap();
    assert!(csrf_for_session(&db, &h, sid, &primary).is_err());
}
#[test]
fn member_confirmation_binds_device_to_current_account_only() {
    let db = db();
    claim(&db);
    let member = dispatch(
        &db,
        "/auth/register",
        None,
        &headers(),
        &json!({"username":"pairedmember","password":"member-long-password"}),
        SESSION_TOKEN,
    )
    .unwrap()
    .0;
    let id = identifier(&member, "account_id").unwrap();
    let profile =
        create_profile(&db, id, &json!({"name":"Member","avatar_style":"thumbs"})).unwrap();
    let profile = identifier(&profile, "id").unwrap();
    assert!(owner().require_profile(&db, profile).is_err());
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
    let pairing = json!({"device_code":code["device_code"]});
    assert!(dispatch(
        &db,
        "/auth/device/token",
        None,
        &HeaderMap::new(),
        &pairing,
        SESSION_TOKEN
    )
    .is_err());
    let member_principal = Principal::Account {
        account_id: id,
        role: "member".into(),
        profile_id: None,
        session_id: None,
    };
    assert!(dispatch(
        &db,
        "/auth/device/approve",
        Some(member_principal.clone()),
        &headers(),
        &json!({"account_id":1,"user_code":code["user_code"]}),
        SESSION_TOKEN,
    )
    .is_err());
    dispatch(
        &db,
        "/auth/device/approve",
        Some(member_principal),
        &headers(),
        &json!({"user_code":code["user_code"]}),
        SESSION_TOKEN,
    )
    .unwrap();
    let issued = dispatch(
        &db,
        "/auth/device/token",
        None,
        &HeaderMap::new(),
        &json!({"device_code":code["device_code"]}),
        SESSION_TOKEN,
    )
    .unwrap()
    .0;
    assert_eq!(identifier(&issued, "account_id").unwrap(), id);
    assert!(issued["profile_id"].is_null());
    assert!(dispatch(
        &db,
        "/auth/device/token",
        None,
        &HeaderMap::new(),
        &pairing,
        SESSION_TOKEN
    )
    .is_err());
    let principal = Principal::Account {
        account_id: id,
        role: "device".into(),
        profile_id: None,
        session_id: Some(issued["session_id"].as_str().unwrap().into()),
    };
    assert!(principal.can_profile(&db, profile).unwrap());
    assert!(!principal.can_profile(&db, 1).unwrap());
}
#[tokio::test]
async fn argon_admission_times_out_but_cheap_actions_bypass_the_queue() {
    let semaphore = tokio::sync::Semaphore::new(0);
    let start = std::time::Instant::now();
    let error = acquire_auth_cpu(true, &semaphore, std::time::Duration::from_millis(10))
        .await
        .unwrap_err();
    assert_eq!(error.0, StatusCode::SERVICE_UNAVAILABLE);
    assert!(start.elapsed() < std::time::Duration::from_secs(1));
    assert!(
        acquire_auth_cpu(false, &semaphore, std::time::Duration::ZERO)
            .await
            .unwrap()
            .is_none()
    );
    semaphore.add_permits(1);
    assert!(
        acquire_auth_cpu(true, &semaphore, std::time::Duration::from_secs(1))
            .await
            .unwrap()
            .is_some()
    );
}

#[test]
fn selected_avatar_is_exact_and_survives_name_edits() {
    let db = db();
    claim(&db);
    let profile = create_profile(
        &db,
        1,
        &json!({"name":"Kid","avatar_style":"pixelbot","avatar_choice":48}),
    )
    .unwrap();
    let id = profile["id"].as_str().unwrap().parse::<i64>().unwrap();
    assert_eq!(profile["avatar_choice"], 48);
    assert!(profile["avatar_url"]
        .as_str()
        .unwrap()
        .contains("seed=viptv-pixelbot-48&"));
    let renamed = update_profile(&db, 1, id, &json!({"name":"New name"})).unwrap();
    assert_eq!(renamed["avatar_url"], profile["avatar_url"]);
    let changed = update_profile(
        &db,
        1,
        id,
        &json!({"avatar_style":"sprouts","avatar_choice":2}),
    )
    .unwrap();
    assert!(changed["avatar_url"]
        .as_str()
        .unwrap()
        .contains("seed=viptv-sprouts-2&"));
    for choice in [
        json!(0),
        json!(49),
        json!(-1),
        json!(1.5),
        json!("2"),
        Value::Null,
    ] {
        assert!(update_profile(&db, 1, id, &json!({"avatar_choice":choice})).is_err());
    }
    assert!(update_profile(&db, 999, id, &json!({"avatar_choice":1})).is_err());
    let disney = update_profile(
        &db,
        1,
        id,
        &json!({"avatar_style":"disney","avatar_choice":1}),
    )
    .unwrap();
    assert_eq!(
        disney["avatar_url"],
        character_avatars()["disney"][0]["url"]
    );
    assert!(update_profile(&db, 1, id, &json!({"avatar_choice":48})).is_err());
    let renamed = update_profile(&db, 1, id, &json!({"name":"Mickey fan"})).unwrap();
    assert_eq!(renamed["avatar_url"], disney["avatar_url"]);
}

#[test]
fn strict_profile_payloads_require_explicit_imported_setup_completion() {
    let db = db();
    claim(&db);
    db.execute("UPDATE profiles SET presentation_complete=0 WHERE id=1", [])
        .unwrap();
    for payload in [
        json!({}),
        json!({"name":"Renamed"}),
        json!({"avatar_style":"moods"}),
        json!({"name":42,"avatar_style":"moods","setup_complete":true}),
        json!({"name":"Renamed","avatar_style":42,"setup_complete":true}),
        json!({"name":"Renamed","avatar_style":"moods","setup_complete":false}),
    ] {
        assert!(update_profile(&db, 1, 1, &payload).is_err(), "{payload}");
        assert!(!db
            .query_row(
                "SELECT presentation_complete FROM profiles WHERE id=1",
                [],
                |row| row.get::<_, bool>(0)
            )
            .unwrap());
    }
    let updated = update_profile(
        &db,
        1,
        1,
        &json!({"name":"Renamed","avatar_style":"moods","setup_complete":true}),
    )
    .unwrap();
    assert_eq!(updated["setup_complete"], true);
    assert!(update_profile(&db, 1, 1, &json!({})).is_err());
    assert!(update_profile(&db, 1, 1, &json!({"avatar_style":42})).is_err());
    assert!(create_profile(&db, 1, &json!({"name":"Invalid","avatar_style":42})).is_err());
    assert!(create_profile(&db, 1, &json!({"name":"Invalid","setup_complete":true})).is_err());
}

#[test]
fn session_eviction_bounds_members_and_preserves_owner_admission() {
    let db = db();
    claim(&db);
    db.execute("DELETE FROM auth_sessions", []).unwrap();
    let tx = db.unchecked_transaction().unwrap();
    for account_id in 2..=251 {
        tx.execute(
                "INSERT INTO auth_accounts(id,username,name,password_hash,role,recovery_hash,created_at) VALUES(?1,?2,?2,'x','member',?2,0)",
                params![account_id, format!("member-{account_id}")],
            )
            .unwrap();
    }
    for index in 0..MAX_MEMBER_SESSIONS {
        let account_id = 2 + index / MAX_SESSIONS_PER_ACCOUNT;
        let session_id = format!("member-session-{index:05}");
        tx.execute(
                "INSERT INTO auth_sessions(id,account_id,access_hash,refresh_hash,csrf_hash,profile_id,kind,device_name,access_expires,refresh_expires,created_at) VALUES(?1,?2,?3,?4,?5,NULL,'browser','test',?6,?7,?8)",
                params![session_id, account_id, format!("access-{index}"), format!("refresh-{index}"), format!("csrf-{index}"), now()+ACCESS, now()+REFRESH, index],
            )
            .unwrap();
    }
    tx.commit().unwrap();

    let owner_session = session(&db, 1, None, "browser", "Owner browser").unwrap().0;
    assert!(db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM auth_sessions WHERE id=?1)",
            [owner_session["session_id"].as_str().unwrap()],
            |row| row.get::<_, bool>(0)
        )
        .unwrap());
    session(&db, 2, None, "browser", "Member browser").unwrap();
    assert!(db
            .query_row(
                "SELECT count(*)<=?1 FROM auth_sessions s JOIN auth_accounts a ON a.id=s.account_id WHERE a.role='member'",
                [MAX_MEMBER_SESSIONS],
                |row| row.get::<_, bool>(0)
            )
            .unwrap());
    assert!(db
        .query_row(
            "SELECT count(*)<=?1 FROM auth_sessions WHERE account_id=2",
            [MAX_SESSIONS_PER_ACCOUNT],
            |row| row.get::<_, bool>(0)
        )
        .unwrap());
    assert!(db
        .query_row(
            "SELECT count(*)<=?1 FROM auth_sessions",
            [MAX_SESSIONS],
            |row| row.get::<_, bool>(0)
        )
        .unwrap());
}

#[test]
fn attacker_keyed_limits_and_pairings_evict_without_fail_closed_capacity() {
    let db = db();
    claim(&db);
    let tx = db.unchecked_transaction().unwrap();
    tx.execute(
        "INSERT INTO auth_limits(bucket,count,expires) VALUES('auth:global:/auth/login',1,?1)",
        [now() + 300],
    )
    .unwrap();
    for index in 1..MAX_AUTH_BUCKETS {
        tx.execute(
            "INSERT INTO auth_limits(bucket,count,expires) VALUES(?1,1,?2)",
            params![format!("auth:subject:random-{index}"), now() + 300],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    rate(&db, "auth:global:/auth/login", 600).unwrap();
    rate(&db, "auth:subject:/auth/login:legitimate-owner", 5).unwrap();
    assert!(dispatch(
        &db,
        "/auth/login",
        None,
        &headers(),
        &json!({"username":"owner","password":"a-long-owner-password"}),
        SESSION_TOKEN,
    )
    .is_ok());
    assert_eq!(
        db.query_row("SELECT count(*) FROM auth_limits", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        MAX_AUTH_BUCKETS
    );
    assert!(db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM auth_limits WHERE bucket='auth:global:/auth/login')",
            [],
            |row| row.get::<_, bool>(0)
        )
        .unwrap());

    db.execute("DELETE FROM auth_pairings", []).unwrap();
    for index in 0..MAX_PAIRINGS {
        db.execute(
                "INSERT INTO auth_pairings(code_hash,device_hash,device_name,expires) VALUES(?1,?2,'attacker',?3)",
                params![format!("code-{index}"), format!("device-{index}"), now()+600],
            )
            .unwrap();
    }
    let pairing = dispatch(
        &db,
        "/auth/device/code",
        None,
        &HeaderMap::new(),
        &json!({"device_name":"Owner Roku"}),
        SESSION_TOKEN,
    )
    .unwrap()
    .0;
    assert!(pairing["device_code"].is_string());
    assert!(db
        .query_row(
            "SELECT count(*)<=?1 FROM auth_pairings",
            [MAX_PAIRINGS],
            |row| row.get::<_, bool>(0)
        )
        .unwrap());
    dispatch(
        &db,
        "/auth/device/approve",
        Some(owner()),
        &headers(),
        &json!({"user_code":pairing["user_code"]}),
        SESSION_TOKEN,
    )
    .unwrap();
}

#[test]
fn refresh_tombstones_are_bounded_without_blocking_rotation() {
    let db = db();
    let (issued, cookies) = claim(&db);
    let family = issued["session_id"].as_str().unwrap();
    let tx = db.unchecked_transaction().unwrap();
    for index in 0..MAX_REFRESH_TOMBSTONES {
        tx.execute(
                "INSERT INTO auth_refresh_used(hash,family,account_id,csrf_hash,kind,expires) VALUES(?1,?2,NULL,'x','device',?3)",
                params![format!("old-{index}"), format!("other-family-{index}"), now()+REFRESH],
            )
            .unwrap();
    }
    for index in 0..MAX_REFRESH_TOMBSTONES_PER_FAMILY {
        tx.execute(
                "INSERT INTO auth_refresh_used(hash,family,account_id,csrf_hash,kind,expires) VALUES(?1,?2,1,'x','browser',?3)",
                params![format!("family-old-{index}"), family, now()+REFRESH],
            )
            .unwrap();
    }
    tx.commit().unwrap();
    let mut h = headers();
    h.insert(
        header::COOKIE,
        HeaderValue::from_str(&format!("viptv_refresh={}", cookies.unwrap().1)).unwrap(),
    );
    h.insert(
        "x-csrf-token",
        HeaderValue::from_str(issued["csrf_token"].as_str().unwrap()).unwrap(),
    );
    dispatch(&db, "/auth/refresh", None, &h, &json!({}), SESSION_TOKEN).unwrap();
    assert!(db
        .query_row(
            "SELECT count(*)<=?1 FROM auth_refresh_used WHERE family=?2",
            params![MAX_REFRESH_TOMBSTONES_PER_FAMILY, family],
            |row| row.get::<_, bool>(0)
        )
        .unwrap());
    assert!(db
        .query_row(
            "SELECT count(*)<=?1 FROM auth_refresh_used",
            [MAX_REFRESH_TOMBSTONES],
            |row| row.get::<_, bool>(0)
        )
        .unwrap());
}

#[test]
fn bounded_limits_events_and_expiry() {
    let db = db();
    for _ in 0..3 {
        rate(&db, "login", 3).unwrap();
    }
    assert_eq!(
        rate(&db, "login", 3).unwrap_err().0,
        StatusCode::TOO_MANY_REQUESTS
    );
    db.execute("UPDATE auth_limits SET expires=0", []).unwrap();
    rate(&db, "login", 3).unwrap();
    for _ in 0..1010 {
        event(&db, "test", None).unwrap();
    }
    assert_eq!(
        db.query_row("SELECT count(*) FROM auth_events", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1000
    );
    let (v, c) = claim(&db);
    db.execute("UPDATE auth_sessions SET refresh_expires=0", [])
        .unwrap();
    let mut h = HeaderMap::new();
    h.insert(header::HOST, HeaderValue::from_static("tv.example"));
    h.insert(
        header::ORIGIN,
        HeaderValue::from_static("https://tv.example"),
    );
    h.insert(
        header::COOKIE,
        HeaderValue::from_str(&format!("viptv_refresh={}", c.unwrap().1)).unwrap(),
    );
    h.insert(
        "x-csrf-token",
        HeaderValue::from_str(v["csrf_token"].as_str().unwrap()).unwrap(),
    );
    assert!(dispatch(&db, "/auth/refresh", None, &h, &json!({}), SESSION_TOKEN).is_err());
}
