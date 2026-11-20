use super::*;
use std::collections::HashSet;

#[derive(Clone, Default)]
struct Outputs {
    live: Arc<Mutex<HashSet<String>>>,
    events: Arc<Mutex<Vec<String>>>,
    created: Arc<AtomicUsize>,
    delete_delay: Arc<AtomicUsize>,
    teardown_delay: Arc<AtomicUsize>,
}
impl Outputs {
    fn events(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }
    fn count(&self, prefix: &str) -> usize {
        self.events().iter().filter(|event| event.starts_with(prefix)).count()
    }
}

async fn capacity_gateway(capacity: usize) -> (App, Outputs, tokio::task::JoinHandle<()>) {
    let mut app = fixture();
    app.secret_vault = Some(Arc::new(
        crate::secret_store::Vault::from_json(
            &json!({"active":"test","keys":{"test":STANDARD.encode([7u8;32])}}).to_string(),
        )
        .unwrap(),
    ));
    app.providers.vault = app.secret_vault.clone();
    app.addons.vault = app.secret_vault.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    app.gateway_client = client::Client::fixture(
        format!("http://{}/", listener.local_addr().unwrap())
            .parse()
            .unwrap(),
    );
    let outputs = Outputs::default();
    let create = outputs.clone();
    let release = outputs.clone();
    let routes = Router::new()
        .route("/v1/capabilities", axum::routing::get(|| async {
            Json(json!({"version":1,"ready":true,"torrent":true,"required_audio_index":true,"protocols":["hls"],"namespaces":["first"],"scopes":["capabilities","create","read","renew","release"],"available":{"inputs":1,"outputs":1,"viewers":5}}))
        }))
        .route("/v1/sessions", axum::routing::post(move |Json(_): Json<Value>| {
            let outputs = create.clone();
            async move {
                let mut live = outputs.live.lock().unwrap();
                if live.len() >= capacity {
                    outputs.events.lock().unwrap().push("refused".into());
                    return (StatusCode::TOO_MANY_REQUESTS, Json(json!({"error":{"code":"output_capacity"}})));
                }
                let id = format!("viewer_{}", outputs.created.fetch_add(1, Ordering::SeqCst) + 1);
                live.insert(id.clone());
                outputs.events.lock().unwrap().push(format!("create:{id}"));
                (StatusCode::CREATED, Json(remote(&id)))
            }
        }))
        .route("/v1/sessions/:id", axum::routing::get(|Path(id): Path<String>| async move { Json(remote(&id)) })
            .delete(move |Path(id): Path<String>| {
                let outputs = release.clone();
                async move {
                    outputs.events.lock().unwrap().push(format!("deleting:{id}"));
                    let delay = outputs.delete_delay.load(Ordering::SeqCst) as u64;
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                    outputs.events.lock().unwrap().push(format!("delete:{id}"));
                    let teardown = outputs.teardown_delay.load(Ordering::SeqCst) as u64;
                    let live = outputs.live.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(teardown)).await;
                        live.lock().unwrap().remove(&id);
                    });
                    StatusCode::NO_CONTENT
                }
            }))
        .route("/v1/sessions/:id/renew", axum::routing::post(|Path(id): Path<String>| async move { Json(remote(&id)) }));
    let task = tokio::spawn(async move {
        axum::serve(listener, routes).await.unwrap();
    });
    gateway(&app, "first", 0);
    (app, outputs, task)
}

fn second_profile(app: &App) -> (&'static str, String) {
    let token = "member-token-3";
    {
        let db = app.db.lock().unwrap();
        db.execute("INSERT INTO profiles(id,name,avatar_seed,presentation_complete) VALUES(2,'Second','fixture-two',1)", []).unwrap();
        db.execute("INSERT INTO profile_owners(profile_id,account_id,created_at) VALUES(2,1,0)", []).unwrap();
        db.execute("INSERT INTO auth_profiles VALUES(1,2)", []).unwrap();
        let hash = format!("{:x}", Sha256::digest(token.as_bytes()));
        db.execute("INSERT INTO auth_sessions(id,account_id,profile_id,access_hash,refresh_hash,csrf_hash,kind,device_name,access_expires,refresh_expires,created_at) VALUES('s3',1,2,?1,'refresh3','unused','browser','test',?2,?2,0)", rusqlite::params![hash, crate::util::now() + 3600]).unwrap();
    }
    let lease = ResourceLease {
        policy_revision: 0,
        principal: auth::Principal::Account {
            account_id: 1,
            role: "owner".into(),
            profile_id: Some(2),
            session_id: Some("s3".into()),
        },
        session_id: Some("s3".into()),
    };
    let (sources, _) = app.clone().with_lease(lease).register(
        "addon:1",
        vec![json!({"url":"http://source.fixture.invalid/movie.mp4","name":"Fixture"})],
        "movie",
    );
    (token, sources[0]["id"].as_str().unwrap().into())
}

