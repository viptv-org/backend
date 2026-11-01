use super::{scrub, MAX_REPORT_BYTES as MAX_BYTES};
use crate::{auth_integration_tests::fixture, test_support::request, *};

const OWNER: &str = "member-token-1";
const OTHER: &str = "member-token-2";

fn report(model: &str) -> Value {
    json!({
        "appVersion": "0.1.9+abc1234",
        "buildCommit": "abc1234",
        "deviceModel": model,
        "androidVersion": "14",
        "thread": "main",
        "stack": "java.lang.RuntimeException: boom\n\tat org.viptv.Main.run(Main.kt:10)",
        "lastScreen": "Home",
        "nativeDuringPlaybackStart": false
    })
}

async fn upload(app: &App, token: &str, body: Value) -> (StatusCode, Value) {
    request(app, token, "POST", "/api/crash-reports", body).await
}

async fn ids(app: &App, token: &str) -> Vec<String> {
    let (status, data) = request(app, token, "GET", "/api/crash-reports", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{data}");
    data["reports"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["id"].as_str().unwrap().to_owned())
        .collect()
}

fn stored_count(app: &App) -> i64 {
    app.db
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM crash_reports", [], |row| row.get(0))
        .unwrap()
}

fn padded_report(total: usize) -> String {
    let prefix = r#"{"stack":""#;
    let suffix = r#""}"#;
    let filler = "a ".repeat((total - prefix.len() - suffix.len()) / 2);
    let mut body = format!("{prefix}{filler}{suffix}");
    while body.len() < total {
        body.insert(prefix.len(), ' ');
    }
    assert_eq!(body.len(), total);
    body
}

async fn raw_upload(app: &App, token: &str, body: String) -> StatusCode {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    router(app.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/crash-reports")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn upload_persists_and_list_returns_newest_first_with_summary_only() {
    let app = fixture();
    let (status, first) = upload(&app, OWNER, report("Pixel 7")).await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let (status, second) = upload(&app, OWNER, report("Shield TV")).await;
    assert_eq!(status, StatusCode::CREATED, "{second}");
    let (status, data) = request(&app, OWNER, "GET", "/api/crash-reports", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{data}");
    let reports = data["reports"].as_array().unwrap();
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0]["id"], second["id"]);
    assert_eq!(reports[1]["id"], first["id"]);
    assert_eq!(reports[0]["summary"]["device_model"], "Shield TV");
    assert_eq!(reports[0]["summary"]["app_version"], "0.1.9+abc1234");
    assert_eq!(reports[0]["summary"]["build_commit"], "abc1234");
    assert!(reports[0]["received_at"].as_i64().unwrap() > 0);
    assert!(!data.to_string().contains("RuntimeException"));
    let id = first["id"].as_str().unwrap();
    let (status, one) = request(
        &app,
        OWNER,
        "GET",
        &format!("/api/crash-reports/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{one}");
    assert_eq!(one["report"]["deviceModel"], "Pixel 7");
    assert_eq!(one["report"]["stack"], report("Pixel 7")["stack"]);
}

#[tokio::test]
async fn only_the_last_twenty_reports_are_kept_per_account() {
    let app = fixture();
    let (status, _) = upload(&app, OTHER, report("Other phone")).await;
    assert_eq!(status, StatusCode::CREATED);
    let mut sent = Vec::new();
    for index in 0..22 {
        let (status, data) = upload(&app, OWNER, report(&format!("Device {index}"))).await;
        assert_eq!(status, StatusCode::CREATED, "{data}");
        sent.push(data["id"].as_str().unwrap().to_owned());
    }
    let kept = ids(&app, OWNER).await;
    assert_eq!(kept.len(), 20);
    let expected: Vec<_> = sent[2..].iter().rev().cloned().collect();
    assert_eq!(kept, expected);
    assert_eq!(ids(&app, OTHER).await.len(), 1);
    assert_eq!(stored_count(&app), 21);
}

#[tokio::test]
async fn bodies_up_to_64_kb_are_accepted_and_larger_ones_get_413() {
    let app = fixture();
    assert_eq!(
        raw_upload(&app, OWNER, padded_report(MAX_BYTES)).await,
        StatusCode::CREATED
    );
    assert_eq!(
        raw_upload(&app, OWNER, padded_report(MAX_BYTES + 1)).await,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(
        raw_upload(&app, OWNER, padded_report(MAX_BYTES * 40)).await,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(stored_count(&app), 1);
}

#[tokio::test]
async fn malformed_and_non_object_bodies_are_rejected_without_storing() {
    let app = fixture();
    for body in ["{", "", "[1,2]", "\"text\"", "null", "42", "{\"a\":1}{\"b\":2}"] {
        let status = raw_upload(&app, OWNER, body.to_owned()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }
    let deep = format!("{}{}", "[".repeat(5000), "]".repeat(5000));
    assert_eq!(
        raw_upload(&app, OWNER, format!("{{\"a\":{deep}}}")).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(stored_count(&app), 0);
    let (status, _) = upload(&app, OWNER, report("Pixel")).await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn urls_tokens_and_credentials_are_scrubbed_before_storage() {
    let app = fixture();
    let secret_token = "ABCDEF0123456789ABCDEF0123456789ABCDEF";
    let body = json!({
        "appVersion": "0.1.9",
        "deviceModel": "https://user:hunter2@host.example:8443/path?token=SECRET#t=1",
        "stack": format!("failed GET http://tv.example/live/u/p/1.ts?auth=SECRETQUERY\nBearer abc.def.ghi\napi_key=SECRETKEY&x=1 raw {secret_token}"),
        "playback": {"providerLabel": "Torrentio", "hdr": ["hdr10", "https://cdn.example/a?sig=SECRETSIG"]},
        "https://leak.example/key?token=KEYSECRET": 1,
    });
    let (status, created) = upload(&app, OWNER, body).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap();
    let (_, one) = request(
        &app,
        OWNER,
        "GET",
        &format!("/api/crash-reports/{id}"),
        Value::Null,
    )
    .await;
    let (_, list) = request(&app, OWNER, "GET", "/api/crash-reports", Value::Null).await;
    let raw: String = app
        .db
        .lock()
        .unwrap()
        .query_row("SELECT body FROM crash_reports", [], |row| row.get(0))
        .unwrap();
    for text in [one.to_string(), list.to_string(), raw] {
        for leaked in [
            "hunter2",
            "SECRET",
            "token=",
            "abc.def.ghi",
            "/live/u/p",
            secret_token,
            "KEYSECRET",
            "/key?",
        ] {
            assert!(!text.contains(leaked), "{leaked} leaked in {text}");
        }
    }
    assert_eq!(
        one["report"]["deviceModel"],
        "https://host.example:8443"
    );
    assert_eq!(one["report"]["playback"]["providerLabel"], "Torrentio");
    assert_eq!(one["report"]["playback"]["hdr"][0], "hdr10");
    assert_eq!(one["report"]["playback"]["hdr"][1], "https://cdn.example");
    assert_eq!(
        list["reports"][0]["summary"]["device_model"],
        "https://host.example:8443"
    );
}

#[test]
fn scrub_matches_client_rules_and_leaves_plain_text() {
    assert_eq!(
        scrub::text_of("https://user:pass@host:8080/path?token=SECRET#t=10"),
        "https://host:8080"
    );
    assert_eq!(scrub::text_of("Torrentio"), "Torrentio");
    assert_eq!(
        scrub::text_of("token ABCDEF0123456789ABCDEF0123456789 tail"),
        "token <redacted> tail"
    );
    assert_eq!(scrub::text_of("ok abc123"), "ok abc123");
    assert_eq!(scrub::text_of("at app.Main.run(Main.kt:10)"), "at app.Main.run(Main.kt:10)");
    assert_eq!(
        scrub::text_of("see (https://a.example/x?y=1) and http://b.example"),
        "see (https://a.example) and http://b.example"
    );
    assert_eq!(scrub::text_of("monkey=banana"), "monkey=banana");
    assert_eq!(scrub::text_of("x_token=abc&y=1"), "x_token=<redacted>&y=1");
    assert_eq!(scrub::text_of("Authorization: Bearer abc123"), "Authorization: Bearer <redacted>");
    assert_eq!(scrub::text_of("://nohost"), "://nohost");
    assert_eq!(scrub::text_of("é https://é.example/ü?q=ü ü"), "é https://é.example ü");
}

#[tokio::test]
async fn accounts_cannot_see_read_or_delete_each_others_reports() {
    let app = fixture();
    let (_, mine) = upload(&app, OWNER, report("Owner phone")).await;
    let (_, theirs) = upload(&app, OTHER, report("Member phone")).await;
    let mine = mine["id"].as_str().unwrap().to_owned();
    let theirs = theirs["id"].as_str().unwrap().to_owned();
    assert_eq!(ids(&app, OWNER).await, vec![mine.clone()]);
    assert_eq!(ids(&app, OTHER).await, vec![theirs.clone()]);
    for (method, token, id) in [
        ("GET", OTHER, &mine),
        ("DELETE", OTHER, &mine),
        ("GET", OWNER, &theirs),
        ("DELETE", OWNER, &theirs),
    ] {
        let (status, error) = request(
            &app,
            token,
            method,
            &format!("/api/crash-reports/{id}"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {id}: {error}");
        assert_eq!(error["error_code"], "crash_report_not_found");
    }
    assert_eq!(ids(&app, OWNER).await, vec![mine]);
    assert_eq!(ids(&app, OTHER).await, vec![theirs]);
}

#[tokio::test]
async fn owner_can_delete_a_report() {
    let app = fixture();
    let (_, created) = upload(&app, OWNER, report("Pixel")).await;
    let path = format!("/api/crash-reports/{}", created["id"].as_str().unwrap());
    let (status, data) = request(&app, OWNER, "DELETE", &path, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{data}");
    assert!(ids(&app, OWNER).await.is_empty());
    let (status, _) = request(&app, OWNER, "DELETE", &path, Value::Null).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn unauthenticated_callers_get_401() {
    let app = fixture();
    for (method, path) in [
        ("POST", "/api/crash-reports"),
        ("GET", "/api/crash-reports"),
        ("GET", "/api/crash-reports/x"),
        ("DELETE", "/api/crash-reports/x"),
    ] {
        let (status, _) = request(&app, "wrong-token", method, path, report("Pixel")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
    }
    assert_eq!(stored_count(&app), 0);
}

#[tokio::test]
async fn restricted_profiles_and_paired_devices_cannot_read_and_only_devices_upload() {
    let app = fixture();
    let (_, created) = upload(&app, OWNER, report("Pixel")).await;
    let id = created["id"].as_str().unwrap().to_owned();
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO auth_sessions(id,account_id,access_hash,refresh_hash,csrf_hash,kind,origin,device_name,access_expires,refresh_expires,created_at) VALUES('tv',1,?1,'refresh-tv','unused','device','tv','Fixture TV',?2,?2,1)",
            params![auth::hash("tv-token"), auth::now() + 3600],
        )
        .unwrap();
    let (status, data) = upload(&app, "tv-token", report("Living room TV")).await;
    assert_eq!(status, StatusCode::CREATED, "{data}");
    let path = format!("/api/crash-reports/{id}");
    for (method, path) in [
        ("GET", "/api/crash-reports".to_owned()),
        ("GET", path.clone()),
        ("DELETE", path.clone()),
    ] {
        let (status, error) = request(&app, "tv-token", method, &path, Value::Null).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}");
        assert_eq!(error["error_code"], "account_session_required");
    }
    app.db
        .lock()
        .unwrap()
        .execute("INSERT INTO kids_profiles(profile_id,enabled) VALUES(1,1)", [])
        .unwrap();
    for (method, path) in [
        ("GET", "/api/crash-reports".to_owned()),
        ("GET", path.clone()),
        ("DELETE", path.clone()),
    ] {
        let (status, error) = request(&app, OWNER, method, &path, Value::Null).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}");
        assert_eq!(error["error_code"], "parent_required");
    }
    let (status, error) = upload(&app, OWNER, report("Kid tablet")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error["error_code"], "parent_required");
    assert_eq!(stored_count(&app), 2);
}

#[tokio::test]
async fn deleting_an_account_removes_its_reports() {
    let app = fixture();
    upload(&app, OTHER, report("Pixel")).await;
    upload(&app, OWNER, report("Pixel")).await;
    app.db
        .lock()
        .unwrap()
        .execute("DELETE FROM auth_accounts WHERE id=2", [])
        .unwrap();
    assert_eq!(stored_count(&app), 1);
}
