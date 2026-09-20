//! Entirely in-process contract tests: no listeners, provider requests or temp files.
use super::*;
use axum::body::to_bytes;

#[tokio::test]
async fn playback_errors_are_closed_and_leave_expiry_unchanged() {
    for (status, message, code) in [
        (
            StatusCode::BAD_REQUEST,
            "Stream expired; discover again",
            None::<&str>,
        ),
        (
            StatusCode::BAD_REQUEST,
            "Requested input audio track is unavailable",
            None::<&str>,
        ),
        (
            StatusCode::BAD_REQUEST,
            "Playback failed with an upstream error",
            None::<&str>,
        ),
        (
            StatusCode::NOT_ACCEPTABLE,
            "Playback engine unavailable",
            None::<&str>,
        ),
        (
            StatusCode::NOT_ACCEPTABLE,
            "Playback could not start; try forced transcoding or another stream",
            None::<&str>,
        ),
        (
            StatusCode::NOT_ACCEPTABLE,
            "Could not inspect source video safely; try another stream",
            None::<&str>,
        ),
    ] {
        let response = ApiError(status, message.into()).into_response();
        assert_eq!(response.status(), status);
        let bytes = to_bytes(response.into_body(), 4096).await.unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        let expected = match code {
            Some(code) => json!({"error":message,"error_code":code}),
            None => json!({"error":message}),
        };
        assert_eq!(value, expected);
    }
}

const ACCOUNT_TOKEN: &str = "contract-account-token-only";
fn app(db: Connection) -> App {
    let mut app = crate::test_support::app_with_db(db);
    let session_id = "contract-session".to_owned();
    {
        let db = app.db.lock().unwrap();
        db.execute("DELETE FROM addons", []).unwrap();
        db.execute("INSERT OR IGNORE INTO auth_accounts(id,username,name,password_hash,role,recovery_hash,created_at) VALUES(1,'contract','Contract','unused','owner','unused',0)",[]).unwrap();
        db.execute("INSERT OR IGNORE INTO profiles(id,name,avatar_seed,presentation_complete) VALUES(1,'Contract','contract-seed',1)",[]).unwrap();
        db.execute("UPDATE profiles SET presentation_complete=1 WHERE id=1", [])
            .unwrap();
        db.execute(
            "INSERT OR REPLACE INTO profile_owners(profile_id,account_id,created_at) VALUES(1,1,0)",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT OR IGNORE INTO auth_profiles(account_id,profile_id) VALUES(1,1)",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO auth_sessions(id,account_id,access_hash,refresh_hash,csrf_hash,profile_id,kind,device_name,access_expires,refresh_expires,created_at) VALUES(?1,1,?2,'contract-refresh','contract-csrf',1,'browser','contract',?3,?3,0)",params![session_id,format!("{:x}",Sha256::digest(ACCOUNT_TOKEN.as_bytes())),util::now()+3600]).unwrap();
    }
    let principal = auth::Principal::Account {
        account_id: 1,
        role: "owner".into(),
        profile_id: Some(1),
        session_id: Some(session_id.clone()),
    };
    app.principal = Some(principal.clone());
    app.lease = Some(ResourceLease {
        policy_revision: 0,
        principal,
        session_id: Some(session_id),
    });
    app
}
async fn api(app: &App, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    crate::test_support::request(app, ACCOUNT_TOKEN, method, path, body).await
}
#[tokio::test]
async fn live_categories_route_returns_paginated_shape() {
    let a = app(Connection::open_in_memory().unwrap());
    for path in [
        "/api/live/categories",
        "/api/live/categories?offset=2&limit=9999",
    ] {
        let (status, body) = api(&a, "GET", path, Value::Null).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({"categories":[],"total":0}));
    }
    {
        let db = a.db.lock().unwrap();
        for id in 1..=4 {
            db.execute("INSERT INTO providers(id,name,url,username,password) VALUES(?1,'Fixture','https://fixture.invalid','fixture','fixture')",[id]).unwrap();
        }
        db.execute("UPDATE providers SET enable_live=0 WHERE id=3", [])
            .unwrap();
        db.execute("UPDATE providers SET enabled=0 WHERE id=4", [])
            .unwrap();
        for id in 0..101 {
            db.execute("INSERT INTO provider_live(id,provider_id,stream_id,name,category) VALUES(?1,1,?2,'Channel',?3)",params![format!("iptv:1:{id}"),id.to_string(),format!("Group{id:03}")]).unwrap();
        }
        for (provider, stream, category, category_id) in [
            (1, 1001, Some("News"), Some("7")),
            (1, 1002, Some(" News "), Some("7")),
            (2, 1003, Some("News"), Some("91")),
            (1, 1004, Some("7"), Some("99")),
            (1, 1005, None, None),
            (3, 1006, Some("HiddenOnly"), Some("3")),
            (4, 1007, Some("DisabledOnly"), Some("4")),
        ] {
            db.execute("INSERT INTO provider_live(id,provider_id,stream_id,name,category,category_id) VALUES(?1,?2,?3,'Channel',?4,?5)",params![format!("iptv:{provider}:{stream}"),provider,stream.to_string(),category,category_id]).unwrap();
        }
    }
    let (status, first) = api(&a, "GET", "/api/live/categories?limit=9999", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["total"], 104);
    assert_eq!(first["categories"].as_array().unwrap().len(), 100);
    let (status, next) = api(
        &a,
        "GET",
        "/api/live/categories?offset=100&limit=100",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(next["total"], 104);
    assert_eq!(next["categories"].as_array().unwrap().len(), 4);
    let categories: Vec<&Value> = first["categories"]
        .as_array()
        .unwrap()
        .iter()
        .chain(next["categories"].as_array().unwrap())
        .collect();
    let ids: std::collections::HashSet<_> = categories
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 104);
    assert!(categories
        .iter()
        .all(|row| row.as_object().unwrap().len() == 3
            && row["name"] != "HiddenOnly"
            && row["name"] != "DisabledOnly"));
    let news = categories.iter().find(|row| row["name"] == "News").unwrap();
    assert_eq!(news["id"], "category:News");
    assert_eq!(news["count"], 3);
    for (category, total) in [
        ("category%3ANews", 3),
        ("category%3A7", 1),
        ("category%3A", 1),
        ("7", 3),
    ] {
        let (status, channels) = api(
            &a,
            "GET",
            &format!("/api/live?category={category}&limit=100"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(channels["total"], total);
    }
}

#[tokio::test]
async fn playback_track_request_is_strict_and_unsupported_selection_never_probes() {
    assert!(serde_json::from_value::<PlaybackRequest>(json!({"stream_id":"source"})).is_ok());
    for value in [json!(-1), json!(1.5), json!("2"), json!(4294967296_u64)] {
        assert!(serde_json::from_value::<PlaybackRequest>(
            json!({"stream_id":"source","audio_track_index":value})
        )
        .is_err());
    }
    assert!(
        serde_json::from_value::<PlaybackRequest>(json!({"allow_unknown_audio":true})).is_err()
    );
    let a = app(Connection::open_in_memory().unwrap());
    a.streams.lock().unwrap().insert(
        "source".into(),
        StreamEntry {
            provider_id: None,
            kind: "movie".into(),
            live: false,
            url: "https://source.invalid/private-secret-path".into(),
            headers: HashMap::new(),
            created: Instant::now(),
        },
    );
    a.own_resource("stream", "source");
    for (request, error) in [
        (
            json!({"stream_id":"source","subtitle_track_index":65536}),
            "Requested input subtitle track index is out of range",
        ),
        (
            json!({"stream_id":"source","audio_track_index":65536}),
            "Requested input audio track index is out of range",
        ),
    ] {
        let (status, body) = api(&a, "POST", "/api/playback", request).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body, json!({"error":error}));
        assert_eq!(a.playback.active_count().await, 0);
    }
}

