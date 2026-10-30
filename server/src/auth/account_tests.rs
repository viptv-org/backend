use super::*;
use crate::test_support::{app, configure_vault};
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHasher, SaltString},
    Argon2,
};
use axum::{
    body::{to_bytes, Body},
    http::{HeaderMap, StatusCode},
};
use tower::ServiceExt;

const PASSWORD: &str = "delete-account-fixture-password";
const PIN: &str = "4821";

fn argon_hash(value: &str) -> String {
    Argon2::default()
        .hash_password(value.as_bytes(), &SaltString::generate(&mut OsRng))
        .unwrap()
        .to_string()
}

fn account(app: &App, id: i64, username: &str, role: &str) -> String {
    let db = app.db.lock().unwrap();
    db.execute(
        "INSERT INTO auth_accounts(id,username,name,password_hash,role,recovery_hash,created_at) VALUES(?1,?2,?2,?3,?4,'unused',0)",
        params![id, username, argon_hash(PASSWORD), role],
    )
    .unwrap();
    let access = format!("access-token-{id}");
    db.execute(
        "INSERT INTO auth_sessions(id,account_id,access_hash,refresh_hash,csrf_hash,kind,device_name,access_expires,refresh_expires,created_at) VALUES(?1,?2,?3,?4,?5,'browser','fixture',?6,?6,0)",
        params![
            format!("session-{id}"),
            id,
            hash(&access),
            format!("refresh-{id}"),
            hash(&format!("csrf-{id}")),
            now() + 3600
        ],
    )
    .unwrap();
    access
}

