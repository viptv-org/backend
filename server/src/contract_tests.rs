//! Entirely in-process contract tests: no listeners, provider requests or temp files.
use super::*;
use axum::body::to_bytes;

/// Retired embedded-engine diagnostics are no longer classified: they pass
/// through as plain 400s without a recovery code, so no client can be steered by
/// a message the current runtime never produces.
#[tokio::test]
async fn retired_engine_messages_are_not_classified() {
    for message in [
        "Stream expired; discover again",
        "Playback capacity reached",
        "Playback engine unavailable",
        "Playback could not start; try forced transcoding or another stream",
        "Could not inspect source video safely; try another stream",
        "Media origin HTTP 404",
        "Media origin unavailable",
        "Cross-origin proxy media requires managed playback",
        "All available connections are busy. Try this channel again shortly.",
    ] {
        let error = ApiError::from(message);
        assert_eq!(error.api_error_code(), None, "{message}");
        let response = error.into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{message}");
        let bytes = to_bytes(response.into_body(), 4096).await.unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value, json!({"error":message}));
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
    crate::test_support::configure_vault(&mut app);
    app.addons.allow_test_loopback = true;
    app
}
async fn api(app: &App, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    crate::test_support::request(app, ACCOUNT_TOKEN, method, path, body).await
}