fn indexed_series(app: &App) {
    let db = app.db.lock().unwrap();
    db.execute_batch("INSERT INTO providers(id,name,url,username,password) VALUES(1,'Fixture','https://provider.invalid','fixture','fixture');
        INSERT INTO provider_vod(id,provider_id,stream_id,kind,name,normalized,year,extension) VALUES('candidate',1,'33','series','Canonical Series','canonical series',2001,'mp4');").unwrap();
    db.execute("INSERT INTO provider_cache(provider_id,cache_key,expires_at,payload) VALUES(1,'get_series_info:33',?1,?2)",params![util::now()+3600,json!({"episodes":{"2":[{"id":44,"episode_num":4,"container_extension":"mp4"}]}}).to_string()]).unwrap();
}
async fn discover(app: &App, body: Value) -> Value {
    let (status, job) = api(app, "POST", "/api/streams", body).await;
    assert_eq!(status, StatusCode::OK, "{job}");
    let id = job["id"].as_str().unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let job = app.jobs.lock().unwrap().get(id).unwrap().clone();
            let notified = job.notify.notified();
            if job.state.lock().unwrap().pending == 0 {
                break;
            }
            notified.await;
        }
    })
    .await
    .unwrap();
    api(
        app,
        "GET",
        &format!("/api/streams/{id}?after=0"),
        Value::Null,
    )
    .await
    .1
}

#[tokio::test]
async fn opaque_episode_progress_roundtrip_rediscovers_without_metadata() {
    let app = app(Connection::open_in_memory().unwrap());
    indexed_series(&app);
    let original = json!({"id":"addon-opaque-episode","type":"series","name":"Canonical Series","position":540.0,"duration":1800.0,
        "series_id":"custom:parent","season":2,"episode":4,"year":2001,"releaseInfo":"2001–2005","imdb_id":"tt1234567","tmdb_id":42});
    assert_eq!(
        api(&app, "PUT", "/api/profiles/1/progress", original.clone())
            .await
            .0,
        StatusCode::OK
    );
    let (_, rows) = api(&app, "GET", "/api/profiles/1/progress", Value::Null).await;
    let row = rows[0].clone();
    for field in [
        "id",
        "name",
        "position",
        "duration",
        "series_id",
        "season",
        "episode",
        "year",
        "releaseInfo",
        "imdb_id",
    ] {
        assert_eq!(row[field], original[field], "{field}");
    }
    assert_eq!(row["tmdb_id"], "42");
    let result = discover(&app, row).await;
    assert_eq!(result["done"], true);
    let streams: Vec<_> = result["events"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|event| event["streams"].as_array().unwrap())
        .collect();
    assert_eq!(streams.len(), 1, "{result}");
    {
        let source = app.streams.lock().unwrap();
        let entry = source.get(streams[0]["id"].as_str().unwrap()).unwrap();
        assert!(entry.url.ends_with("/44.mp4"));
    }
    // An old client must not erase context written by a newer client.
    assert_eq!(api(&app,"PUT","/api/profiles/1/progress",json!({"id":"addon-opaque-episode","type":"series","name":"Canonical Series","position":600,"duration":1800})).await.0,StatusCode::OK);
    let rows = api(&app, "GET", "/api/profiles/1/progress", Value::Null)
        .await
        .1;
    assert_eq!(rows[0]["season"], 2);
    assert_eq!(rows[0]["position"], 600.0);
}

