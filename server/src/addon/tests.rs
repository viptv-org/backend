use super::*;
#[test]
fn endpoint_encodes_untrusted_ids() {
    let u = Addons::endpoint(
        "https://host/key/manifest.json",
        &["meta", "movie", "../../secret.json"],
    )
    .unwrap();
    assert!(u.contains("..%2F..%2Fsecret.json"));
}
#[tokio::test]
async fn catalog_cache_and_protocol_without_skip() {
    use axum::{routing::get, Router};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let mock = Router::new()
            .route("/manifest.json", get(|| async { axum::Json(json!({"id":"mock","name":"Mock","resources":["catalog"],"types":["movie"],"catalogs":[{"id":"top","type":"movie","name":"Top"}]})) }))
            .route("/catalog/movie/top.json", get(move || { let counter = counter.clone(); async move { counter.fetch_add(1, Ordering::SeqCst); axum::Json(json!({"metas":[{"id":"tt42","type":"movie","name":"Cached"}]})) } }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let addons = Addons::new(
        Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
        reqwest::Client::builder().no_proxy().build().unwrap(),
    )
    .unwrap();
    let saved = addons
        .add(&format!("http://{address}/manifest.json"))
        .await
        .unwrap();
    for _ in 0..2 {
        let response = addons
            .discover("movie".into(), None, saved["id"].as_i64(), 0, None, None)
            .await
            .unwrap();
        assert_eq!(response["metas"][0]["id"], "tt42");
    }
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert!(addons
        .discover("movie".into(), None, saved["id"].as_i64(), 100, None, None)
        .await
        .is_err());
    task.abort();
}
fn test_addons() -> Addons {
    let addons = Addons::new(
        Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
        reqwest::Client::builder().no_proxy().build().unwrap(),
    )
    .unwrap();
    addons.delete(1).unwrap();
    addons
}

#[tokio::test]
async fn concurrent_fetches_share_upstream_and_recover_after_cancellation() {
    use axum::{routing::get, Router};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let mock = Router::new().route(
        "/slow",
        get(move || {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(80)).await;
                axum::Json(json!({"metas": [{"id": "shared"}]}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/slow", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let addons = test_addons();
    let responses = futures::future::join_all((0..16).map(|_| addons.fetch(&url, 300))).await;
    assert!(responses
        .iter()
        .all(|value| value.as_ref().unwrap()["metas"][0]["id"] == "shared"));
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    addons.cache.lock().unwrap().clear();
    let owner = addons.clone();
    let pending_url = url.clone();
    let pending = tokio::spawn(async move { owner.fetch(&pending_url, 300).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while hits.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    pending.abort();
    let _ = pending.await;
    let recovered = tokio::time::timeout(Duration::from_secs(2), addons.fetch(&url, 300))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered["metas"][0]["id"], "shared");
    assert_eq!(hits.load(Ordering::SeqCst), 3);
    server.abort();
}

#[tokio::test]
async fn complete_episode_art_does_not_wait_for_slow_secondary_metadata() {
    use axum::{routing::get, Router};
    let response = json!({"meta": {"id": "tt-series", "type": "series", "name": "Series", "videos": [{"id": "tt-series:1:1", "season": 1, "episode": 1, "thumbnail": "https://image.tmdb.org/t/p/w500/episode.jpg"}]}});
    let mock = Router::new()
        .route(
            "/primary/meta/series/tt-series.json",
            get(move || {
                let response = response.clone();
                async move { axum::Json(response) }
            }),
        )
        .route(
            "/secondary/meta/series/tt-series.json",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(2)).await;
                axum::Json(json!({"meta": {}}))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let host = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let addons = test_addons();
    let manifest = json!({"resources": ["meta"], "types": ["series"]});
    insert(
        &addons,
        "Primary",
        &format!("http://{host}/primary/manifest.json"),
        manifest.clone(),
        0,
    );
    insert(
        &addons,
        "Secondary",
        &format!("http://{host}/secondary/manifest.json"),
        manifest,
        1,
    );
    let result = tokio::time::timeout(
        Duration::from_millis(500),
        addons.meta("series", "tt-series"),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result["meta"]["id"], "tt-series");
    assert!(result["meta"]["videos"][0]["thumbnail"]
        .as_str()
        .unwrap()
        .contains("wsrv.nl"));
    server.abort();
}
fn insert(addons: &Addons, name: &str, url: &str, manifest: Value, priority: i64) -> i64 {
    let db = addons.db.lock().unwrap();
    db.execute(
        "INSERT INTO addons(name,manifest_url,manifest,priority) VALUES(?1,?2,?3,?4)",
        params![name, url, manifest.to_string(), priority],
    )
    .unwrap();
    db.last_insert_rowid()
}
#[test]
fn seed_migration_and_restarts_never_restore_deleted_addons() {
    let path = std::env::temp_dir().join(format!(
        "viptv-addon-{}-{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let open = || {
        Addons::new(
            Arc::new(Mutex::new(Connection::open(&path).unwrap())),
            reqwest::Client::new(),
        )
        .unwrap()
    };
    let addons = open();
    assert_eq!(addons.list().unwrap().as_array().unwrap().len(), 1);
    addons.update(1, json!({"enabled":false})).unwrap();
    drop(addons);
    let addons = open();
    assert_eq!(addons.list().unwrap()[0]["enabled"], false);
    assert!(addons.list().unwrap()[0]["priority"].is_null());
    addons.delete(1).unwrap();
    drop(addons);
    assert_eq!(open().list().unwrap(), json!([]));
    std::fs::remove_file(&path).unwrap();
    // Both empty and populated pre-priority schemas are already initialized.
    for populated in [false, true] {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE addons(id INTEGER PRIMARY KEY,name TEXT NOT NULL,manifest_url TEXT UNIQUE NOT NULL,enabled INTEGER NOT NULL DEFAULT 1,manifest TEXT NOT NULL)").unwrap();
        if populated {
            db.execute(
                "INSERT INTO addons VALUES(4,'Legacy','https://legacy/manifest.json',0,'{}')",
                [],
            )
            .unwrap();
        }
        let db = Arc::new(Mutex::new(db));
        for _ in 0..2 {
            let addons = Addons::new(db.clone(), reqwest::Client::new()).unwrap();
            let list = addons.list().unwrap();
            assert_eq!(list.as_array().unwrap().len(), usize::from(populated));
            if populated {
                assert!(list[0]["priority"].is_null());
                assert_eq!(list[0]["enabled"], false);
            }
        }
    }
}
#[test]
fn patch_validation_and_order() {
    let addons = test_addons();
    let a = insert(&addons, "A", "https://a/manifest.json", json!({}), 0);
    let b = insert(&addons, "B", "https://b/manifest.json", json!({}), 0);
    assert_eq!(
        addons
            .entries()
            .unwrap()
            .iter()
            .map(|e| e.0)
            .collect::<Vec<_>>(),
        vec![a, b]
    );
    for patch in [
        json!(null),
        json!([]),
        json!({}),
        json!({"enabled":1}),
        json!({"enabled":null}),
        json!({"priority":1.5}),
        json!({"priority":"2"}),
        json!({"priority":18446744073709551615u64}),
        json!({"name":"bad"}),
        json!({"enabled":false,"priority":"bad"}),
    ] {
        assert!(addons.update(a, patch).is_err());
    }
    assert!(addons.update(999, json!({"enabled":false})).is_err());
    assert_eq!(addons.entries().unwrap().len(), 2);
    assert!(addons.update(b, json!({"priority":-1})).is_err());
    assert_eq!(addons.entries().unwrap()[0].0, a);
    addons.update(b, json!({"enabled":false})).unwrap();
    assert_eq!(addons.entries().unwrap().len(), 1);
    assert_eq!(addons.list().unwrap()[1]["id"], b);
    assert_eq!(addons.list().unwrap()[1]["enabled"], false);
}
#[tokio::test]
async fn ordered_catalogs_raw_pagination_search_constraints_and_meta() {
    use axum::{extract::OriginalUri, routing::get, Router};
    let paths = Arc::new(Mutex::new(Vec::<String>::new()));
    let captured = paths.clone();
    let mock = Router::new().fallback(get(move |OriginalUri(uri): OriginalUri| {
            let captured = captured.clone();
            async move {
                let path = uri.path().to_owned();
                captured.lock().unwrap().push(path.clone());
                if path == "/slow/manifest.json" {
                    return axum::Json(json!({"id":"slow","name":"Refreshed","resources":["catalog","meta"],"types":["movie"],"catalogs":[{"id":"custom","type":"movie","extra":[{"name":"skip"},{"name":"search"}]}]}));
                }
                if path.starts_with("/slow/") { tokio::time::sleep(Duration::from_millis(40)).await; }
                if path.contains("/meta/") { return axum::Json(json!({"meta":{"id":"tt1","name":if path.starts_with("/slow/") {"preferred"} else {"fast"}}})); }
                if path.contains("skip=205") { return axum::Json(json!({"metas":[]})); }
                let metas: Vec<_> = (0..205).map(|i| json!({"id":format!("tt{}",i/2),"type":"movie","name":if path.starts_with("/slow/") {"preferred"} else {"fast"}})).collect();
                axum::Json(json!({"metas":metas}))
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let addons = test_addons();
    let manifest = json!({"resources":["catalog","meta"],"types":["movie"],"catalogs":[{"id":"custom","type":"movie","extra":[{"name":"skip"},{"name":"search"}]},{"id":"other","type":"movie","extraSupported":["search"]}]});
    let fast = insert(
        &addons,
        "Fast",
        &format!("http://{address}/fast/manifest.json"),
        manifest.clone(),
        0,
    );
    let slow_url = format!("http://{address}/slow/manifest.json");
    let slow = insert(&addons, "Slow", &slow_url, manifest, -1);
    let page = addons
        .discover("movie".into(), None, None, 0, None, None)
        .await
        .unwrap();
    assert_eq!(page["metas"].as_array().unwrap().len(), 103);
    assert_eq!(page["metas"][0]["name"], "fast");
    assert_eq!(page["next_skip"], 205);
    assert_eq!(page["has_more"], true);
    assert_eq!(
        *paths.lock().unwrap(),
        vec!["/fast/catalog/movie/custom/skip=0.json"]
    );
    paths.lock().unwrap().clear();
    let whitespace = addons
        .discover("movie".into(), None, None, 0, Some("   ".into()), None)
        .await
        .unwrap();
    assert_eq!(whitespace["aggregated"], false);
    assert_eq!(whitespace["next_skip"], 205);
    paths.lock().unwrap().clear();
    let empty = addons
        .discover("movie".into(), None, None, 205, None, None)
        .await
        .unwrap();
    assert_eq!(empty["has_more"], false);
    assert!(empty["next_skip"].is_null());
    assert_eq!(
        addons.meta("movie", "tt1").await.unwrap()["meta"]["name"],
        "fast"
    );
    paths.lock().unwrap().clear();
    let search = addons
        .discover(
            "movie".into(),
            Some("other".into()),
            Some(fast),
            0,
            Some("a/b &?%".into()),
            None,
        )
        .await
        .unwrap();
    assert_eq!(search["has_more"], false);
    assert!(search["next_skip"].is_null());
    assert_eq!(search["aggregated"], false);
    // meta() cancels its lower-priority futures once the preferred result
    // arrives, but an already-sent metadata request can reach this mock
    // after clear(). Count catalog requests, not unrelated in-flight work.
    let requests = paths
        .lock()
        .unwrap()
        .iter()
        .filter(|path| path.contains("/catalog/"))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(requests.len(), 1, "observed catalog requests: {requests:?}");
    assert!(requests[0].starts_with("/fast/catalog/movie/other/search="));
    assert!(requests[0].contains("a%2Fb+%26%3F%25"));
    assert!(addons
        .discover("movie".into(), None, None, 1, Some("x".into()), None)
        .await
        .is_err());
    assert!(addons
        .discover(
            "movie".into(),
            Some("missing".into()),
            None,
            0,
            Some("x".into()),
            None,
        )
        .await
        .is_err());
    addons.update(slow, json!({"enabled":false})).unwrap();
    let saved = addons.add(&slow_url).await.unwrap();
    assert_eq!(saved["enabled"], false);
    assert!(saved["priority"].is_null());
    assert_eq!(saved["name"], "Refreshed");
    assert_eq!(
        addons
            .discover("movie".into(), None, None, 0, None, None)
            .await
            .unwrap()["metas"][0]["name"],
        "fast"
    );
    task.abort();
}
#[tokio::test]
async fn bounded_search_and_raw_count_before_truncation() {
    let addons = test_addons();
    let base = "http://127.0.0.1:1/manifest.json";
    let catalogs: Vec<_> = (0..40).map(|i| json!({"id":format!("c{i}"),"type":"movie","extra":[{"name":"search"},{"name":"skip"}]})).collect();
    let id = insert(
        &addons,
        "Many",
        base,
        json!({"resources":["catalog"],"catalogs":catalogs}),
        0,
    );
    for i in 0..32 {
        let endpoint = viptv_provider::discover::addon_extra_endpoint(
            base,
            "movie",
            &format!("c{i}"),
            "skip=0&search=x",
        )
        .unwrap();
        let metas: Vec<_> = (0..210)
            .map(|j| json!({"id":format!("{i}-{j}"),"type":"movie"}))
            .collect();
        addons
            .cache
            .lock()
            .unwrap()
            .insert(endpoint, (now() + 300, json!({"metas":metas}), 1));
    }
    let search = addons
        .discover("movie".into(), None, Some(id), 0, Some("x".into()), None)
        .await
        .unwrap();
    assert_eq!(search["metas"].as_array().unwrap().len(), 200);
    assert_eq!(search["metas"][0]["id"], "0-0");
    assert_eq!(search["metas"][199]["id"], "0-199");
    assert_eq!(search["has_more"], false);
    let endpoint =
        viptv_provider::discover::addon_extra_endpoint(base, "movie", "c0", "skip=0").unwrap();
    let metas: Vec<_> = (0..250).map(|j| json!({"id":j,"type":"movie"})).collect();
    addons
        .cache
        .lock()
        .unwrap()
        .insert(endpoint, (now() + 300, json!({"metas":metas}), 1));
    let page = addons
        .discover("movie".into(), None, None, 0, None, None)
        .await
        .unwrap();
    assert_eq!(page["metas"].as_array().unwrap().len(), 200);
    assert_eq!(page["next_skip"], 250);
    assert!(addons
        .discover("movie".into(), None, None, 10001, None, None)
        .await
        .is_err());
}
#[test]
fn catalogs_preserve_bounded_normalized_extra_capabilities() {
    let addons = test_addons();
    let mut options = (0..70)
        .map(|index| Value::String(format!("Genre {index}")))
        .collect::<Vec<_>>();
    options[0] = Value::String("x".repeat(200));
    insert(
        &addons,
        "Capabilities",
        "http://127.0.0.1:1/manifest.json",
        json!({
            "resources":["catalog"],
            "catalogs":[{
                "id":"discover",
                "type":"movie",
                "name":"Discover movies",
                "extra":[
                    {"name":"genre","isRequired":true,"options":options,"optionsLimit":5000},
                    "search"
                ],
                "extraSupported":["skip","genre","not valid"],
                "extraRequired":["search"],
                "genres":["Legacy genre"]
            }]
        }),
        0,
    );
    let catalogs = addons.catalogs().unwrap();
    let catalog = &catalogs[0];
    assert_eq!(catalog["supports_search"], true);
    assert_eq!(catalog["supports_skip"], true);
    assert_eq!(catalog["genres"].as_array().unwrap().len(), 70);
    assert_eq!(catalog["extra"].as_array().unwrap().len(), 3);
    let genre = catalog["extra"]
        .as_array()
        .unwrap()
        .iter()
        .find(|extra| extra["name"] == "genre")
        .unwrap();
    assert_eq!(genre["is_required"], true);
    assert_eq!(genre["options_limit"], 1000);
    assert_eq!(genre["options"][0], "Genre 1");
    assert!(!genre["options"]
        .as_array()
        .unwrap()
        .iter()
        .any(|option| option.as_str().unwrap().chars().count() > MAX_EXTRA_OPTION));
    let search = catalog["extra"]
        .as_array()
        .unwrap()
        .iter()
        .find(|extra| extra["name"] == "search")
        .unwrap();
    assert_eq!(search["is_required"], true);
    assert!(search["options"].as_array().unwrap().is_empty());
}
#[tokio::test]
async fn genre_discover_requires_advertisement_validates_options_and_encodes_path() {
    let addons = test_addons();
    let base = "http://127.0.0.1:1/manifest.json";
    let addon = insert(
        &addons,
        "Genres",
        base,
        json!({
            "resources":["catalog"],
            "catalogs":[{
                "id":"discover",
                "type":"movie",
                "extra":[{"name":"genre","isRequired":true,"options":["Family & Kids","Comedy"]},{"name":"skip"}]
            }]
        }),
        0,
    );
    let endpoint = viptv_provider::discover::addon_extra_endpoint(
        base,
        "movie",
        "discover",
        "skip=0&genre=Family+%26+Kids",
    )
    .unwrap();
    addons.cache.lock().unwrap().insert(
        endpoint,
        (
            now() + 300,
            json!({"metas":[{"id":"tt-kids","type":"movie","name":"Kids"}]}),
            1,
        ),
    );
    let result = addons
        .discover(
            "movie".into(),
            Some("discover".into()),
            Some(addon),
            0,
            None,
            Some("Family & Kids".into()),
        )
        .await
        .unwrap();
    assert_eq!(result["metas"][0]["id"], "tt-kids");
    let invalid = addons
        .discover(
            "movie".into(),
            Some("discover".into()),
            Some(addon),
            0,
            None,
            Some("Horror".into()),
        )
        .await
        .unwrap_err();
    assert!(invalid.contains("advertised options"));
    let missing = addons
        .discover(
            "movie".into(),
            Some("discover".into()),
            Some(addon),
            0,
            None,
            None,
        )
        .await
        .unwrap_err();
    assert!(missing.contains("No catalog source"));

    let unsupported = test_addons();
    let unsupported_id = insert(
        &unsupported,
        "No genres",
        base,
        json!({"resources":["catalog"],"catalogs":[{"id":"top","type":"movie"}]}),
        0,
    );
    let error = unsupported
        .discover(
            "movie".into(),
            Some("top".into()),
            Some(unsupported_id),
            0,
            None,
            Some("Comedy".into()),
        )
        .await
        .unwrap_err();
    assert!(error.contains("does not advertise genre"));
}
#[tokio::test]
async fn metadata_falls_back_in_priority_order() {
    let addons = test_addons();
    for (i, response) in [
        json!({"meta":null}),
        json!({"meta":{"name":"second"}}),
        json!({"meta":{"name":"third"}}),
    ]
    .into_iter()
    .enumerate()
    {
        let base = format!("http://127.0.0.1:1/{i}/manifest.json");
        insert(
            &addons,
            "Meta",
            &base,
            json!({"resources":["meta"],"types":["movie"],"idPrefixes":["tt"]}),
            i as i64,
        );
        let endpoint = Addons::endpoint(&base, &["meta", "movie", "tt1.json"]).unwrap();
        addons
            .cache
            .lock()
            .unwrap()
            .insert(endpoint, (now() + 300, response, 1));
    }
    assert_eq!(
        addons.meta("movie", "tt1").await.unwrap()["meta"]["name"],
        "second"
    );
    assert!(addons.meta("movie", "other").await.is_err());
}
#[test]
fn root_prefixes_and_search_extras() {
    assert!(!supports(
        &json!({"resources":[{"name":"meta","idPrefixes":["abc"]}],"idPrefixes":["tt"]}),
        "meta",
        "movie",
        "abc1"
    ));
    assert!(!supports(
        &json!({"resources":[{"name":"meta","idPrefixes":["tt1"]}],"idPrefixes":["tt"]}),
        "meta",
        "movie",
        "tt9"
    ));
    for resources in [
        json!(["meta"]),
        json!([{"name":"meta","types":["movie"],"idPrefixes":["tt1"]}]),
    ] {
        let m = json!({"resources":resources,"types":["movie"],"idPrefixes":["tt"]});
        assert!(supports(&m, "meta", "movie", "tt123"));
        assert!(!supports(&m, "meta", "movie", "other"));
    }
    assert!(!supports(
        &json!({"resources":["meta"],"idPrefixes":[]}),
        "meta",
        "movie",
        "tt1"
    ));
    assert!(catalog_extra(
        &json!({"extra":[{"name":"search"}]}),
        "search"
    ));
    assert!(catalog_extra(
        &json!({"extraSupported":["search"]}),
        "search"
    ));
    assert!(!catalog_extra(
        &json!({"extra":[{"name":"genre"}]}),
        "search"
    ));
}
#[test]
fn respects_resource_prefixes() {
    assert!(!supports(
        &json!({"resources":[{"name":"stream","idPrefixes":["tt"],"types":["movie"]}]}),
        "stream",
        "movie",
        "abc"
    ));
}
#[tokio::test]
async fn custom_catalog_types_options_and_explicit_search_pages_work() {
    let addons = test_addons();
    let base = "http://127.0.0.1:1/manifest.json";
    let languages: Vec<_> = (0..186).map(|i| format!("Language {i}")).collect();
    let addon = insert(
        &addons,
        "Expanded metadata",
        base,
        json!({"resources":["catalog"],"catalogs":[
            {"id":"search.anime","type":"anime.series","extra":[{"name":"search","isRequired":true},{"name":"skip"}]},
            {"id":"calendar","type":"series","extra":[{"name":"calendarVideosIds","isRequired":true}]},
            {"id":"languages","type":"movie","extra":[{"name":"genre","isRequired":true,"options":languages,"default":"Language 100"}]}
        ]}),
        0,
    );
    let catalogs = addons.catalogs().unwrap();
    assert_eq!(catalogs[2]["genres"].as_array().unwrap().len(), 186);
    assert_eq!(catalogs[2]["extra"][0]["default"], "Language 100");
    let endpoint = viptv_provider::discover::addon_extra_endpoint(
        base,
        "anime.series",
        "search.anime",
        "skip=40&search=Naruto",
    )
    .unwrap();
    addons.cache.lock().unwrap().insert(
        endpoint,
        (
            now() + 300,
            json!({"metas":[{"id":"tt0409591","type":"series"}]}),
            1,
        ),
    );
    let page = addons
        .discover(
            "anime.series".into(),
            Some("search.anime".into()),
            Some(addon),
            40,
            Some("Naruto".into()),
            None,
        )
        .await
        .unwrap();
    assert_eq!(page["metas"][0]["type"], "series");
    assert_eq!(page["next_skip"], 41);
    assert_eq!(page["aggregated"], false);
    let request = |extras| DiscoverOptions {
        kind: "series".into(),
        catalog: Some("calendar".into()),
        addon: Some(addon),
        skip: 0,
        search: None,
        genre: None,
        extras,
    };
    let endpoint = viptv_provider::discover::addon_extra_endpoint(
        base,
        "series",
        "calendar",
        "calendarVideosIds=tt123%3A1%3A2%26x",
    )
    .unwrap();
    addons
        .cache
        .lock()
        .unwrap()
        .insert(endpoint, (now() + 300, json!({"metas":[]}), 1));
    let result = addons
        .discover_with_options(request(HashMap::from([(
            "calendarVideosIds".into(),
            "tt123:1:2&x".into(),
        )])))
        .await
        .unwrap();
    assert_eq!(result["has_more"], false);
    assert!(addons
        .discover_with_options(request(HashMap::new()))
        .await
        .is_err());
    assert!(addons
        .discover_with_options(request(HashMap::from([("unknown".into(), "x".into())])))
        .await
        .unwrap_err()
        .contains("not advertised"));
    assert!(addons
        .discover_with_options(request(HashMap::from([("skip".into(), "99".into())])))
        .await
        .is_err());
}
#[test]
fn episode_art_enrichment_preserves_identity_and_requires_matching_episode() {
    let mut primary = json!({"id":"tt5607616","videos":[{"id":"tt5607616:4:1","season":4,"episode":1,"released":"2026-04-08T00:00:00Z","thumbnail":"https://episodes.metahub.space/tt5607616/4/1/w780.jpg"}]});
    let alternate = json!({"id":"tt5607616","videos":[{"id":"kitsu:49746:1","season":4,"episode":1,"released":"2026-04-08T13:30:00Z","thumbnail":"https://api.top-posters.com/private/asset?fallback_url=https%3A%2F%2Fartworks.thetvdb.com%2Fepisode.jpg"}]});
    enrich_episode_art(&mut primary, &alternate);
    assert_eq!(primary["videos"][0]["id"], "tt5607616:4:1");
    let proxy = episode_art_url(&primary["videos"][0]["thumbnail"]).unwrap();
    assert!(proxy.starts_with("https://wsrv.nl/?"));
    assert!(proxy.contains("w=512") && !proxy.contains("private"));
    let mut wrong = primary.clone();
    wrong["videos"][0]["thumbnail"] = json!("");
    wrong["videos"][0]["released"] = json!("2026-04-09T00:00:00Z");
    enrich_episode_art(&mut wrong, &alternate);
    assert_eq!(wrong["videos"][0]["thumbnail"], "");
    for url in [
        "http://127.0.0.1/image.jpg",
        "https://provider.invalid/private.jpg",
        "https://artworks.thetvdb.com/image?token=secret",
    ] {
        assert!(episode_art_url(&json!(url)).is_none());
    }
}
