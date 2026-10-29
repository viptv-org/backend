use super::*;
use axum::response::IntoResponse;

const DAY: u64 = 86_400_000;

fn now() -> u64 {
    util::now_millis() as u64
}
fn iso(millis: u64) -> String {
    chrono::DateTime::from_timestamp_millis(millis as i64)
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
fn day(millis: u64) -> String {
    iso(millis)[..10].to_owned()
}

struct Served {
    calls: Arc<AtomicUsize>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}
impl Drop for Served {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

async fn addon(app: &mut App, episodes: Vec<(u32, u32, u64)>) -> Served {
    app.addons.allow_test_loopback = true;
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let routes = Router::new().route(
        "/meta/series/:file",
        get(move |Path(file): Path<String>| {
            let id = file.trim_end_matches(".json").to_owned();
            let missing = id.starts_with("tt9");
            let videos = episodes
                .iter()
                .map(|(season, number, at)| json!({"id":format!("{id}:{season}:{number}"),"title":format!("Chapter {number}"),"season":season,"episode":number,"released":iso(*at)}))
                .collect::<Vec<_>>();
            counted.fetch_add(1, Ordering::SeqCst);
            async move {
                if missing {
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
                axum::Json(json!({"meta":{"id":id,"type":"series","name":"Fixture Show","poster":"https://img.invalid/poster.jpg","videos":videos}})).into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    app.db.lock().unwrap().execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id) VALUES(1,'Metadata',?1,?2,1)",params![format!("http://{address}/manifest.json"),json!({"id":"metadata","name":"Metadata","version":"1","resources":["meta"],"types":["series"]}).to_string()]).unwrap();
    crate::test_support::encrypt_fixture_sources(app);
    Served {
        calls,
        tasks: vec![task],
    }
}

fn save_series(app: &App, id: &str) {
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO favorites(profile_id,type,id,name,poster) VALUES(1,'series',?1,'Fixture Show',NULL)",
            [id],
        )
        .unwrap();
}

async fn calendar(app: &App, query: &str) -> (StatusCode, Value) {
    request(
        app,
        "member-token-1",
        "GET",
        &format!("/api/calendar{query}"),
        json!({}),
    )
    .await
}
async fn navigation(app: &App) -> Vec<String> {
    let (status, body) = request(app, "member-token-1", "GET", "/api/bootstrap", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["navigation"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_owned())
        .collect()
}
fn coordinates(page: &Value) -> Vec<(u64, u64)> {
    page["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["season"].as_u64().unwrap(), e["episode"].as_u64().unwrap()))
        .collect()
}

#[tokio::test]
async fn a_library_only_profile_gets_its_series_episodes_without_simkl() {
    let mut app = fixture();
    let t = now();
    let _served = addon(
        &mut app,
        vec![(1, 1, t - 10 * DAY), (1, 2, t - DAY), (1, 3, t + 2 * DAY), (1, 4, t + 20 * DAY)],
    )
    .await;
    assert_eq!(calendar(&app, "").await.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!navigation(&app).await.contains(&"calendar".to_owned()));

    save_series(&app, "tt100");
    assert!(navigation(&app).await.contains(&"calendar".to_owned()));
    let (_, controls) = request(&app, "member-token-1", "GET", "/api/discovery-controls", json!({})).await;
    assert_eq!(controls["calendar"]["availability"], "Available");
    let (status, page) = calendar(&app, "").await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["sources"], json!(["library"]));
    assert_eq!(coordinates(&page), vec![(1, 2), (1, 3)]);
    let entry = &page["entries"][0];
    assert_eq!(entry["source"], "library");
    assert_eq!(entry["card"]["identity"]["id"], "tt100");
    assert_eq!(entry["card"]["title"], "Fixture Show");
    assert_eq!(entry["card"]["poster"], "https://img.invalid/poster.jpg");
    assert_eq!(entry["episode_title"], "Chapter 2");
    assert_eq!(entry["date"], iso(t - DAY));
    assert_eq!(entry["episode_card"]["identity"]["id"], "tt100:1:2");
    assert_eq!(entry["episode_card"]["identity"]["episode"]["season"], 1);
    assert_eq!(entry["episode_card"]["parent_identity"]["id"], "tt100");
    assert!(page.get("refresh_after_millis").is_none());

    let (status, movies) = calendar(&app, "?type=movie").await;
    assert_eq!(status, StatusCode::OK, "{movies}");
    assert!(movies["entries"].as_array().unwrap().is_empty());

    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO kids_profiles(profile_id,enabled,max_age,revision) VALUES(1,1,12,1)",
            [],
        )
        .unwrap();
    assert!(!navigation(&app).await.contains(&"calendar".to_owned()));
    assert_eq!(calendar(&app, "").await.0, StatusCode::UNPROCESSABLE_ENTITY);
    let (_, controls) = request(&app, "member-token-1", "GET", "/api/discovery-controls", json!({})).await;
    assert_eq!(controls["calendar"]["availability"], "Unsupported");
}

#[tokio::test]
async fn a_series_without_metadata_leaves_the_others_and_no_refresh_hint() {
    let mut app = fixture();
    let t = now();
    let _served = addon(&mut app, vec![(1, 1, t)]).await;
    save_series(&app, "tt100");
    save_series(&app, "tt900");
    let (status, page) = calendar(&app, "").await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(coordinates(&page), vec![(1, 1)]);
    assert_eq!(page["entries"][0]["card"]["identity"]["id"], "tt100");
    assert!(page.get("refresh_after_millis").is_none());
}

#[tokio::test]
async fn library_calendar_windows_pages_and_caches_per_profile() {
    let mut app = fixture();
    let t = now();
    let served = addon(
        &mut app,
        vec![(1, 1, t - 10 * DAY), (1, 2, t - DAY), (1, 3, t + 2 * DAY), (1, 4, t + 20 * DAY)],
    )
    .await;
    save_series(&app, "tt100");
    let window = format!("?start={}&end={}", day(t - 10 * DAY), day(t + 20 * DAY));
    let (status, page) = calendar(&app, &window).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(coordinates(&page), vec![(1, 1), (1, 2), (1, 3), (1, 4)]);
    assert_eq!(page["complete"], true);
    let (status, second) = calendar(&app, &format!("{window}&skip=1&limit=1")).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(coordinates(&second), vec![(1, 2)]);
    assert_eq!(second["next_cursor"], "2");
    assert_eq!(second["complete"], false);
    assert_eq!(second["entries"][0]["id"], page["entries"][1]["id"]);

    let calls = served.calls.load(Ordering::SeqCst);
    assert_eq!(calls, 1);
    app.addons.forget_responses();
    let (_, again) = calendar(&app, &window).await;
    assert_eq!(again, page);
    assert_eq!(served.calls.load(Ordering::SeqCst), calls);

    save_series(&app, "tt200");
    app.addons.forget_responses();
    let ((a, first), (b, other)) = tokio::join!(calendar(&app, &window), calendar(&app, &window));
    assert_eq!((a, b), (StatusCode::OK, StatusCode::OK));
    assert_eq!(first, other);
    assert_eq!(coordinates(&first).len(), 8);
    assert_eq!(served.calls.load(Ordering::SeqCst), calls + 2);
}

#[tokio::test]
async fn a_simkl_profile_merges_library_and_public_entries_once() {
    let t = now();
    let shared = t + DAY;
    let calendar_doc = json!({"calendar":[
        {"simkl_id":20,"date":iso(shared + 3_600_000),"episode":{"season":2,"episode":3,"title":"Public title"}},
        {"simkl_id":30,"date":iso(t + 2 * DAY),"episode":{"season":1,"episode":1,"title":"Other show"}}
    ],"metadata":{
        "20":{"title":"Shared series","ids":{"simkl_id":20,"imdb":"tt1234567"}},
        "30":{"title":"Public only","ids":{"simkl_id":30}}
    }});
    let anime_doc = json!({"calendar":[
        {"simkl_id":40,"date":iso(shared),"episode":{"season":2,"episode":3}}
    ],"metadata":{"40":{"title":"Shared anime","ids":{"simkl_id":40,"imdb":"tt1234567"}}}});
    let router = Router::new()
        .route(
            "/calendar/v2/tv.json",
            get(move || {
                let doc = calendar_doc.clone();
                async move { axum::Json(doc) }
            }),
        )
        .route(
            "/calendar/v2/anime.json",
            get(move || {
                let doc = anime_doc.clone();
                async move { axum::Json(doc) }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let mut app = fixture();
    app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
    let mut served = addon(&mut app, vec![(2, 3, shared), (2, 4, t + 3 * DAY)]).await;
    served.tasks.push(task);

    assert!(navigation(&app).await.contains(&"calendar".to_owned()));
    let (status, public) = calendar(&app, "").await;
    assert_eq!(status, StatusCode::OK, "{public}");
    assert_eq!(public["sources"], json!(["public"]));
    assert_eq!(public["entries"].as_array().unwrap().len(), 2);
    assert!(public["entries"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["source"] == "public" && e.get("episode_card").is_none()));

    save_series(&app, "tt1234567");
    let (status, page) = calendar(&app, "").await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["sources"], json!(["library", "public"]));
    let entries = page["entries"].as_array().unwrap();
    let summary = entries
        .iter()
        .map(|e| {
            (
                e["source"].as_str().unwrap().to_owned(),
                e["card"]["identity"]["id"].as_str().unwrap().to_owned(),
                e["episode"].as_u64().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        summary,
        vec![
            ("library".into(), "tt1234567".into(), 3),
            ("public".into(), "simkl:tv:30".into(), 1),
            ("library".into(), "tt1234567".into(), 4),
        ]
    );
    let (status, anime) = calendar(&app, "?type=anime").await;
    assert_eq!(status, StatusCode::OK, "{anime}");
    assert!(anime["entries"].as_array().unwrap().is_empty(), "{anime}");
    assert_eq!(
        entries[1]["id"],
        public["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["card"]["identity"]["id"] == "simkl:tv:30")
            .unwrap()["id"]
    );
}