fn indexed_series(app: &App) {
    let db = app.db.lock().unwrap();
    db.execute_batch("INSERT INTO providers(id,name,url,username,password) VALUES(1,'Fixture','https://provider.invalid','fixture','fixture');
        INSERT INTO provider_vod(id,provider_id,stream_id,kind,name,normalized,year,extension) VALUES('candidate',1,'33','series','Canonical Series','canonical series',2001,'mp4');").unwrap();
    db.execute("INSERT INTO provider_cache(provider_id,cache_key,expires_at,payload) VALUES(1,'get_series_info:33',?1,?2)",params![util::now()+3600,json!({"episodes":{"2":[{"id":44,"episode_num":4,"container_extension":"mp4"}]}}).to_string()]).unwrap();
}
async fn discover(app: &App, body: Value) -> Value {
    // Explicit synthetic fixture ownership; never production migration inference.
    app.db.lock().unwrap().execute("INSERT OR IGNORE INTO provider_ownership(provider_id,account_id) SELECT id,1 FROM providers",[]).unwrap();
    crate::test_support::encrypt_fixture_sources(app);
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
    let record = json!({
        "identity": {
            "id": "addon-opaque-episode",
            "kind": "series",
            "ids": [{"namespace": "imdb", "value": "tt1234567"}, {"namespace": "tmdb", "value": "42"}],
            "episode": {"order": "aired", "season": 2, "episode": 4},
            "edition": null
        },
        "provider_id": "fixture",
        "title": "Canonical Series",
        "poster": null,
        "landscape": null,
        "position_millis": 540_000,
        "duration_millis": 1_800_000,
        "updated_at_millis": 0,
        "completed_at_millis": null,
        "parent_identity": {
            "id": "custom:parent",
            "kind": "series",
            "ids": [],
            "episode": null,
            "edition": null
        }
    });
    assert_eq!(
        api(
            &app,
            "PUT",
            "/api/profiles/1/progress",
            json!({"record": record, "ended": false})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, rows) = api(&app, "GET", "/api/profiles/1/progress/page", Value::Null).await;
    let row = rows["items"][0].clone();
    assert_eq!(row["id"], "addon-opaque-episode");
    assert_eq!(row["name"], "Canonical Series");
    assert_eq!(row["series_id"], "custom:parent");
    assert_eq!(row["season"], 2);
    assert_eq!(row["episode"], 4);
    let result = discover(
        &app,
        json!({"type":"series","id":"addon-opaque-episode","series_id":"custom:parent","name":"Canonical Series","season":2,"episode":4,"year":2001,"imdb_id":"tt1234567","source_addon_id":"iptv:1"}),
    )
    .await;
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
    let mut resumed = record.clone();
    resumed["position_millis"] = json!(600_000);
    assert_eq!(
        api(
            &app,
            "PUT",
            "/api/profiles/1/progress",
            json!({"record": resumed, "ended": false})
        )
        .await
        .0,
        StatusCode::OK
    );
    let rows = api(&app, "GET", "/api/profiles/1/progress/page", Value::Null)
        .await
        .1;
    assert_eq!(rows["items"][0]["season"], 2);
    assert_eq!(rows["items"][0]["position"], 600.0);
}

#[tokio::test]
async fn migration_and_old_client_are_idempotent() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE progress(profile_id INTEGER NOT NULL,id TEXT NOT NULL,type TEXT NOT NULL,name TEXT NOT NULL,poster TEXT,position REAL NOT NULL,duration REAL NOT NULL,updated_at INTEGER NOT NULL,PRIMARY KEY(profile_id,type,id)); INSERT INTO progress VALUES(1,'old','movie','Old',NULL,10,100,1);").unwrap();
    let app = app(db);
    init_progress_context(&app.db.lock().unwrap()).unwrap();
    init_progress_context(&app.db.lock().unwrap()).unwrap();
    let rows = api(&app, "GET", "/api/profiles/1/progress/page", Value::Null)
        .await
        .1;
    assert_eq!(rows["items"][0]["id"], "old");
    assert_eq!(
        api(
            &app,
            "PUT",
            "/api/profiles/1/progress",
            json!({"record":{"identity":{"id":"old","kind":"movie","ids":[],"episode":null,"edition":null},"provider_id":"fixture","title":"Old","poster":null,"landscape":null,"position_millis":20_000,"duration_millis":100_000,"updated_at_millis":0,"completed_at_millis":null},"ended":false})
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn invalid_optional_context_rejected_before_save_or_job() {
    let app = app(Connection::open_in_memory().unwrap());
    // The typed record rejects invalid optional context (oversized ids,
    // control characters, out-of-range coordinates) before any row is saved
    // or a discovery job starts.
    for (key, value) in [
        ("id", json!("x".repeat(513))),
        ("id", json!("parent\u{0001}id")),
        ("title", json!("x".repeat(1025))),
        ("episode", json!({"order":"aired","season":-1,"episode":2})),
        ("episode", json!({"order":"aired","season":1,"episode":1.5})),
        ("ids", json!([{"namespace":"imdb","value":"tt1"}])),
        ("ids", json!([{"namespace":"imdb","value":"ttbad"}])),
        ("position_millis", json!(-1)),
        ("duration_millis", json!(-1)),
    ] {
        let mut record = json!({
            "identity": {"id":"opaque","kind":"series","ids":[],"episode":{"order":"aired","season":1,"episode":2},"edition":null},
            "provider_id":"fixture",
            "title":"Title",
            "poster":null,
            "landscape":null,
            "position_millis":1_000,
            "duration_millis":100_000,
            "updated_at_millis":0,
            "completed_at_millis":null
        });
        match key {
            "id" => record["identity"]["id"] = value.clone(),
            "title" => record["title"] = value.clone(),
            "episode" => record["identity"]["episode"] = value.clone(),
            "ids" => record["identity"]["ids"] = value.clone(),
            _ => record[key] = value.clone(),
        }
        // Malformed shapes are rejected at deserialization (422) while
        // semantic violations are rejected by validation (400); either way
        // nothing is saved and no discovery job starts.
        let status = api(
            &app,
            "PUT",
            "/api/profiles/1/progress",
            json!({"record": record, "ended": false}),
        )
        .await
        .0;
        assert!(
            status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY,
            "{key}: {status}"
        );
    }
    assert!(app.jobs.lock().unwrap().is_empty());
    let (_, page) = api(&app, "GET", "/api/profiles/1/progress/page", Value::Null)
        .await;
    assert_eq!(page["items"], json!([]));
}

#[tokio::test]
async fn final_roku_wire_get_accepts_parent_512_and_emits_decimal_tmdb() {
    let app = app(Connection::open_in_memory().unwrap());
    let parent = format!("opaque parent {}", "x".repeat(498));
    assert_eq!(parent.len(), 512);
    let body = json!({"record":{
        "identity": {"id":"original opaque episode","kind":"series","ids":[{"namespace":"imdb","value":"tt12345"},{"namespace":"tmdb","value":"2147483647"}],"episode":{"order":"aired","season":0,"episode":100000},"edition":null},
        "provider_id":"fixture",
        "title":"Canonical Name",
        "poster":null,
        "landscape":null,
        "position_millis":42_000,
        "duration_millis":100_000,
        "updated_at_millis":0,
        "completed_at_millis":null,
        "parent_identity": {"id":parent,"kind":"series","ids":[],"episode":null,"edition":null}
    },"ended":false});
    assert_eq!(
        api(&app, "PUT", "/api/profiles/1/progress", body.clone())
            .await
            .0,
        StatusCode::OK
    );
    let rows = api(&app, "GET", "/api/profiles/1/progress/page", Value::Null)
        .await
        .1;
    let row = rows["items"][0].clone();
    assert_eq!(row["id"], "original opaque episode");
    assert_eq!(row["name"], "Canonical Name");
    assert_eq!(row["series_id"], parent);
    assert_eq!(row["season"], 0);
    assert_eq!(row["episode"], 100000);
    assert_eq!(row["position"], 42.0);
    // External ids stay in the typed identity; they are no longer
    // duplicated onto the legacy row projection.
    assert!(row.get("tmdb_id").is_none() || row["tmdb_id"].is_null());
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

#[tokio::test]
async fn pengu_fetch_context_retains_only_explicit_safe_headers_privately() {
    let app = app(Connection::open_in_memory().unwrap());
    let allowed = [
        "User-Agent",
        "Referer",
        "Origin",
        "Authorization",
        "Cookie",
        "aCcEpT",
        "Accept-Language",
        "X-Requested-With",
        "X-CSRF-Token",
        "X-API-Key",
    ];
    let denied = [
        "Host",
        "Connection",
        "Content-Length",
        "Transfer-Encoding",
        "Range",
        "Set-Cookie",
        "X-Other",
        "Proxy-Authorization",
    ];
    let mut headers = serde_json::Map::new();
    for key in allowed {
        headers.insert(key.into(), json!(format!("private-fixture-{key}")));
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
    let public_fields = [
        "id",
        "source",
        "name",
        "title",
        "description",
        "audio_language_status",
        "presentation",
        "source_addon_id",
        "source_fingerprint",
        "source_name",
        "quality_height",
        "audio_languages",
        "detail",
    ];
    assert!(public[0]
        .as_object()
        .unwrap()
        .keys()
        .all(|key| public_fields.contains(&key.as_str())));
    assert_eq!(public[0]["quality_height"], 1080);
    assert_eq!(public[0]["audio_languages"], json!([]));
    assert_eq!(public[0]["detail"], "1080p source");
    assert_eq!(
        public[0]["presentation"]["provider_name"],
        public[0]["source_name"]
    );
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
        let (public,error) = app.register("addon:7",vec![json!({"url":"https://fixture.invalid/source","behaviorHints":{"proxyHeaders":{"request":{"Accept":value,"Accept-Language":value,"X-Requested-With":value,"X-CSRF-Token":value}}}})],"movie");
        if !retained {
            assert!(public.is_empty());
            assert_eq!(error.as_deref(), Some("source_headers_unsupported"));
            continue;
        }
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
async fn cookie_and_api_key_reflections_are_redacted_from_every_card_field() {
    let app = app(Connection::open_in_memory().unwrap());
    let cookie = "session=cookie-credential; other=\"second-credential\"";
    let api_key = "fixture-api-key-secret";
    let url = "https://fixture.invalid/private-input";
    let text = format!(
        "French en eng Player {cookie} cookie-credential second-credential {api_key} {url}"
    );
    app.db.lock().unwrap().execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id) VALUES(7,?1,'https://addon.fixture.invalid/manifest.json','{}',1)",[text.clone()]).unwrap();
    crate::test_support::encrypt_fixture_sources(&app);
    let (cards,error) = app.register("addon:7",vec![json!({
        "url":url,"name":text,"title":text,"description":text,"languages":["en","eng","French"],
        "behaviorHints":{"filename":format!("prefixcookie-credentialsuffix.{api_key}.mkv"),"bingeGroup":text,"proxyHeaders":{"request":{
            "Cookie":cookie,"X-API-Key":api_key,"Accept-Language":"en","User-Agent":"Player"
        }}}
    })],"movie");
    assert!(error.is_none());
    assert_eq!(cards.len(), 1);
    let public = cards[0].to_string();
    for secret in [
        cookie,
        "cookie-credential",
        "second-credential",
        api_key,
        url,
    ] {
        assert!(!public.contains(secret));
    }
    for field in [
        "name",
        "title",
        "description",
        "source_name",
        "source_binge_group",
    ] {
        assert!(cards[0][field]
            .as_str()
            .unwrap()
            .starts_with("French en eng Player"));
    }
    assert_eq!(
        cards[0]["reported_languages"],
        json!(["en", "eng", "French"])
    );
    let entries = app.streams.lock().unwrap();
    let stored = &entries[cards[0]["id"].as_str().unwrap()];
    assert_eq!(stored.headers["cookie"], cookie);
    assert_eq!(stored.headers["x-api-key"], api_key);
    assert_eq!(stored.headers["accept-language"], "en");
    assert_eq!(stored.headers["user-agent"], "Player");
}

#[tokio::test]
async fn short_cookie_tokens_do_not_tear_language_or_client_identification_words() {
    let app = app(Connection::open_in_memory().unwrap());
    let (cards,error) = app.register("addon:7",vec![json!({"url":"https://fixture.invalid/media","name":"French eng Player client","description":"French eng Player client sid=en en", "languages":["eng","French"],"behaviorHints":{"proxyHeaders":{"request":{"Cookie":"sid=en","Accept-Language":"en","User-Agent":"Player"}}}})],"movie");
    assert!(error.is_none());
    assert_eq!(cards[0]["name"], "French eng Player client");
    assert!(cards[0]["description"]
        .as_str()
        .unwrap()
        .starts_with("French eng Player client"));
    assert!(!cards[0]["description"].as_str().unwrap().contains("sid=en"));
    assert_eq!(cards[0]["reported_languages"], json!(["eng", "French"]));
}

#[tokio::test]
async fn unsupported_required_headers_fail_safely_without_discarding_healthy_sources() {
    let app = app(Connection::open_in_memory().unwrap());
    for headers in [
        json!({"Host":"private-credential"}),
        json!({"X-Other":"private-credential"}),
        json!({"Cookie":"sid=private-credential","cookie":"second-private-credential"}),
        json!({"Authorization":"Bearer private-credential\r\nX-Injected: yes"}),
        json!({"X-API-Key":"private-credential\r\nX-Injected: yes"}),
        json!({"X-API-Key":42}),
        json!("private-credential"),
    ] {
        let before = app.streams.lock().unwrap().len();
        let (cards,error)=app.register("addon:7",vec![
            json!({"url":"https://fixture.invalid/private-input","name":"private-credential","behaviorHints":{"proxyHeaders":{"request":headers}}}),
            json!({"url":"https://fixture.invalid/healthy","name":"Healthy"}),
        ],"movie");
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0]["name"], "Healthy");
        assert_eq!(error.as_deref(), Some("source_headers_unsupported"));
        assert!(!serde_json::to_string(&cards)
            .unwrap()
            .contains("private-credential"));
        assert_eq!(app.streams.lock().unwrap().len(), before + 1);
    }
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
        .execute("INSERT INTO provider_ownership VALUES(1,1)", [])
        .unwrap();
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE providers SET name='Family IPTV' WHERE id=1", [])
        .unwrap();
    app.db.lock().unwrap().execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id) VALUES(99,'Cinema Addon','https://example.invalid/manifest.json','{}',1)", []).unwrap();
    crate::test_support::encrypt_fixture_sources(&app);
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
    let record = json!({"record":{
        "identity":{"id":"opaque-one","kind":"series","ids":[],"episode":{"order":"aired","season":1,"episode":1},"edition":null},
        "provider_id":"fixture","title":"Show","poster":null,"landscape":null,
        "position_millis":1_000_000,"duration_millis":1_000_000,"updated_at_millis":0,"completed_at_millis":null,
        "parent_identity":{"id":"show","kind":"series","ids":[],"episode":null,"edition":null}
    },"ended":false});
    assert_eq!(
        api(&app, "PUT", "/api/profiles/1/progress", record.clone())
            .await
            .0,
        StatusCode::OK
    );
    let (_, page) = api(&app, "GET", "/api/profiles/1/continue", json!(null)).await;
    assert_eq!(page["entries"][0]["status"], "Pending");
    assert_eq!(
        api(
            &app,
            "PUT",
            "/api/profiles/1/continue/visibility",
            json!({"identity":{"id":"opaque-one","kind":"series","ids":[],"episode":{"order":"aired","season":1,"episode":1},"edition":null},"parent_identity":{"id":"show","kind":"series","ids":[],"episode":null,"edition":null},"hidden":true})
        )
        .await
        .0,
        StatusCode::OK
    );
    api(&app, "PUT", "/api/profiles/1/progress", record.clone()).await;
    assert_eq!(
        api(&app, "GET", "/api/profiles/1/continue", json!(null))
            .await
            .1["entries"],
        json!([])
    );
    let (_, progress) = api(&app, "GET", "/api/profiles/1/progress/page", json!(null)).await;
    assert_eq!(progress["total"], 1);
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
        "/api/profiles/1/continue?limit=20",
        json!(null),
    )
    .await;
    assert_eq!(one["entries"].as_array().unwrap().len(), 20);
    assert_eq!(one["next_cursor"], "20");
    let (_, two) = api(
        &app,
        "GET",
        "/api/profiles/1/continue?limit=20&offset=20",
        json!(null),
    )
    .await;
    assert_eq!(two["next_cursor"], "40");
    let (_, three) = api(
        &app,
        "GET",
        "/api/profiles/1/continue?limit=20&offset=40",
        json!(null),
    )
    .await;
    assert_eq!(three["entries"].as_array().unwrap().len(), 5);
    assert!(three["next_cursor"].is_null());
    assert_eq!(three["complete"], true);
    let ids: std::collections::HashSet<_> = [one, two, three]
        .iter()
        .flat_map(|p| p["entries"].as_array().unwrap())
        .map(|v| v["card"]["identity"]["id"].as_str().unwrap().to_owned())
        .collect();
    // The hidden entry stays hidden: 45 films plus the current episode.
    assert_eq!(ids.len(), 45);
    assert_eq!(
        api(&app, "GET", "/api/profiles/999/continue", json!(null))
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
        axum::serve(listener,Router::new().fallback(||async {axum::Json(json!({"meta":{"id":"canonical","type":"series","videos":[{"id":"opaque-before","season":2,"episode":3,"released":"2020-01-01T00:00:00Z"},{"id":"opaque-next","season":2,"episode":4,"released":"2020-01-01T00:00:00Z"}]}}))})).await.unwrap();
    });
    app.db.lock().unwrap().execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id) VALUES(99,'Fixture',?1,?2,1)",params![format!("http://{address}/manifest.json"),json!({"id":"metadata","name":"Metadata","version":"1","resources":["meta"],"types":["series"]}).to_string()]).unwrap();
    crate::test_support::encrypt_fixture_sources(&app);
    let (status, next) = api(
        &app,
        "POST",
        "/api/profiles/1/continue/next",
        json!({"identity":{"id":"opaque-before","kind":"series","ids":[],"episode":{"order":"aired","season":2,"episode":3},"edition":null},"parent_identity":{"id":"canonical","kind":"series","ids":[],"episode":null,"edition":null}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{next}");
    assert_eq!(next["item"]["identity"]["id"], "opaque-next", "{next}");
    let discovery = discover(
        &app,
        json!({"type":"series","id":"opaque-next","series_id":"canonical","name":"Canonical Series","season":2,"episode":4,"year":2001,"source_addon_id":"iptv:1"}),
    )
    .await;
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
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream = tokio::spawn(async move {
        let routes = Router::new().route(
            "/meta/series/show.json",
            axum::routing::get(|| async {
                axum::Json(json!({"meta":{"id":"show","type":"series","name":"Show","videos":[{"id":"e1","season":1,"episode":1,"released":"2020-01-01T00:00:00Z"},{"id":"e2","season":1,"episode":2,"released":"2020-01-01T00:00:00Z"}]}}))
            }),
        );
        axum::serve(listener, routes).await.unwrap();
    });
    app.db.lock().unwrap().execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id) VALUES(99,'Metadata',?1,?2,1)",params![format!("http://{address}/manifest.json"),json!({"id":"metadata","name":"Metadata","version":"1","resources":["meta"],"types":["series"]}).to_string()]).unwrap();
    crate::test_support::encrypt_fixture_sources(&app);
    let record = json!({"record":{
        "identity":{"id":"e1","kind":"series","ids":[],"episode":{"order":"aired","season":1,"episode":1},"edition":null},
        "provider_id":"stremio","title":"Show","poster":null,"landscape":null,
        "position_millis":970_000,"duration_millis":1_000_000,"updated_at_millis":0,"completed_at_millis":null,
        "parent_identity":{"id":"show","kind":"series","ids":[],"episode":null,"edition":null}
    },"ended":false});
    api(&app, "PUT", "/api/profiles/1/progress", record.clone()).await;
    // Prepare the next episode through the resolve endpoint.
    let (status, next) = api(
        &app,
        "POST",
        "/api/profiles/1/continue/next",
        json!({"identity":{"id":"e1","kind":"series","ids":[],"episode":{"order":"aired","season":1,"episode":1},"edition":null},"parent_identity":{"id":"show","kind":"series","ids":[],"episode":null,"edition":null}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{next}");
    let (_, page) = api(&app, "GET", "/api/profiles/1/continue", json!(null)).await;
    assert_eq!(page["entries"][0]["record"]["identity"]["id"], "e1");
    let mut resumed = record.clone();
    resumed["record"]["position_millis"] = json!(995_000);
    api(&app, "PUT", "/api/profiles/1/progress", resumed).await;
    let (_, page) = api(&app, "GET", "/api/profiles/1/continue", json!(null)).await;
    assert_eq!(page["entries"][0]["card"]["identity"]["id"], "e2");
    assert_eq!(page["entries"][0]["previous_identity"]["id"], "e1");
    assert_eq!(page["entries"][0]["status"], "Next");
    upstream.abort();
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

#[tokio::test]
async fn source_cookies_are_kept_for_playback_and_never_published_on_cards() {
    let app = app(Connection::open_in_memory().unwrap());
    let (cards, error) = app.register(
        "addon:7",
        vec![json!({
            "url":"https://fixture.invalid/source", "name":"Cookie source",
            "behaviorHints":{"proxyHeaders":{"request":{"Cookie":"provider-cookie-secret"}}}
        })],
        "movie",
    );
    assert!(error.is_none());
    assert_eq!(cards.len(), 1);
    assert!(!cards[0].to_string().contains("provider-cookie-secret"));
    let id = cards[0]["id"].as_str().unwrap();
    assert_eq!(
        app.streams.lock().unwrap()[id]
            .headers
            .get("cookie")
            .map(String::as_str),
        Some("provider-cookie-secret")
    );
}
