use crate::{auth_integration_tests::fixture, test_support::request, *};
fn setup() -> (App, Arc<Job>) {
    let app = fixture();
    let owned = app.clone().with_lease(ResourceLease {
        policy_revision: 0,
        principal: auth::Principal::Account {
            account_id: 1,
            role: "owner".into(),
            profile_id: Some(1),
            session_id: Some("s1".into()),
        },
        session_id: Some("s1".into()),
    });
    owned.own_resource("job", "wait-job");
    let job = Arc::new(Job {metadata_proof:None,
        source_slots:std::sync::atomic::AtomicUsize::new(0),
        cancelled: std::sync::atomic::AtomicBool::new(false),
        configuration_revision: None,
        media_id: Some("opaque-movie".into()),
        exact_vod: None,
        kind: "movie".into(),
        created: Instant::now(),
        state: Mutex::new(JobState {
            events: Vec::new(),
            pending: 2,
        }),
        notify: Notify::new(),
    });
    app.jobs
        .lock()
        .unwrap()
        .insert("wait-job".into(), job.clone());
    (app, job)
}
#[tokio::test]
async fn long_poll_wakes_for_partial_results_then_returns_terminal_without_waiting() {
    let (app, job) = setup();
    let poller = app.clone();
    let waiting = tokio::spawn(async move {
        request(
            &poller,
            "member-token-1",
            "GET",
            "/api/streams/wait-job?wait_ms=10000",
            Value::Null,
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!waiting.is_finished());
    {
        let mut state = job.state.lock().unwrap();
        state
            .events
            .push(json!({"seq":1,"source":"fixture","streams":[]}));
        state.pending = 1;
    }
    job.notify.notify_waiters();
    let (status, response) = tokio::time::timeout(Duration::from_secs(1), waiting)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["events"].as_array().unwrap().len(), 1);
    assert_eq!(response["done"], false);
    {
        job.state.lock().unwrap().pending = 0;
    }
    job.notify.notify_waiters();
    let (status, response) = tokio::time::timeout(
        Duration::from_secs(1),
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/streams/wait-job?after=1&wait_ms=10000",
            Value::Null,
        ),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["done"], true);
    assert!(response["events"].as_array().unwrap().is_empty());
}
#[tokio::test]
async fn long_poll_times_out_safely_and_rechecks_profile_authority_while_waiting() {
    let (app, _) = setup();
    let started = Instant::now();
    let (status, response) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/streams/wait-job?wait_ms=40",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response["done"], false);
    assert!(started.elapsed() >= Duration::from_millis(30));
    assert_eq!(
        request(
            &app,
            "member-token-2",
            "GET",
            "/api/streams/wait-job?wait_ms=10000",
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/streams/wait-job?wait_ms=10001",
            Value::Null
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let poller = app.clone();
    let waiting = tokio::spawn(async move {
        request(
            &poller,
            "member-token-1",
            "GET",
            "/api/streams/wait-job?wait_ms=10000",
            Value::Null,
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE auth_sessions SET profile_id=NULL WHERE id='s1'", [])
        .unwrap();
    let (status, _) = tokio::time::timeout(Duration::from_secs(2), waiting)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
#[tokio::test]
async fn cancellation_is_owned_idempotent_and_preserves_published_events(){
 let (app,job)=setup();job.state.lock().unwrap().events.push(json!({"seq":1,"source":"fixture","streams":[{"id":"published-grant"}]}));
 assert_eq!(request(&app,"member-token-2","DELETE","/api/streams/wait-job",Value::Null).await.0,StatusCode::FORBIDDEN);
 for _ in 0..2{let(status,receipt)=request(&app,"member-token-1","DELETE","/api/streams/wait-job",Value::Null).await;assert_eq!(status,StatusCode::OK,"{receipt}");assert_eq!(receipt["cancelled"],true);}
 assert!(job.cancelled.load(std::sync::atomic::Ordering::Acquire));assert_eq!(job.state.lock().unwrap().events[0]["streams"][0]["id"],"published-grant");
 let(status,poll)=request(&app,"member-token-1","GET","/api/streams/wait-job",Value::Null).await;assert_eq!(status,StatusCode::OK);assert_eq!(poll["done"],true);assert_eq!(poll["events"][0]["streams"][0]["id"],"published-grant");
}

#[tokio::test]
async fn concurrent_producers_share_one_whole_job_ranking_budget(){
 let (_,job)=setup();let total=std::thread::scope(|scope|{let workers=(0..64).map(|_|{let job=job.clone();scope.spawn(move||job.reserve_sources(100))}).collect::<Vec<_>>();workers.into_iter().map(|worker|worker.join().unwrap()).sum::<usize>()});assert_eq!(total,crate::app_state::MAX_SOURCES_PER_JOB);assert_eq!(job.reserve_sources(100),0);
}
