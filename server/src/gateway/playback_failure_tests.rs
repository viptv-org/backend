use super::*;
use axum::{body::Body, http::Request};
use tower::ServiceExt;
use tracing::instrument::WithSubscriber;

const SECOND_SESSION: &str = "member-token-1-second";

async fn report(app: &App, token: &str, body: String) -> (StatusCode, String) {
    let (subscriber, logs) = crate::test_support::capture_logs(tracing::Level::INFO);
    let response = crate::router(app.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/playback-failures")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .with_subscriber(subscriber)
        .await
        .unwrap();
    let logs = logs.text();
    let line = logs
        .lines()
        .filter(|line| line.contains("Playback failure reported"))
        .collect::<Vec<_>>()
        .join("\n");
    (response.status(), line)
}

fn second_profile_session(app: &App) {
    let hash = format!("{:x}", Sha256::digest(SECOND_SESSION.as_bytes()));
    app.db.lock().unwrap().execute(
        "INSERT INTO auth_sessions(id,account_id,access_hash,refresh_hash,csrf_hash,kind,device_name,access_expires,refresh_expires,created_at,profile_id) VALUES('s1-second',1,?1,'refresh1-second','unused','browser','test',?2,?2,0,1)",
        rusqlite::params![hash, crate::util::now() + 3600],
    ).unwrap();
}

#[tokio::test]
async fn every_cause_is_logged_once_with_its_bounded_fields() {
    let app = fixture();
    for cause in ["renderer", "undecodable", "stall", "start_timeout", "open_failed"] {
        let (status, line) = report(
            &app,
            "member-token-1",
            json!({"cause":cause,"operation":"RemoteFailure","status":503,"position_ms":61000,"first_frame":true}).to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{cause}");
        assert_eq!(line.lines().count(), 1, "{line}");
        assert!(line.contains("WARN"), "{line}");
        for fact in [
            format!("cause=\"{cause}\""),
            "operation=\"RemoteFailure\"".into(),
            "status=503".into(),
            "position_ms=61000".into(),
            "first_frame=true".into(),
            format!("profile_tag={}", crate::gateway::diagnostics::tag("1")),
            format!("account_tag={}", crate::gateway::diagnostics::tag("1")),
        ] {
            assert!(line.contains(&fact), "missing {fact} in {line}");
        }
    }
    let (status, line) = report(&app, "member-token-1", json!({"cause":"stall","first_frame":false}).to_string()).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(line.contains("first_frame=false") && line.contains("session_tag= "), "{line}");
}

#[tokio::test]
async fn invalid_reports_are_rejected_without_a_log_line() {
    let app = fixture();
    let long = "a".repeat(65);
    let oversized = "a".repeat(1100);
    for body in [
        json!({"cause":"stall","first_frame":true,"extra":1}).to_string(),
        json!({"cause":"stall","first_frame":true,"message":"https://user:pass@host/Movie.mkv"}).to_string(),
        json!({"cause":"crash","first_frame":true}).to_string(),
        json!({"cause":"stall"}).to_string(),
        json!({"first_frame":true}).to_string(),
        json!({"cause":"stall","first_frame":"yes"}).to_string(),
        json!({"cause":"open_failed","first_frame":false,"operation":long}).to_string(),
        json!({"cause":"open_failed","first_frame":false,"operation":"https://host/x"}).to_string(),
        json!({"cause":"open_failed","first_frame":false,"operation":""}).to_string(),
        json!({"cause":"open_failed","first_frame":false,"status":42}).to_string(),
        json!({"cause":"open_failed","first_frame":false,"status":-1}).to_string(),
        json!({"cause":"stall","first_frame":true,"position_ms":-5}).to_string(),
        json!({"cause":"stall","first_frame":true,"playback_id":"a".repeat(129)}).to_string(),
        json!({"cause":"stall","first_frame":true,"playback_id":"../x"}).to_string(),
        json!({"cause":"stall","first_frame":true,"playback_id":oversized}).to_string(),
        r#"{"cause":"stall","cause":"renderer","first_frame":true}"#.into(),
        "[]".into(),
        String::new(),
    ] {
        let (status, line) = report(&app, "member-token-1", body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(line.is_empty(), "{body} logged {line}");
    }
}

#[tokio::test]
async fn reports_need_a_profile_session() {
    let app = fixture();
    let body = json!({"cause":"stall","first_frame":true}).to_string();
    let (status, line) = report(&app, "member-token-2", body.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(line.is_empty());
    let (status, _) = report(&app, "not-a-token", body).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn only_the_callers_own_playback_is_tagged() {
    let (app, _peer) = setup().await;
    gateway(&app, "first", 1);
    second_profile_session(&app);
    let source = source(&app);
    let (status, start) = request(&app, "member-token-1", "POST", "/api/playback", body(&source, "failure_tag", "android")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{start}");
    let id = start["id"].as_str().unwrap().to_owned();
    settled(&app, &id).await;
    let tagged = format!("session_tag={}", crate::gateway::diagnostics::tag(&id));
    let failure = json!({"cause":"renderer","first_frame":true,"playback_id":id}).to_string();

    let (status, own) = report(&app, "member-token-1", failure.clone()).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(own.contains(&tagged), "{own}");
    assert!(!own.contains(&id), "{own}");

    let (status, foreign) = report(&app, SECOND_SESSION, failure).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(foreign.contains("Playback failure reported"), "{foreign}");
    assert!(!foreign.contains(&tagged) && !foreign.contains(&id), "{foreign}");
    assert!(foreign.contains("session_tag= "), "{foreign}");
}

#[tokio::test]
async fn reports_beyond_the_session_rate_are_dropped_silently() {
    let app = fixture();
    second_profile_session(&app);
    let body = json!({"cause":"stall","first_frame":true}).to_string();
    let mut logged = 0;
    for _ in 0..35 {
        let (status, line) = report(&app, "member-token-1", body.clone()).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        logged += usize::from(!line.is_empty());
    }
    assert_eq!(logged, 30);
    let (status, line) = report(&app, SECOND_SESSION, body).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!line.is_empty(), "another session keeps its own allowance");
}