fn seed(app: &App, account: i64) {
    let db = app.db.lock().unwrap();
    let profile = account * 10;
    let provider = account * 10;
    let addon = account * 10;
    let pairing = format!("pairing-{account}");
    db.execute(
        "INSERT INTO profiles(id,name,avatar_seed,presentation_complete) VALUES(?1,?2,'seed',1)",
        params![profile, format!("Profile {account}")],
    )
    .unwrap();
    db.execute(
        "INSERT INTO profile_owners(profile_id,account_id,created_at) VALUES(?1,?2,0)",
        params![profile, account],
    )
    .unwrap();
    db.execute("INSERT INTO auth_profiles VALUES(?1,?2)", params![account, profile]).unwrap();
    db.execute(
        "INSERT INTO auth_onboarding VALUES(?1,'{}')",
        [account],
    )
    .unwrap();
    db.execute(
        "INSERT INTO progress(profile_id,id,type,name,poster,position,duration,updated_at) VALUES(?1,'tt1','movie','Title',NULL,10.0,100.0,0)",
        [profile],
    )
    .unwrap();
    db.execute(
        "INSERT INTO favorites(profile_id,id,type,name,poster) VALUES(?1,'tt1','movie','Title',NULL)",
        [profile],
    )
    .unwrap();
    db.execute(
        "INSERT INTO playback_preferences VALUES(?1,'{}')",
        [profile],
    )
    .unwrap();
    db.execute("INSERT INTO viewing_settings VALUES(?1,1)", [profile])
        .unwrap();
    db.execute(
        "INSERT INTO queue_hidden(profile_id,type,title_id) VALUES(?1,'series','tt2')",
        [profile],
    )
    .unwrap();
    db.execute("INSERT INTO continuation_cache VALUES(?1,'tt2','tt3','{}',0)", [profile])
        .unwrap();
    db.execute(
        "INSERT INTO continuation_releases VALUES(?1,'tt2','tt3',0,'{}',0,0)",
        [profile],
    )
    .unwrap();
    db.execute("INSERT INTO engine_library_revisions VALUES(?1,1)", [profile])
        .unwrap();
    db.execute(
        "INSERT INTO engine_scoped_records VALUES(?1,'favorite','title','movie','tt1','tmdb','1','title','{}',0,0,1)",
        [profile],
    )
    .unwrap();
    db.execute(
        "INSERT INTO engine_verified_lookup_bindings VALUES(?1,'tmdb','movie','1','title','tt1','title','{}',0)",
        [profile],
    )
    .unwrap();
    db.execute(
        "INSERT INTO engine_scoped_queue_hidden VALUES(?1,'title','series','tt2')",
        [profile],
    )
    .unwrap();
    db.execute(
        "INSERT INTO engine_scoped_next VALUES(?1,'selector','{}','{}',0)",
        [profile],
    )
    .unwrap();
    db.execute("INSERT INTO engine_watch_counts VALUES(?1,'movie','tt1',1)", [profile])
        .unwrap();
    db.execute("INSERT INTO engine_profile_presentation VALUES(?1,'critters')", [profile])
        .unwrap();
    db.execute(
        "INSERT INTO engine_profile_configuration VALUES(?1,?2,1,'{}')",
        params![profile, account],
    )
    .unwrap();
    db.execute(
        "INSERT INTO engine_profile_preferences VALUES(?1,?2,1,'{}')",
        params![profile, account],
    )
    .unwrap();
    db.execute(
        "INSERT INTO engine_profile_display_preferences VALUES(?1,?2,'{}')",
        params![profile, account],
    )
    .unwrap();
    db.execute(
        "INSERT INTO engine_source_integrations(account_id,profile_id,revision,enabled,secret) VALUES(?1,?2,1,1,'sealed-source')",
        params![account, profile],
    )
    .unwrap();
    db.execute(
        "INSERT INTO engine_xtream_connections(account_id,profile_id,revision,enabled,secret) VALUES(?1,?2,1,1,'sealed-xtream')",
        params![account, profile],
    )
    .unwrap();
    db.execute(
        "INSERT INTO engine_m3u_connections(account_id,profile_id,revision,enabled,secret) VALUES(?1,?2,1,1,'sealed-m3u')",
        params![account, profile],
    )
    .unwrap();
    db.execute(
        "INSERT INTO engine_live_favorites VALUES(?1,'channel-1','connection-1',1)",
        [profile],
    )
    .unwrap();
    db.execute(
        "INSERT INTO kids_profiles VALUES(?1,1,12,1)",
        [profile],
    )
    .unwrap();
    db.execute(
        "INSERT INTO parent_controls VALUES(?1,?2,0,0)",
        params![account, argon_hash(PIN)],
    )
    .unwrap();
    db.execute(
        "INSERT INTO parent_grants VALUES(?1,?2)",
        params![format!("session-{account}"), now() + 60],
    )
    .unwrap();
    db.execute(
        "INSERT INTO kids_approvals VALUES(?1,'movie','tt1')",
        [profile],
    )
    .unwrap();
    db.execute(
        "INSERT INTO kids_origin_approvals VALUES(?1,'policy','{}')",
        [profile],
    )
    .unwrap();
    db.execute(
        "INSERT INTO kids_media VALUES(?1,'movie','tt1','tt1','{}',5,0,0)",
        [account],
    )
    .unwrap();
    db.execute("INSERT INTO kids_ambiguous VALUES(?1,'movie','tt1')", [account])
        .unwrap();
    db.execute(
        "INSERT INTO kids_origin_media VALUES(?1,'addon:1','movie','tt1','policy','parent','{}',5,0,1,0)",
        [account],
    )
    .unwrap();
    db.execute(
        "INSERT INTO kids_origin_ambiguous VALUES(?1,'addon:1','movie','tt1')",
        [account],
    )
    .unwrap();
    db.execute(
        "INSERT INTO auth_csrf_tokens VALUES(?1,'csrf-token',?2,0)",
        params![format!("session-{account}"), now() + 60],
    )
    .unwrap();
    db.execute(
        "INSERT INTO auth_device_profiles VALUES(?1,?2)",
        params![format!("session-{account}"), profile],
    )
    .unwrap();
    db.execute(
        "INSERT INTO auth_pairings VALUES(?1,?2,'Fixture TV',?3,?4,?5,0)",
        params![pairing, format!("device-{account}"), account, profile, now() + 60],
    )
    .unwrap();
    db.execute(
        "INSERT INTO auth_pairing_profiles VALUES(?1,?2)",
        params![pairing, profile],
    )
    .unwrap();
    db.execute(
        "INSERT INTO auth_refresh_used VALUES(?1,?2,?3,'csrf','browser',?4)",
        params![format!("refresh-{account}"), format!("session-{account}"), account, now() + 60],
    )
    .unwrap();
    db.execute(
        "INSERT INTO auth_events(kind,account_id,created_at) VALUES('login',?1,0)",
        [account],
    )
    .unwrap();
    db.execute(
        "INSERT INTO providers(id,name,url,username,password) VALUES(?1,?2,'http://iptv.example','user','secret')",
        params![provider, format!("IPTV {account}")],
    )
    .unwrap();
    db.execute(
        "INSERT INTO provider_ownership VALUES(?1,?2)",
        params![provider, account],
    )
    .unwrap();
    db.execute(
        "INSERT INTO provider_credentials_v2 VALUES(?1,?2,'sealed-credentials')",
        params![provider, account],
    )
    .unwrap();
    db.execute(
        "INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES(?1,?2,'1','Channel')",
        params![format!("live-{account}"), provider],
    )
    .unwrap();
    db.execute(
        "INSERT INTO provider_vod(id,provider_id,stream_id,kind,name,normalized,extension) VALUES(?1,?2,'1','movie','Title','title','mp4')",
        params![format!("vod-{account}"), provider],
    )
    .unwrap();
    db.execute(
        "INSERT INTO provider_matches VALUES(?1,'tt1','movie')",
        [format!("vod-{account}")],
    )
    .unwrap();
    db.execute(
        "INSERT INTO provider_cache VALUES(?1,'cache-key',?2,'sealed-cache')",
        params![provider, now() + 60],
    )
    .unwrap();
    db.execute(
        "INSERT INTO provider_refresh_v2(provider_id,account_id,token,state,requested_at) VALUES(?1,?2,'token','queued',0)",
        params![provider, account],
    )
    .unwrap();
    db.execute("INSERT INTO provider_live_generations VALUES(?1,1)", [provider])
        .unwrap();
    db.execute(
        "INSERT INTO provider_live_categories_v2 VALUES(?1,'1','News',1)",
        [provider],
    )
    .unwrap();
    db.execute(
        "INSERT INTO provider_match_revisions_v2 VALUES(?1,1)",
        [account],
    )
    .unwrap();
    db.execute(
        "INSERT INTO account_media_settings VALUES(?1,?2)",
        params![account, provider],
    )
    .unwrap();
    db.execute(
        "INSERT INTO addons(id,name,manifest_url,manifest,account_id) VALUES(?1,?2,?3,'{}',?4)",
        params![addon, format!("Addon {account}"), format!("https://addon{account}.example/manifest.json"), account],
    )
    .unwrap();
    db.execute(
        "INSERT INTO addon_credentials_v2 VALUES(?1,?2,'sealed-addon')",
        params![addon, account],
    )
    .unwrap();
    db.execute("INSERT INTO addon_encryption_accounts_v2 VALUES(?1)", [account])
        .unwrap();
    db.execute(
        "INSERT INTO playback_gateways VALUES(?1,?2,'Gateway','https://gateway.example','default',100,1,'sealed-gateway',1)",
        params![format!("gateway-{account}"), account],
    )
    .unwrap();
    db.execute(
        "INSERT INTO playback_gateway_grants VALUES(?1,?2)",
        params![format!("gateway-{account}"), account],
    )
    .unwrap();
    db.execute(
        "INSERT INTO simkl_links VALUES(?1,?2,1,'sealed-simkl')",
        params![profile, account],
    )
    .unwrap();
    db.execute(
        "INSERT INTO simkl_tracking_receipts VALUES(?1,'tt1',1,'sent',0)",
        [profile],
    )
    .unwrap();
    db.execute(
        "INSERT INTO simkl_oauth_states VALUES(?1,?2,?3,?4,'member',1,1,?5)",
        params![format!("state-{account}"), profile, account, format!("session-{account}"), now() + 60],
    )
    .unwrap();
    db.execute(
        "INSERT INTO simkl_import_entries VALUES(?1,'remote-1','movie','tt1','movie','{}','{}',1)",
        [profile],
    )
    .unwrap();
    db.execute(
        "INSERT INTO library_removals VALUES(?1,'movie','tt9')",
        [profile],
    )
    .unwrap();
    db.execute(
        "INSERT INTO library_history_removals VALUES(?1,'movie','tt9')",
        [profile],
    )
    .unwrap();
    db.execute(
        "INSERT INTO simkl_import_history_floors VALUES(?1,0)",
        [profile],
    )
    .unwrap();
    db.execute(
        "INSERT INTO simkl_import_progress VALUES(?1,'movie','tt1','{}',0)",
        [profile],
    )
    .unwrap();
    db.execute(
        "INSERT INTO simkl_import_snapshots VALUES(?1,?2,1,1,'{}','{}')",
        params![profile, account],
    )
    .unwrap();
    db.execute(
        "INSERT INTO stremio_import_receipts VALUES(?1,'stremio-user','movie','tt1',1,0,0)",
        [profile],
    )
    .unwrap();
}