async fn admit(app: &App, token: &str, stream: &str, request_id: &str, position: f64) -> String {
    let mut start = body(stream, request_id, "roku");
    start["position"] = json!(position);
    let (status, admitted) = request(app, token, "POST", "/api/playback", start).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{admitted}");
    admitted["id"].as_str().unwrap().into()
}

async fn stop(app: &App, token: &str, id: &str) {
    let (status, value) = request(app, token, "DELETE", &format!("/api/playback/{id}"), Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{value}");
}

#[tokio::test]
async fn a_seek_reopen_waits_for_its_own_release_before_creating_the_next_output() {
    let (app, outputs, _task) = capacity_gateway(1).await;
    let stream = source(&app);
    let first = admit(&app, "member-token-1", &stream, "seek-115", 115.0).await;
    assert_eq!(settled(&app, &first).await["status"], "ready");
    outputs.delete_delay.store(400, Ordering::SeqCst);
    let releasing = {
        let app = app.clone();
        let first = first.clone();
        tokio::spawn(async move { stop(&app, "member-token-1", &first).await })
    };
    tokio::time::timeout(Duration::from_secs(3), async {
        while outputs.count("deleting:") == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let next = admit(&app, "member-token-1", &stream, "seek-125", 125.0).await;
    let ready = settled(&app, &next).await;
    releasing.await.unwrap();
    assert_eq!(ready["status"], "ready", "{ready}");
    assert_eq!(
        outputs.events(),
        ["create:viewer_1", "deleting:viewer_1", "delete:viewer_1", "create:viewer_2"]
    );
}

#[tokio::test]
async fn a_seek_reopen_retries_while_the_gateway_still_tears_down_the_released_output() {
    let (app, outputs, _task) = capacity_gateway(1).await;
    let stream = source(&app);
    let first = admit(&app, "member-token-1", &stream, "seek-115", 115.0).await;
    assert_eq!(settled(&app, &first).await["status"], "ready");
    outputs.teardown_delay.store(600, Ordering::SeqCst);
    stop(&app, "member-token-1", &first).await;
    let next = admit(&app, "member-token-1", &stream, "seek-125", 125.0).await;
    let ready = settled(&app, &next).await;
    assert_eq!(ready["status"], "ready", "{ready}");
    assert!(outputs.count("refused") >= 1, "{:?}", outputs.events());
    assert_eq!(outputs.count("create:"), 2);
}

#[tokio::test]
async fn another_viewer_at_capacity_still_gets_gateway_capacity_and_never_frees_the_playing_output() {
    let (app, outputs, _task) = capacity_gateway(1).await;
    let stream = source(&app);
    let (second, second_stream) = second_profile(&app);
    let earlier = admit(&app, second, &second_stream, "second-115", 115.0).await;
    assert_eq!(settled_for(&app, second, &earlier).await["status"], "ready");
    stop(&app, second, &earlier).await;
    let playing = admit(&app, "member-token-1", &stream, "first-0", 0.0).await;
    assert_eq!(settled(&app, &playing).await["status"], "ready");
    let reopened = admit(&app, second, &second_stream, "second-125", 125.0).await;
    let refused = settled_for(&app, second, &reopened).await;
    assert_eq!(refused["error_code"], "gateway_capacity", "{refused}");
    assert!(outputs.count("refused") >= 1, "{:?}", outputs.events());
    let (_, playing_state) = request(&app, "member-token-1", "GET", &format!("/api/playback/{playing}"), Value::Null).await;
    assert_eq!(playing_state["status"], "ready", "{playing_state}");
    assert_eq!(outputs.events().iter().filter(|event| event.starts_with("delet")).collect::<Vec<_>>(), ["deleting:viewer_1", "delete:viewer_1"]);
}

#[tokio::test]
async fn a_viewer_without_a_released_playback_is_refused_on_the_first_capacity_answer() {
    let (app, outputs, _task) = capacity_gateway(1).await;
    let stream = source(&app);
    let (second, second_stream) = second_profile(&app);
    let playing = admit(&app, "member-token-1", &stream, "first-0", 0.0).await;
    assert_eq!(settled(&app, &playing).await["status"], "ready");
    let started = std::time::Instant::now();
    let other = admit(&app, second, &second_stream, "second-0", 0.0).await;
    let refused = settled_for(&app, second, &other).await;
    assert_eq!(refused["error_code"], "gateway_capacity", "{refused}");
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(outputs.count("refused"), 1);
    assert_eq!(outputs.count("delet"), 0);
}
