use crate::{auth_integration_tests::fixture, test_support::request, *};
use axum::response::IntoResponse;
use axum::routing::get;
use std::sync::atomic::{AtomicUsize, Ordering};

pub(crate) struct MetadataAddon {
    pub(crate) calls: Arc<AtomicUsize>,
    pub(crate) peak: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for MetadataAddon {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub(crate) fn full_meta(kind: &str, id: &str) -> Value {
    json!({"meta":{"id":id,"type":kind,"name":format!("Upstream {id}"),
        "description":format!("Full description of {id}"),"releaseInfo":"2019",
        "genres":["Drama","Mystery"],"imdbRating":"8.4","runtime":"45 min",
        "poster":format!("https://images.example/{id}/poster.jpg"),
        "background":format!("https://images.example/{id}/background.jpg"),
        "logo":format!("https://images.example/{id}/logo.png")}})
}
pub(crate) async fn metadata_addon(app: &mut App) -> MetadataAddon {
    use axum::extract::Path;
    struct Active(Arc<AtomicUsize>);
    impl Drop for Active {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let (counted, highest) = (calls.clone(), peak.clone());
    let routes = Router::new().route(
        "/meta/:kind/:id",
        get(move |Path((kind, id)): Path<(String, String)>| {
            let (calls, peak, active) = (counted.clone(), highest.clone(), active.clone());
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                peak.fetch_max(active.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
                let _active = Active(active);
                tokio::time::sleep(Duration::from_millis(30)).await;
                let id = id.trim_end_matches(".json").to_owned();
                if id.starts_with("tt") {
                    axum::Json(full_meta(&kind, &id)).into_response()
                } else {
                    StatusCode::NOT_FOUND.into_response()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    app.addons.allow_test_loopback = true;
    let manifest = json!({"id":"fixture.meta","name":"Fixture metadata","resources":["meta"],"types":["movie","series"]});
    app.db.lock().unwrap().execute(
        "INSERT INTO addons(id,name,manifest_url,manifest,account_id)VALUES(501,'Fixture metadata',?1,?2,1)",
        params![format!("http://{address}/manifest.json"), manifest.to_string()],
    ).unwrap();
    crate::test_support::encrypt_fixture_sources(app);
    app.library_hydration.enabled.store(true, Ordering::SeqCst);
    MetadataAddon { calls, peak, task }
}

fn start(app: &App) -> bool {
    let principal = auth::Principal::Account {
        account_id: 1,
        role: "member".into(),
        profile_id: Some(1),
        session_id: Some("s1".into()),
    };
    let lease = ResourceLease {
        policy_revision: 0,
        principal,
        session_id: Some("s1".into()),
    };
    super::hydration::schedule(app, &lease, 1)
}

async fn get_ok(app: &App, path: &str) -> Value {
    let (status, value) = request(app, "member-token-1", "GET", path, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value
}
fn context(app: &App, id: &str) -> Value {
    let raw: String = app
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT context FROM favorites WHERE profile_id=1 AND id=?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    serde_json::from_str(&raw).unwrap()
}
fn card<'a>(page: &'a Value, id: &str) -> &'a Value {
    page["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["identity"]["id"] == id)
        .unwrap_or_else(|| panic!("{id} missing from {page}"))
}

#[tokio::test]
async fn thin_rows_gain_the_details_card_in_the_background_without_overwriting_row_data() {
    let mut app = fixture();
    let addon = metadata_addon(&mut app).await;
    let partial = json!({"identity":{"id":"tt0000003","kind":"series","ids":[],"edition":null,"episode":null},
        "provider_id":"addon:9","title":"Old","subtitle":null,"description":null,"poster":null,
        "landscape":"https://user.example/landscape.jpg","logo":null,"progress":null,"badges":[],"parent_identity":null});
    app.db.lock().unwrap().execute_batch(&format!(
        "INSERT INTO favorites(profile_id,id,type,name,poster) VALUES
            (1,'tt0000001','series','Imported name','https://images.metahub.space/poster/small/tt0000001/img'),
            (1,'tt0000002','movie','Imported movie',NULL),
            (1,'kitsu:42','series','Opaque anime',NULL);
         INSERT INTO favorites(profile_id,id,type,name,poster,context) VALUES
            (1,'tt0000003','series','My name',NULL,'{partial}');"
    )).unwrap();
    let before = get_ok(&app, "/api/profiles/1/revisions").await["library"].clone();
    assert!(start(&app));
    let first = get_ok(&app, "/api/profiles/1/library").await;
    assert_eq!(
        first["refresh_after_millis"], 4000,
        "clients are told to look again"
    );
    super::hydration::settled(&app, 1).await;

    app.verified_cards.lock().unwrap().clear();
    let page = get_ok(&app, "/api/profiles/1/library").await;
    assert!(page["refresh_after_millis"].is_null(), "{page}");
    let show = card(&page, "tt0000001");
    assert_eq!(show["title"], "Imported name", "the row's name is kept");
    assert_eq!(
        show["poster"],
        "https://images.metahub.space/poster/small/tt0000001/img"
    );
    assert_eq!(show["description"], "Full description of tt0000001");
    assert_eq!(
        show["landscape"],
        "https://images.example/tt0000001/background.jpg"
    );
    assert_eq!(show["logo"], "https://images.example/tt0000001/logo.png");
    assert_eq!(show["metadata"]["year"], 2019);
    assert_eq!(show["metadata"]["genres"], json!(["Drama", "Mystery"]));
    assert_eq!(show["metadata"]["rating"], 8.4);
    assert_eq!(show["metadata"]["runtime_minutes"], 45);
    let stored = context(&app, "tt0000001");
    assert_eq!(stored["description"], "Full description of tt0000001");
    assert!(stored.get("metadata_origin").is_none());
    assert!(stored["progress"].is_null());

    let movie = card(&page, "tt0000002");
    assert_eq!(
        movie["poster"],
        "https://images.example/tt0000002/poster.jpg"
    );
    assert_eq!(movie["metadata"]["year"], 2019);

    let kept = card(&page, "tt0000003");
    assert_eq!(kept["title"], "My name");
    assert_eq!(kept["provider_id"], "addon:9");
    assert_eq!(kept["landscape"], "https://user.example/landscape.jpg");
    assert_eq!(kept["description"], "Full description of tt0000003");

    assert_eq!(context(&app, "kitsu:42"), json!({}));
    assert!(card(&page, "kitsu:42")["description"].is_null());

    let after = get_ok(&app, "/api/profiles/1/revisions").await["library"].clone();
    assert_ne!(
        before, after,
        "the library revision advances so clients refetch"
    );
    assert!(
        addon.peak.load(Ordering::SeqCst) <= 4,
        "lookups are bounded"
    );

    let calls = addon.calls.load(Ordering::SeqCst);
    start(&app);
    super::hydration::settled(&app, 1).await;
    assert_eq!(addon.calls.load(Ordering::SeqCst), calls);
}

#[tokio::test]
async fn a_full_saved_card_is_never_looked_up_or_replaced() {
    let mut app = fixture();
    let addon = metadata_addon(&mut app).await;
    let saved = json!({"card":{"identity":{"id":"tt0000005","kind":"movie","ids":[],"edition":null,"episode":null},
        "provider_id":"stremio","title":"Saved","subtitle":null,"description":"User-visible saved description",
        "poster":"https://images.example/saved.jpg","landscape":null,"logo":null,"progress":null,"badges":[],"parent_identity":null}});
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/profiles/1/library",
            saved
        )
        .await
        .0,
        StatusCode::OK
    );
    start(&app);
    super::hydration::settled(&app, 1).await;
    assert_eq!(
        addon.calls.load(Ordering::SeqCst),
        0,
        "a saved card is not thin"
    );
    assert_eq!(
        context(&app, "tt0000005")["description"],
        "User-visible saved description"
    );
}

#[tokio::test]
async fn restricted_profiles_never_store_unapproved_metadata() {
    let mut app = fixture();
    let addon = metadata_addon(&mut app).await;
    app.db.lock().unwrap().execute_batch(
        "INSERT INTO favorites(profile_id,id,type,name,poster) VALUES(1,'tt0000001','series','Imported',NULL);
         INSERT INTO kids_profiles(profile_id,enabled,max_age,revision) VALUES(1,1,7,1);",
    ).unwrap();
    start(&app);
    super::hydration::settled(&app, 1).await;
    assert_eq!(addon.calls.load(Ordering::SeqCst), 0);
    assert_eq!(context(&app, "tt0000001"), json!({}));
}

#[tokio::test]
async fn a_request_never_waits_for_metadata_lookups() {
    let mut app = fixture();
    let _addon = metadata_addon(&mut app).await;
    {
        let db = app.db.lock().unwrap();
        for n in 1..=40 {
            db.execute(
                "INSERT INTO favorites(profile_id,id,type,name) VALUES(1,?1,'series','Imported')",
                [format!("tt{n:07}")],
            )
            .unwrap();
        }
    }
    let started = std::time::Instant::now();
    assert!(start(&app));
    let page = get_ok(&app, "/api/profiles/1/library").await;
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(page["refresh_after_millis"], 4000);
    assert!(super::hydration::running(&app, 1));
    super::hydration::settled(&app, 1).await;
    let filled: i64 = app.db.lock().unwrap().query_row(
        "SELECT COUNT(*) FROM favorites WHERE profile_id=1 AND json_extract(context,'$.description') IS NOT NULL",
        [], |r| r.get(0)).unwrap();
    assert_eq!(filled, 40);
}

#[tokio::test]
async fn listing_my_list_never_starts_metadata_lookups() {
    let mut app = fixture();
    let addon = metadata_addon(&mut app).await;
    app.db.lock().unwrap().execute(
        "INSERT INTO favorites(profile_id,id,type,name) VALUES(1,'tt0000001','series','Imported')",
        [],
    ).unwrap();
    let page = get_ok(&app, "/api/profiles/1/library").await;
    get_ok(&app, "/api/bootstrap").await;
    assert!(page["refresh_after_millis"].is_null(), "{page}");
    assert!(
        !super::hydration::running(&app, 1),
        "only a Stremio import starts a run"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(addon.calls.load(Ordering::SeqCst), 0);
    assert_eq!(context(&app, "tt0000001"), json!({}));
}

#[tokio::test]
async fn details_progress_counts_the_titles_of_a_run_until_it_finishes() {
    let mut app = fixture();
    let _addon = metadata_addon(&mut app).await;
    assert_eq!(
        get_ok(&app, "/api/profiles/1/library/details").await,
        json!({"running":false,"done":0,"filled":0,"total":0})
    );
    {
        let db = app.db.lock().unwrap();
        for n in 1..=24 {
            db.execute(
                "INSERT INTO favorites(profile_id,id,type,name) VALUES(1,?1,'movie','Imported')",
                [format!("tt{n:07}")],
            )
            .unwrap();
        }
        db.execute(
            "INSERT INTO favorites(profile_id,id,type,name) VALUES(1,'kitsu:42','series','Opaque')",
            [],
        )
        .unwrap();
    }
    assert!(start(&app));
    let (mut seen_partial, mut last_done) = (false, 0);
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let finished = loop {
        let status = get_ok(&app, "/api/profiles/1/library/details").await;
        let object = status.as_object().unwrap();
        assert_eq!(object.len(), 4, "only the four counters: {status}");
        let (done, total) = (
            status["done"].as_u64().unwrap(),
            status["total"].as_u64().unwrap(),
        );
        assert!(done >= last_done, "done never goes back: {status}");
        assert!(
            status["filled"].as_u64().unwrap() <= done,
            "only looked-up titles can be filled: {status}"
        );
        assert!(total == 0 || done <= total, "{status}");
        assert!(total == 0 || total == 25, "{status}");
        last_done = done;
        if status["running"] == false {
            break status;
        }
        seen_partial |= total == 25 && done > 0 && done < total;
        assert!(
            std::time::Instant::now() < deadline,
            "the run never finished"
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
    };
    assert!(seen_partial, "progress is visible while the run is active");
    assert_eq!(
        finished,
        json!({"running":false,"done":25,"filled":24,"total":25})
    );
}

#[tokio::test]
async fn details_progress_belongs_to_the_session_profile_only() {
    let mut app = fixture();
    let _addon = metadata_addon(&mut app).await;
    for (token, path) in [
        ("member-token-1", "/api/profiles/2/library/details"),
        ("member-token-2", "/api/profiles/1/library/details"),
    ] {
        let (status, _) = request(&app, token, "GET", path, Value::Null).await;
        assert!(
            status == StatusCode::FORBIDDEN || status == StatusCode::NOT_FOUND,
            "{token} {path}: {status}"
        );
    }
    let (status, _) = request(
        &app,
        "no-such-token",
        "GET",
        "/api/profiles/1/library/details",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn finished_counts_stay_until_the_next_run() {
    let mut app = fixture();
    let _addon = metadata_addon(&mut app).await;
    {
        let db = app.db.lock().unwrap();
        for n in 1..=12 {
            db.execute(
                "INSERT INTO favorites(profile_id,id,type,name) VALUES(1,?1,'movie','Imported')",
                [format!("tt{n:07}")],
            )
            .unwrap();
        }
    }
    assert!(start(&app));
    super::hydration::settled(&app, 1).await;
    assert_eq!(
        get_ok(&app, "/api/profiles/1/library/details").await,
        json!({"running":false,"done":12,"filled":12,"total":12})
    );
    get_ok(&app, "/api/profiles/1/library").await;
    assert_eq!(
        get_ok(&app, "/api/profiles/1/library/details").await,
        json!({"running":false,"done":12,"filled":12,"total":12})
    );
}