fn tables_with_column(db: &Connection, column: &str) -> Vec<String> {
    let names = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    names
        .into_iter()
        .filter(|name| {
            db.prepare(&format!("PRAGMA table_info(\"{name}\")"))
                .and_then(|mut statement| {
                    statement
                        .query_map([], |row| row.get::<_, String>(1))?
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .map(|columns| columns.iter().any(|existing| existing == column))
                .unwrap_or(false)
        })
        .collect()
}

fn scoped_counts(db: &Connection, column: &str, value: i64) -> Vec<(String, i64)> {
    tables_with_column(db, column)
        .into_iter()
        .map(|table| {
            let count: i64 = db
                .query_row(
                    &format!("SELECT count(*) FROM \"{table}\" WHERE {column}=?1"),
                    [value],
                    |row| row.get(0),
                )
                .unwrap();
            (table, count)
        })
        .collect()
}

fn owned_counts(db: &Connection, account: i64) -> Vec<(String, i64)> {
    let mut counts = scoped_counts(db, "account_id", account);
    counts.extend(scoped_counts(db, "profile_id", account * 10));
    counts.push((
        "playback_gateways".into(),
        db.query_row(
            "SELECT count(*) FROM playback_gateways WHERE owner_account_id=?1",
            [account],
            |row| row.get(0),
        )
        .unwrap(),
    ));
    counts.push((
        "provider_live".into(),
        db.query_row(
            "SELECT count(*) FROM provider_live WHERE provider_id=?1",
            [account * 10],
            |row| row.get(0),
        )
        .unwrap(),
    ));
    counts.push((
        "provider_vod".into(),
        db.query_row(
            "SELECT count(*) FROM provider_vod WHERE provider_id=?1",
            [account * 10],
            |row| row.get(0),
        )
        .unwrap(),
    ));
    counts.push((
        "provider_matches".into(),
        db.query_row(
            "SELECT count(*) FROM provider_matches WHERE vod_id=?1",
            [format!("vod-{account}")],
            |row| row.get(0),
        )
        .unwrap(),
    ));
    counts.push((
        "engine_addon_incarnations".into(),
        db.query_row(
            "SELECT count(*) FROM engine_addon_incarnations WHERE addon_id=?1",
            [account * 10],
            |row| row.get(0),
        )
        .unwrap(),
    ));
    counts.push((
        "auth_pairing_profiles".into(),
        db.query_row(
            "SELECT count(*) FROM auth_pairing_profiles WHERE code_hash=?1",
            [format!("pairing-{account}")],
            |row| row.get(0),
        )
        .unwrap(),
    ));
    counts
}

async fn send(
    app: &App,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: Value,
) -> (StatusCode, HeaderMap, String) {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    for (name, value) in headers {
        builder = builder.header(*name, value);
    }
    let response = router(app.clone())
        .oneshot(
            builder
                .body(if body.is_null() {
                    Body::empty()
                } else {
                    Body::from(body.to_string())
                })
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

async fn delete(
    app: &App,
    token: &str,
    body: Value,
) -> (StatusCode, HeaderMap, String) {
    send(
        app,
        "DELETE",
        "/api/auth/account",
        &[("authorization", format!("Bearer {token}"))],
        body,
    )
    .await
}

fn code(body: &str) -> String {
    serde_json::from_str::<Value>(body).unwrap()["error_code"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test]
async fn rejected_deletions_change_nothing_and_never_echo_a_secret() {
    let mut a = app();
    configure_vault(&mut a);
    let token = account(&a, 1, "member1", "member");
    seed(&a, 1);
    let before = {
        let db = a.db.lock().unwrap();
        owned_counts(&db, 1)
    };
    assert!(
        before.iter().filter(|(_, count)| *count > 0).count() >= 30,
        "the fixture must reach every owned table: {before:?}"
    );
    for (label, body, expected) in [
        (
            "wrong username",
            json!({"username":"member2","password":PASSWORD,"pin":PIN}),
            "account_delete_rejected",
        ),
        (
            "wrong password",
            json!({"username":"member1","password":"wrong-password","pin":PIN}),
            "account_delete_rejected",
        ),
        (
            "missing password",
            json!({"username":"member1","pin":PIN}),
            "account_delete_rejected",
        ),
        (
            "missing pin",
            json!({"username":"member1","password":PASSWORD}),
            "parent_pin_incorrect",
        ),
        (
            "wrong pin",
            json!({"username":"member1","password":PASSWORD,"pin":"1111"}),
            "parent_pin_incorrect",
        ),
    ] {
        let (status, headers, text) = delete(&a, &token, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{label}");
        assert_eq!(code(&text), expected, "{label}: {text}");
        assert_eq!(
            headers["cache-control"].to_str().unwrap(),
            "no-store",
            "{label}"
        );
        assert!(!text.contains(PASSWORD), "{label} echoed the password");
        assert!(!text.contains(PIN), "{label} echoed the pin");
        assert!(!text.contains("member1"), "{label} echoed the username");
        let db = a.db.lock().unwrap();
        assert_eq!(owned_counts(&db, 1), before, "{label} changed a row");
    }
}

#[tokio::test]
async fn deleting_an_account_removes_every_owned_row_and_keeps_other_accounts() {
    let mut a = app();
    configure_vault(&mut a);
    let token = account(&a, 1, "member1", "member");
    let other = account(&a, 2, "member2", "member");
    seed(&a, 1);
    seed(&a, 2);
    let untouched = {
        let db = a.db.lock().unwrap();
        owned_counts(&db, 2)
    };
    assert!(untouched.iter().filter(|(_, count)| *count > 0).count() >= 30);
    let (status, headers, text) = delete(
        &a,
        &token,
        json!({"username":"member1","password":PASSWORD,"pin":PIN}),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{text}");
    assert_eq!(headers["cache-control"].to_str().unwrap(), "no-store");
    let cleared: Vec<String> = headers
        .get_all("set-cookie")
        .iter()
        .map(|value| value.to_str().unwrap().to_owned())
        .collect();
    assert_eq!(cleared.len(), 2, "{cleared:?}");
    for (name, cookie) in ["viptv_session", "viptv_refresh"].iter().zip(&cleared) {
        assert!(cookie.starts_with(&format!("{name}=;")), "{cookie}");
        assert!(cookie.contains("Max-Age=0"), "{cookie}");
        assert!(cookie.contains("HttpOnly; Secure; SameSite=Strict"), "{cookie}");
    }
    let db = a.db.lock().unwrap();
    assert_eq!(
        owned_counts(&db, 1)
            .into_iter()
            .filter(|(_, count)| *count > 0)
            .collect::<Vec<_>>(),
        Vec::new()
    );
    assert_eq!(owned_counts(&db, 2), untouched);
    assert_eq!(
        db.query_row("SELECT count(*) FROM profiles WHERE id=10", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM auth_accounts WHERE id=1", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM auth_events WHERE kind='account_deleted'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    drop(db);
    let (status, _, _) = delete(
        &a,
        &other,
        json!({"username":"member2","password":PASSWORD,"pin":PIN}),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn device_sessions_and_unverified_cookie_writes_cannot_delete_an_account() {
    let mut a = app();
    configure_vault(&mut a);
    let token = account(&a, 1, "member1", "member");
    seed(&a, 1);
    let device = "device-access-token";
    a.db.lock()
        .unwrap()
        .execute(
            "INSERT INTO auth_sessions(id,account_id,access_hash,refresh_hash,csrf_hash,kind,device_name,access_expires,refresh_expires,created_at) VALUES('device-session',1,?1,'device-refresh','device-csrf','device','Living room TV',?2,?2,0)",
            params![hash(device), now() + 3600],
        )
        .unwrap();
    let body = json!({"username":"member1","password":PASSWORD,"pin":PIN});
    let (status, _, text) = delete(&a, device, body.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(code(&text), "account_session_required");
    let cookie = format!("viptv_session={token}");
    let (status, _, _) = send(
        &a,
        "DELETE",
        "/api/auth/account",
        &[
            ("host", "tv.example".into()),
            ("origin", "https://tv.example".into()),
            ("cookie", cookie.clone()),
        ],
        body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        a.db.lock()
            .unwrap()
            .query_row("SELECT count(*) FROM auth_accounts WHERE id=1", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    let (status, _, text) = send(
        &a,
        "DELETE",
        "/api/auth/account",
        &[
            ("host", "tv.example".into()),
            ("origin", "https://tv.example".into()),
            ("cookie", cookie),
            ("x-csrf-token", "csrf-1".into()),
        ],
        body,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{text}");
}

#[tokio::test]
async fn a_sole_owner_deletes_its_account_and_the_household() {
    let mut a = app();
    configure_vault(&mut a);
    let owner = account(&a, 1, "owner", "owner");
    seed(&a, 1);
    let (status, headers, text) = delete(
        &a,
        &owner,
        json!({"username":"owner","password":PASSWORD,"pin":PIN}),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{text}");
    assert_eq!(headers["cache-control"].to_str().unwrap(), "no-store");
    let db = a.db.lock().unwrap();
    assert_eq!(
        owned_counts(&db, 1)
            .into_iter()
            .filter(|(_, count)| *count > 0)
            .collect::<Vec<_>>(),
        Vec::new()
    );
    for table in ["auth_accounts", "profiles", "profile_owners"] {
        assert_eq!(
            db.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0,
            "{table} kept a row"
        );
    }
}

#[tokio::test]
async fn an_owner_is_refused_while_other_accounts_still_need_one() {
    let mut a = app();
    configure_vault(&mut a);
    let owner = account(&a, 1, "owner", "owner");
    let member = account(&a, 2, "member2", "member");
    seed(&a, 1);
    seed(&a, 2);
    let before = {
        let db = a.db.lock().unwrap();
        owned_counts(&db, 1)
    };
    let (status, _, text) = delete(
        &a,
        &owner,
        json!({"username":"owner","password":PASSWORD,"pin":PIN}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{text}");
    assert_eq!(code(&text), "last_owner");
    assert!(text.contains("Have them delete their accounts first"), "{text}");
    {
        let db = a.db.lock().unwrap();
        assert_eq!(owned_counts(&db, 1), before);
        assert_eq!(
            db.query_row("SELECT count(*) FROM auth_accounts WHERE id=1", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    let (status, _, text) = delete(
        &a,
        &member,
        json!({"username":"member2","password":PASSWORD,"pin":PIN}),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{text}");
    let (status, _, text) = delete(
        &a,
        &owner,
        json!({"username":"owner","password":PASSWORD,"pin":PIN}),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{text}");
    assert_eq!(
        a.db.lock()
            .unwrap()
            .query_row("SELECT count(*) FROM auth_accounts", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn account_deletion_uses_the_shared_auth_rate_limit_bucket() {
    let mut a = app();
    configure_vault(&mut a);
    let token = account(&a, 1, "member1", "member");
    seed(&a, 1);
    let bucket = format!("auth:subject:/auth/account:{}", hash("account-1"));
    a.db.lock()
        .unwrap()
        .execute(
            "INSERT INTO auth_limits(bucket,count,expires) VALUES(?1,31,?2)",
            params![bucket, now() + 120],
        )
        .unwrap();
    let (status, headers, text) = delete(
        &a,
        &token,
        json!({"username":"member1","password":PASSWORD,"pin":PIN}),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{text}");
    assert_eq!(code(&text), "rate_limited");
    assert!(headers["retry-after"].to_str().unwrap().parse::<i64>().unwrap() > 0);
    assert_eq!(
        a.db.lock()
            .unwrap()
            .query_row("SELECT count(*) FROM auth_accounts WHERE id=1", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}