#[tokio::test]
async fn migration_and_old_client_are_idempotent() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE progress(profile_id INTEGER NOT NULL,id TEXT NOT NULL,type TEXT NOT NULL,name TEXT NOT NULL,poster TEXT,position REAL NOT NULL,duration REAL NOT NULL,updated_at INTEGER NOT NULL,PRIMARY KEY(profile_id,type,id)); INSERT INTO progress VALUES(1,'old','movie','Old',NULL,10,100,1);").unwrap();
    let app = app(db);
    init_progress_context(&app.db.lock().unwrap()).unwrap();
    init_progress_context(&app.db.lock().unwrap()).unwrap();
    let rows = api(&app, "GET", "/api/profiles/1/progress", Value::Null)
        .await
        .1;
    assert_eq!(rows[0]["id"], "old");
    assert!(rows[0].get("series_id").is_none());
    assert_eq!(
        api(
            &app,
            "PUT",
            "/api/profiles/1/progress",
            json!({"id":"old","type":"movie","name":"Old","position":20,"duration":100})
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn invalid_optional_context_rejected_before_save_or_job() {
    let app = app(Connection::open_in_memory().unwrap());
    for (key, value) in [
        ("series_id", json!(" ")),
        ("series_id", json!("x".repeat(513))),
        ("series_id", json!("parent\u{0001}id")),
        ("year", json!(1869)),
        ("year", json!(1800)),
        ("imdb_id", json!("tt1")),
        ("imdb_id", json!("tt1234")),
        ("tmdb_id", json!(2147483648_u64)),
        ("releaseInfo", json!("x".repeat(129))),
        ("season", json!(-1)),
        ("episode", json!(1.5)),
        ("episode", json!("2")),
        ("year", json!(2201)),
        ("releaseInfo", json!({})),
        ("imdb_id", json!("ttbad")),
        ("tmdb_id", json!(0)),
    ] {
        let mut body =
            json!({"id":"opaque","type":"series","name":"Title","position":1,"duration":100});
        body[key] = value;
        assert_eq!(
            api(&app, "PUT", "/api/profiles/1/progress", body.clone())
                .await
                .0,
            StatusCode::BAD_REQUEST,
            "{key}"
        );
        assert_eq!(
            api(&app, "POST", "/api/streams", body).await.0,
            StatusCode::BAD_REQUEST,
            "{key}"
        );
    }
    assert!(app.jobs.lock().unwrap().is_empty());
    assert_eq!(
        api(&app, "GET", "/api/profiles/1/progress", Value::Null)
            .await
            .1,
        json!([])
    );
}

#[tokio::test]
async fn final_roku_wire_get_accepts_parent_512_and_emits_decimal_tmdb() {
    let app = app(Connection::open_in_memory().unwrap());
    let parent = format!("opaque parent {}", "x".repeat(498));
    assert_eq!(parent.len(), 512);
    let body = json!({"id":"original opaque episode","type":"series","name":"Canonical Name","position":42,"duration":100,
        "series_id":parent,"season":0,"episode":100000,"year":1870,"releaseInfo":"1870","imdb_id":"tt12345","tmdb_id":"tmdb:2147483647"});
    assert_eq!(
        api(&app, "PUT", "/api/profiles/1/progress", body.clone())
            .await
            .0,
        StatusCode::OK
    );
    let rows = api(&app, "GET", "/api/profiles/1/progress", Value::Null)
        .await
        .1;
    assert_eq!(rows[0]["tmdb_id"], "2147483647");
    assert!(rows[0]["tmdb_id"]
        .as_str()
        .unwrap()
        .bytes()
        .all(|b| b.is_ascii_digit()));
    for key in [
        "id",
        "name",
        "series_id",
        "season",
        "episode",
        "year",
        "releaseInfo",
        "imdb_id",
    ] {
        assert_eq!(rows[0][key], body[key], "{key}");
    }
}

#[test]
fn roku_matching_context_wire_boundaries() {
    for tmdb in [json!(42), json!(42.0), json!("00042"), json!("tmdb:42")] {
        let context = matching_context(&json!({"series_id":"custom parent", "season":0, "episode":100000, "year":1870, "imdb_id":"tt12345", "tmdb_id":tmdb})).unwrap();
        assert_eq!(context["tmdb_id"], "42");
        assert_eq!(context["year"], 1870);
        assert_eq!(context["imdb_id"], "tt12345");
        assert_eq!(context["series_id"], "custom parent");
    }
    let upper = matching_context(&json!({"series_id":"x".repeat(512),"releaseInfo":"x".repeat(128),"year":2200,"imdb_id":"tt123456789012","tmdb_id":2147483647_u64})).unwrap();
    assert_eq!(upper["tmdb_id"], "2147483647");
    assert!(matching_context(&json!({"year":1869})).is_err());
    assert!(matching_context(&json!({"imdb_id":"tt1234"})).is_err());
    assert!(matching_context(&json!({"imdb_id":"tt1234567890123"})).is_err());
    assert!(matching_context(&json!({"tmdb_id":42.5})).is_err());
    assert!(matching_context(&json!({"tmdb_id":2147483648_u64})).is_err());
    assert!(matching_context(&json!({"tmdb_id":"2147483648"})).is_err());
    assert_eq!(release_year(&json!("1870–1871")), Some(json!(1870)));
}

#[test]
fn enrichment_repairs_blank_names_preserves_ids_and_prefers_valid_context() {
    for name in [Value::Null, json!(""), json!("  \t ")] {
        let mut request = json!({"id":"opaque:episode","type":"series","series_id":"custom:parent","name":name,"year":2001,"imdb_id":"tt1234567"});
        assert_eq!(
            enrichment_id(&request, "series", "opaque:episode"),
            "custom:parent"
        );
        enrich_matching(
            &mut request,
            &json!({"name":"Canonical Series","year":2020,"imdb_id":"tt7654321","tmdb_id":42}),
        );
        assert_eq!(request["name"], "Canonical Series");
        assert_eq!(request["year"], 2001);
        assert_eq!(request["imdb_id"], "tt1234567");
        assert_eq!(request["tmdb_id"], "42");
        assert_eq!(request["id"], "opaque:episode");
        assert_eq!(request["type"], "series");
    }
    let mut request = json!({"id":"tt1234567"});
    enrich_matching(
        &mut request,
        &json!({"name":"Title","year":2001,"releaseInfo":"2002","imdb_id":"malformed","tmdb_id":42}),
    );
    assert_eq!(request["year"], 2001);
    assert_eq!(request["tmdb_id"], "42");
    assert!(request["imdb_id"].is_null());
}

fn first_source_id(result: &Value) -> String {
    result["events"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|event| event["streams"].as_array().unwrap())
        .next()
        .expect("registered source")["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn pengu_fetch_context_retains_only_explicit_safe_headers_privately() {
    let app = app(Connection::open_in_memory().unwrap());
    let allowed = [
        "User-Agent",
        "Referer",
        "Origin",
        "Authorization",
        "aCcEpT",
        "Accept-Language",
        "X-Requested-With",
        "X-CSRF-Token",
    ];
    let denied = [
        "Host",
        "Connection",
        "Content-Length",
        "Transfer-Encoding",
        "Range",
        "Cookie",
        "Set-Cookie",
        "X-Other",
        "Proxy-Authorization",
    ];
    let mut headers = serde_json::Map::new();
    for key in allowed {
        headers.insert(key.into(), json!(format!("private-fixture-{key}")));
    }
    for key in denied {
        headers.insert(key.into(), json!("must-not-forward"));
    }
    let (public,error) = app.register("addon:7",vec![json!({"url":"https://fixture.invalid/private-source-token","name":"Pengu fixture","description":"1080p source","behaviorHints":{"proxyHeaders":{"request":headers}}})],"movie");
    assert!(error.is_none());
    assert_eq!(public.len(), 1);
    let entries = app.streams.lock().unwrap();
    let stored = entries.get(public[0]["id"].as_str().unwrap()).unwrap();
    assert_eq!(stored.headers.len(), allowed.len());
    for key in allowed {
        assert_eq!(
            stored.headers.get(&key.to_ascii_lowercase()),
            Some(&format!("private-fixture-{key}"))
        );
    }
    for key in denied {
        assert!(!stored.headers.contains_key(&key.to_ascii_lowercase()));
    }
    let serialized = serde_json::to_string(&public).unwrap();
    assert!(!serialized.contains("private-fixture"));
    assert!(!serialized.contains("private-source-token"));
    assert!(!serialized.contains("must-not-forward"));
    assert_eq!(public[0].as_object().unwrap().len(), 9);
    assert!(public[0]["source_name"].is_string());
    assert_eq!(public[0]["description"], "1080p source");
    assert_eq!(public[0]["audio_language_status"], "unknown");
    assert_eq!(public[0]["title"], "1080p source");
}

#[tokio::test]
async fn ordinary_header_values_do_not_mutilate_language_or_display_metadata() {
    let a = app(Connection::open_in_memory().unwrap());
    let (cards,error)=a.register("addon:7",vec![json!({
        "url":"https://fixture.invalid/media-secret",
        "name":"French Player client", "title":"English en eng French",
        "description":"French en eng Player client application/json credential-secret csrf-secret https://origin.invalid/private https://referer.invalid/private",
        "languages":["en","eng","French"],
        "behaviorHints":{"proxyHeaders":{"request":{
            "Accept-Language":"en","Accept":"application/json","User-Agent":"Player","X-Requested-With":"client",
            "Authorization":"Bearer credential-secret","X-CSRF-Token":"csrf-secret",
            "Origin":"https://origin.invalid/private","Referer":"https://referer.invalid/private"
        }}}
    })],"movie");
    assert!(error.is_none());
    let card = &cards[0];
    assert_eq!(card["reported_languages"], json!(["en", "eng", "French"]));
    assert_eq!(card["name"], "French Player client");
    assert_eq!(card["title"], "English en eng French");
    assert!(card["description"]
        .as_str()
        .unwrap()
        .starts_with("French en eng Player client application/json"));
    let serialized = card.to_string();
    for secret in [
        "credential-secret",
        "csrf-secret",
        "https://origin.invalid/private",
        "https://referer.invalid/private",
        "media-secret",
    ] {
        assert!(!serialized.contains(secret));
    }
}

#[tokio::test]
async fn source_cards_preserve_bounded_reports_without_promoting_or_leaking_them() {
    let app = app(Connection::open_in_memory().unwrap());
    let url = "https://fixture.invalid/private-stream-token";
    let (public,error)=app.register("addon:7",vec![json!({
        "url":url,"name":"[TB+] Torrentio","title":"A release title",
        "description":format!("1080p H264\nAAC stereo https://other.invalid/private-url {} Bearer private-auth-value",url),
        "languages":["English","Italian","English"],
        "subtitles":[{"lang":"eng","url":"https://subtitle.invalid/private-subtitle-token"}],
        "behaviorHints":{"filename":"/private/path/Movie.1080p.mkv?private-query=yes","videoSize":123456789,
            "proxyHeaders":{"request":{"Authorization":"Bearer private-auth-value"}}}
    })],"movie");
    assert!(error.is_none());
    let card = &public[0];
    assert_eq!(card["name"], "[TB+] Torrentio");
    assert_eq!(card["title"], "A release title");
    assert!(card["description"]
        .as_str()
        .unwrap()
        .contains("1080p H264\nAAC stereo"));
    assert_eq!(card["filename"], "Movie.1080p.mkv");
    assert_eq!(card["size_bytes"], 123456789);
    assert_eq!(card["reported_languages"], json!(["English", "Italian"]));
    assert_eq!(card["audio_language_status"], "unverified");
    let serialized = card.to_string();
    for private in [
        url,
        "private-url",
        "private-auth-value",
        "private-subtitle-token",
        "private-query",
        "/private/path",
        "behaviorHints",
        "proxyHeaders",
    ] {
        assert!(
            !serialized.contains(private),
            "Leaked private metadata category"
        );
    }
    let (public,_) = app.register("addon:7",vec![json!({"url":url,"name":"€".repeat(1000),"title":"€".repeat(1000),"description":"€".repeat(2000),"behaviorHints":{"filename":"€".repeat(1000),"videoSize":(1_u64<<50)+1}})],"movie");
    for (key, limit) in [
        ("name", 256),
        ("title", 1024),
        ("description", 2048),
        ("filename", 512),
    ] {
        assert!(public[0][key].as_str().unwrap().len() <= limit);
    }
    assert!(public[0].get("size_bytes").is_none());
    assert!(public[0].get("reported_languages").is_none());
    assert_eq!(public[0]["audio_language_status"], "unknown");
}

#[tokio::test]
async fn registered_headers_and_display_fallback_are_bounded() {
    let app = app(Connection::open_in_memory().unwrap());
    for (value, retained) in [
        (json!("x".repeat(4096)), true),
        (json!("x".repeat(4097)), false),
        (json!("bad\r\nInjected: yes"), false),
        (json!("bad\tvalue"), false),
        (json!("bad\u{0001}value"), false),
        (json!("bad\u{0085}value"), false),
        (json!(42), false),
    ] {
        let (public,_) = app.register("addon:7",vec![json!({"url":"https://fixture.invalid/source","behaviorHints":{"proxyHeaders":{"request":{"Accept":value,"Accept-Language":value,"X-Requested-With":value,"X-CSRF-Token":value}}}})],"movie");
        let entries = app.streams.lock().unwrap();
        let headers = &entries
            .get(public[0]["id"].as_str().unwrap())
            .unwrap()
            .headers;
        assert_eq!(headers.len(), if retained { 4 } else { 0 });
    }
    for (title, description, expected) in [
        (
            json!("Preferred title"),
            json!("Description"),
            "Preferred title",
        ),
        (json!("  "), json!(" Description "), "Description"),
        (Value::Null, json!("Only description"), "Only description"),
        (json!(42), json!("Description"), "Description"),
        (Value::Null, json!(" \t "), "HTTP stream"),
        (Value::Null, Value::Null, "HTTP stream"),
    ] {
        let (public,_) = app.register("addon:7",vec![json!({"url":"https://fixture.invalid/source","title":title,"description":description})],"movie");
        assert_eq!(public[0]["title"], expected);
    }
    let (public, _) = app.register(
        "addon:7",
        vec![json!({"url":"https://fixture.invalid/source","description":"€".repeat(1000)})],
        "movie",
    );
    let title = public[0]["title"].as_str().unwrap();
    assert_eq!(title.len(), 1023);
    assert_eq!(title, "€".repeat(341));
}

#[tokio::test]
async fn cached_source_ids_recheck_exact_content_scope_before_probe() {
    let app = app(Connection::open_in_memory().unwrap());
    indexed_series(&app);
    app.db.lock().unwrap().execute_batch("INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES('iptv:1:11',1,'11','Fixture Live');
        INSERT INTO provider_vod(id,provider_id,stream_id,kind,name,normalized,year,extension) VALUES('fixture-movie',1,'22','movie','Fixture Movie','fixture movie',2001,'mp4');").unwrap();
    let live = first_source_id(&discover(&app, json!({"type":"live","id":"iptv:1:11"})).await);
    let movie = first_source_id(
        &discover(
            &app,
            json!({"type":"movie","id":"opaque-movie","name":"Fixture Movie","year":2001}),
        )
        .await,
    );
    let series = first_source_id(&discover(&app,json!({"type":"series","id":"opaque-episode","name":"Canonical Series","year":2001,"season":2,"episode":4})).await);
    let cases = [
        ("live", "enable_live", &live, &movie),
        ("movie", "enable_movies", &movie, &series),
        ("series", "enable_series", &series, &live),
    ];
    for (kind, scope, source, other) in cases {
        assert_eq!(
            api(&app, "PATCH", "/api/providers/1", json!({scope:false}))
                .await
                .0,
            StatusCode::OK
        );
        let (status, error) = api(&app, "POST", "/api/playback", json!({"stream_id":source})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{kind}: {error}");
        assert_eq!(error["error"], "Provider not found or disabled", "{kind}");
        // Missing probe executable proves an eligible scope reaches probing without network.
        let (_, allowed) = api(&app, "POST", "/api/playback", json!({"stream_id":other})).await;
        assert_eq!(
            allowed["error"], "Could not inspect source video safely; try another stream",
            "{kind}: {allowed}"
        );
        if kind == "live" {
            let (_, direct) = api(
                &app,
                "POST",
                "/api/playback",
                json!({"channel_id":"iptv:1:11"}),
            )
            .await;
            assert_ne!(direct["error"], allowed["error"]);
        }
        assert_eq!(
            api(&app, "PATCH", "/api/providers/1", json!({scope:true}))
                .await
                .0,
            StatusCode::OK
        );
        let (_, reenabled) = api(&app, "POST", "/api/playback", json!({"stream_id":source})).await;
        assert_eq!(
            reenabled["error"],
            "Could not inspect source video safely; try another stream"
        );
    }
}

#[tokio::test]
async fn upstream_type_and_source_cannot_override_job_scope_or_addon_ownership() {
    let app = app(Connection::open_in_memory().unwrap());
    indexed_series(&app);
    for (kind, scope, spoofed) in [
        ("live", "enable_live", "movie"),
        ("movie", "enable_movies", "series"),
        ("series", "enable_series", "live"),
    ] {
        let job = Job {
            kind: kind.into(),
            created: Instant::now(),
            state: Mutex::new(JobState {
                events: vec![],
                pending: 1,
            }),
            notify: Notify::new(),
        };
        emit(
            &app,
            &job,
            "iptv:1",
            Ok(vec![
                json!({"url":"https://provider.invalid/media.mp4","type":spoofed,"live":kind != "live","source":"addon:1"}),
            ]),
        );
        let id = job.state.lock().unwrap().events[0]["streams"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        {
            let registry = app.streams.lock().unwrap();
            let source = registry.get(&id).unwrap();
            assert_eq!(source.kind, kind);
            assert_eq!(source.live, kind == "live");
            assert_eq!(source.provider_id, Some(1));
        }
        assert_eq!(
            api(&app, "PATCH", "/api/providers/1", json!({scope:false}))
                .await
                .0,
            StatusCode::OK
        );
        let (_, error) = api(&app, "POST", "/api/playback", json!({"stream_id":id})).await;
        assert_eq!(error["error"], "Provider not found or disabled");
        assert_eq!(
            api(&app, "PATCH", "/api/providers/1", json!({scope:true}))
                .await
                .0,
            StatusCode::OK
        );
    }
    let (sources, _) = app.register(
        "addon:1",
        vec![json!({"url":"https://addon.invalid/media.mp4","source":"iptv:1","type":"live"})],
        "movie",
    );
    assert_eq!(
        api(
            &app,
            "PATCH",
            "/api/providers/1",
            json!({"enable_movies":false})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, error) = api(
        &app,
        "POST",
        "/api/playback",
        json!({"stream_id":sources[0]["id"]}),
    )
    .await;
    assert_eq!(
        error["error"],
        "Could not inspect source video safely; try another stream"
    );
}

#[tokio::test]
async fn enriched_numeric_year_and_alternate_id_match_through_api() {
    let app = app(Connection::open_in_memory().unwrap());
    app.db.lock().unwrap().execute_batch("INSERT INTO providers(id,name,url,username,password) VALUES(1,'Fixture','https://provider.invalid','fixture','fixture'); INSERT INTO provider_vod(id,provider_id,stream_id,kind,name,normalized,year,tmdb_id,extension) VALUES('movie',1,'22','movie','Localized Title','localized title',2001,'tmdb:42','mp4');").unwrap();
    let mut request = json!({"type":"movie","id":"tt1234567","name":""});
    enrich_matching(
        &mut request,
        &json!({"name":"Original Title","year":2001,"tmdb_id":42}),
    );
    let result = discover(&app, request).await;
    assert!(
        result["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| !event["streams"].as_array().unwrap().is_empty()),
        "{result}"
    );
}

#[test]
fn history_never_groups_unrelated_opaque_addon_ids() {
    assert_eq!(legacy_series_id("tt123:2:1"), "tt123");
    assert_eq!(legacy_series_id("addon:one:episode"), "addon:one:episode");
}

#[tokio::test]
async fn source_filter_labels_use_configured_producer_not_upstream_name() {
    let app = app(Connection::open_in_memory().unwrap());
    indexed_series(&app);
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE providers SET name='Family IPTV' WHERE id=1", [])
        .unwrap();
    app.db.lock().unwrap().execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id) VALUES(99,'Cinema Addon','https://example.invalid/manifest.json','{}',0)", []).unwrap();
    for (source, name) in [("iptv:1", "Family IPTV"), ("addon:99", "Cinema Addon")] {
        let (cards, _) = app.register(source, vec![json!({"url":"https://example.invalid/video.mp4","name":"Release label 2160p","source_name":"Spoofed producer"})], "movie");
        assert_eq!(cards[0]["source"], source);
        assert_eq!(cards[0]["source_name"], name);
        assert_eq!(cards[0]["name"], "Release label 2160p");
    }
}

#[tokio::test]
async fn source_resume_identity_survives_new_jobs_and_rotating_urls() {
    let app = app(Connection::open_in_memory().unwrap());
    let source = |url: &str, file: &str| json!({"url":url,"name":"Provider 1080p","title":"English dub","behaviorHints":{"filename":file}});
    let (a, _) = app.register(
        "addon:7",
        vec![source("https://fixture.invalid/a?token=one", "Episode.mkv")],
        "series",
    );
    let (b, _) = app.register(
        "addon:7",
        vec![source("https://fixture.invalid/a?token=two", "Episode.mkv")],
        "series",
    );
    let (c, _) = app.register(
        "addon:7",
        vec![source("https://fixture.invalid/b", "Different.mkv")],
        "series",
    );
    assert_eq!(a[0]["source_addon_id"], "addon:7");
    assert_ne!(a[0]["id"], b[0]["id"]);
    assert_eq!(a[0]["source_fingerprint"], b[0]["source_fingerprint"]);
    assert_ne!(a[0]["source_fingerprint"], c[0]["source_fingerprint"]);
    assert_eq!(a[0]["source_fingerprint"].as_str().unwrap().len(), 64);
}

#[tokio::test]
async fn viewing_queue_keeps_completed_series_hides_without_erasing_and_pages_titles() {
    let app = app(Connection::open_in_memory().unwrap());
    let episode = json!({"id":"opaque-one","type":"series","series_id":"show","name":"Show","season":1,"episode":1,"position":1000,"duration":1000,"source_addon_id":"iptv:1","source_name":"Provider"});
    assert_eq!(
        api(&app, "PUT", "/api/profiles/1/progress", episode.clone())
            .await
            .0,
        StatusCode::OK
    );
    let (_, page) = api(&app, "GET", "/api/profiles/1/continue/page", json!(null)).await;
    assert_eq!(page["items"][0]["queue_status"], "pending");
    let mut hidden = episode.clone();
    hidden["hidden"] = json!(true);
    assert_eq!(
        api(
            &app,
            "PUT",
            "/api/profiles/1/continue/visibility",
            hidden.clone()
        )
        .await
        .0,
        StatusCode::OK
    );
    api(&app, "PUT", "/api/profiles/1/progress", episode.clone()).await;
    assert_eq!(
        api(&app, "GET", "/api/profiles/1/continue/page", json!(null))
            .await
            .1["items"],
        json!([])
    );
    assert_eq!(
        api(&app, "GET", "/api/profiles/1/progress", json!(null))
            .await
            .1
            .as_array()
            .unwrap()
            .len(),
        1
    );
    hidden["hidden"] = json!(false);
    api(&app, "PUT", "/api/profiles/1/continue/visibility", hidden).await;
    {
        let db = app.db.lock().unwrap();
        for n in 0..510 {
            db.execute("INSERT INTO progress(profile_id,id,type,name,position,duration,updated_at,context,title_id) VALUES(1,?1,'series','Show',5,100,?2,'{\"series_id\":\"show\"}','show')",params![format!("older-{n}"),n]).unwrap();
        }
        for n in 0..45 {
            db.execute("INSERT INTO progress(profile_id,id,type,name,position,duration,updated_at,context,title_id) VALUES(1,?1,'movie','Film',5,100,?2,'{}',?1)",params![format!("film-{n}"),n]).unwrap();
        }
    }
    let (_, one) = api(
        &app,
        "GET",
        "/api/profiles/1/continue/page?limit=20",
        json!(null),
    )
    .await;
    let (_, two) = api(
        &app,
        "GET",
        "/api/profiles/1/continue/page?limit=20&offset=20",
        json!(null),
    )
    .await;
    let (_, three) = api(
        &app,
        "GET",
        "/api/profiles/1/continue/page?limit=20&offset=40",
        json!(null),
    )
    .await;
    assert_eq!(one["total"], 46);
    assert_eq!(one["next_offset"], 20);
    assert_eq!(two["next_offset"], 40);
    assert!(three["next_offset"].is_null());
    let ids: std::collections::HashSet<_> = [one, two, three]
        .iter()
        .flat_map(|p| p["items"].as_array().unwrap())
        .map(|v| v["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids.len(), 46);
    assert_eq!(
        api(
            &app,
            "GET",
            "/api/profiles/1/continue/settings",
            json!(null)
        )
        .await
        .1["autoplay"],
        true
    );
    assert_eq!(
        api(
            &app,
            "PUT",
            "/api/profiles/1/continue/settings",
            json!({"autoplay":false})
        )
        .await
        .1["autoplay"],
        false
    );
    assert_eq!(
        api(&app, "GET", "/api/profiles/999/continue/page", json!(null))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}

#[test]
fn next_episode_preserves_actual_identity_and_rejects_ambiguous_or_future_targets() {
    let current = json!({"type":"series","id":"opaque-A","series_id":"show","name":"Show","season":1,"episode":12,"source_addon_id":"iptv:2"});
    let meta = json!({"videos":[{"id":"special","season":0,"episode":1},{"id":"opaque-B","season":2,"episode":1},{"id":"opaque-A","season":1,"episode":12}]});
    let next = continuation::resolve(&current, &meta, util::now());
    assert_eq!(next["item"]["id"], "opaque-B");
    assert_eq!(next["item"]["season"], 2);
    assert_eq!(next["item"]["source_addon_id"], "iptv:2");
    assert_eq!(next["item"]["position"], 0);
    let mut duplicate = meta.clone();
    duplicate["videos"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"different","season":2,"episode":1}));
    assert_eq!(
        continuation::resolve(&current, &duplicate, util::now())["status"],
        "unavailable"
    );
    let mut future = meta.clone();
    future["videos"][1]["released"] = json!("2199-01-01T00:00:00Z");
    assert_eq!(
        continuation::resolve(&current, &future, util::now())["status"],
        "upcoming"
    );
    assert_eq!(
        continuation::resolve(&next["item"], &meta, util::now())["status"],
        "caught_up"
    );
    assert_eq!(
        continuation::resolve(&current, &json!({}), util::now())["status"],
        "unavailable"
    );
}

#[tokio::test]
async fn release_family_and_iptv_identity_survive_episode_changes() {
    assert_eq!(
        continuation::release_group("Re.Zero.S04E01.1080p.Dual.Group.mkv"),
        continuation::release_group("Re.Zero.S04E02.1080p.Dual.Group.mkv")
    );
    assert_ne!(
        continuation::release_group("Show.S01E01.1080p.A.mkv"),
        continuation::release_group("Show.S01E02.720p.B.mkv")
    );
    assert!(continuation::release_group("ambiguous-123.mkv").is_none());
    let app = app(Connection::open_in_memory().unwrap());
    let (streams, _) = app.register(
        "iptv:9",
        vec![json!({"url":"https://fixture.invalid/episode","name":"Provider S2E1"})],
        "series",
    );
    assert_eq!(streams[0]["source_addon_id"], "iptv:9");
    assert_eq!(streams[0]["source_fingerprint"].as_str().unwrap().len(), 64);
}

#[tokio::test]
async fn next_api_reads_metadata_envelope_then_discovers_actual_iptv_episode() {
    let app = app(Connection::open_in_memory().unwrap());
    indexed_series(&app);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream = tokio::spawn(async move {
        axum::serve(listener,Router::new().fallback(||async {axum::Json(json!({"meta":{"id":"canonical","type":"series","videos":[{"id":"opaque-before","season":2,"episode":3},{"id":"opaque-next","season":2,"episode":4}]}}))})).await.unwrap();
    });
    app.db.lock().unwrap().execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id) VALUES(99,'Fixture',?1,?2,1)",params![format!("http://{address}/manifest.json"),json!({"resources":["meta"],"types":["series"]}).to_string()]).unwrap();
    let (status,next)=api(&app,"POST","/api/profiles/1/continue/next",json!({"type":"series","id":"opaque-before","series_id":"canonical","name":"Canonical Series","season":2,"episode":3,"year":2001,"source_addon_id":"iptv:1"})).await;
    assert_eq!(status, StatusCode::OK, "{next}");
    assert_eq!(next["item"]["id"], "opaque-next", "{next}");
    let discovery = discover(&app, next["item"].clone()).await;
    let sources: Vec<_> = discovery["events"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|v| v["streams"].as_array().unwrap())
        .collect();
    assert_eq!(sources.len(), 1, "{discovery}");
    assert_eq!(sources[0]["source_addon_id"], "iptv:1");
    let streams = app.streams.lock().unwrap();
    let actual = &streams[sources[0]["id"].as_str().unwrap()];
    assert!(actual.url.ends_with("/44.mp4"));
    upstream.abort();
}

#[tokio::test]
async fn next_queue_preserves_resume_only_in_final_seconds() {
    let app = app(Connection::open_in_memory().unwrap());
    let mut episode = json!({"id":"e1","type":"series","series_id":"show","name":"Show","season":1,"episode":1,"position":970,"duration":1000,"source_addon_id":"iptv:1","source_name":"Provider","source_fingerprint":"saved"});
    let next = json!({"status":"next","item":{"id":"e2","type":"series","series_id":"show","name":"Show","season":1,"episode":2,"position":0,"duration":0,"queue_status":"next"}});
    app.db.lock().unwrap().execute("INSERT INTO continuation_cache(profile_id,series_id,from_id,result,updated_at) VALUES(1,'show','e1',?1,?2)",params![next.to_string(),util::now()]).unwrap();
    api(&app, "PUT", "/api/profiles/1/progress", episode.clone()).await;
    let (_, page) = api(&app, "GET", "/api/profiles/1/continue/page", json!(null)).await;
    assert_eq!(page["items"][0]["id"], "e1");
    episode["position"] = json!(995);
    api(&app, "PUT", "/api/profiles/1/progress", episode).await;
    let (_, page) = api(&app, "GET", "/api/profiles/1/continue/page", json!(null)).await;
    assert_eq!(page["items"][0]["id"], "e2");
    assert_eq!(page["items"][0]["previous_episode"]["id"], "e1");
    assert_eq!(page["items"][0]["previous_episode"]["position"], 995.0);
    assert_eq!(
        page["items"][0]["previous_episode"]["source_fingerprint"],
        "saved"
    );
}

#[tokio::test]
async fn continuation_discovery_scopes_skip_unrelated_producers() {
    let app = app(Connection::open_in_memory().unwrap());
    indexed_series(&app);
    {
        let db = app.db.lock().unwrap();
        db.execute_batch("INSERT INTO providers(id,name,url,username,password) VALUES(2,'Other','https://other.invalid','fixture','fixture'); INSERT INTO provider_vod(id,provider_id,stream_id,kind,name,normalized,year,extension) VALUES('other',2,'33','series','Canonical Series','canonical series',2001,'mp4');").unwrap();
        db.execute("INSERT INTO provider_cache(provider_id,cache_key,expires_at,payload) SELECT 2,cache_key,expires_at,payload FROM provider_cache WHERE provider_id=1",[]).unwrap();
    }
    let request = json!({"id":"opaque","type":"series","name":"Canonical Series","season":2,"episode":4,"year":2001,"only_provider_id":1});
    let result = discover(&app, request.clone()).await;
    let streams: Vec<_> = result["events"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|e| e["streams"].as_array().unwrap())
        .collect();
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0]["source_addon_id"], "iptv:1");
    let mut addons = request.clone();
    addons.as_object_mut().unwrap().remove("only_provider_id");
    addons["only_addons"] = json!(true);
    let result = discover(&app, addons).await;
    assert_eq!(result["done"], true);
    assert!(result["events"].as_array().unwrap().is_empty());
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE providers SET enabled=0 WHERE id=1", [])
        .unwrap();
    let result = discover(&app, request.clone()).await;
    assert!(result["events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["streams"].as_array().unwrap().is_empty()));
    for fields in [
        json!({"only_provider_id":0}),
        json!({"only_addons":"yes"}),
        json!({"only_provider_id":1,"only_addons":true}),
    ] {
        let mut invalid = request.clone();
        invalid
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        assert_eq!(
            api(&app, "POST", "/api/streams", invalid).await.0,
            StatusCode::BAD_REQUEST
        );
    }
}
