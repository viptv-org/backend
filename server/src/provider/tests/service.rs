use super::*;

#[test]
fn scope_migration_defaults_and_api_validation_preserve_settings() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE providers(id INTEGER PRIMARY KEY,name TEXT NOT NULL,url TEXT NOT NULL,username TEXT NOT NULL,password TEXT NOT NULL,enabled INTEGER NOT NULL DEFAULT 1);
            INSERT INTO providers VALUES(1,'Legacy','https://example.com','u','p',1);").unwrap();
    init(&db).unwrap();
    init(&db).unwrap();
    let s = ProviderService::new(Arc::new(Mutex::new(db)), reqwest::Client::new());
    assert_eq!(s.scopes(1).unwrap(), [true, true, true]);
    assert_eq!(s.list().unwrap()[0]["enable_live"], true);
    let changed = s
        .update(1, json!({"enable_live":false,"enable_series":false}))
        .unwrap();
    assert_eq!(changed["enable_live"], false);
    assert_eq!(changed["enable_movies"], true);
    s.update(1, json!({"max_connections":2})).unwrap();
    assert_eq!(s.scopes(1).unwrap(), [false, true, false]);
    init(&s.lock().unwrap()).unwrap();
    assert_eq!(s.scopes(1).unwrap(), [false, true, false]);
    for field in ["enable_live", "enable_movies", "enable_series"] {
        for invalid in [json!("false"), json!(0), Value::Null, json!([])] {
            assert!(s.update(1, json!({field:invalid.clone()})).is_err());
            let mut add =
                json!({"name":"Bad","url":"https://example.com","username":"u","password":"p"});
            add[field] = invalid;
            assert!(s.add(add).is_err());
        }
    }
    assert_eq!(s.scopes(1).unwrap(), [false, true, false]);
    let added = s.add(json!({"name":"New","url":"https://example.com","username":"new-u","password":"p","enable_live":false})).unwrap();
    assert_eq!(added["enable_live"], false);
    assert_eq!(added["enable_movies"], true);
    assert_eq!(added["enable_series"], true);
}

#[tokio::test]
async fn stored_source_admission_rechecks_exact_kind_and_shares_permits() {
    let s = service();
    let p = add_provider(&s);
    for (kind, field) in [
        ("live", "enable_live"),
        ("movie", "enable_movies"),
        ("series", "enable_series"),
    ] {
        // A caller can hold an opaque source issued while the kind was enabled.
        let first = s.acquire_playback_for_kind(p, kind).await.unwrap();
        assert!(s.acquire_playback(p).await.is_err());
        assert!(s.acquire_playback_for_kind(p, kind).await.is_err());
        drop(first);
        s.update(p, json!({field:false})).unwrap();
        assert!(s.acquire_playback_for_kind(p, kind).await.is_err());
        let other = if kind == "movie" { "series" } else { "movie" };
        let permit = s.acquire_playback_for_kind(p, other).await.unwrap();
        drop(permit);
        s.update(p, json!({field:true})).unwrap();
        let permit = s.acquire_playback_for_kind(p, kind).await.unwrap();
        drop(permit);
    }
    for kind in ["", "movies", "Movie", "unknown"] {
        assert!(s.acquire_playback_for_kind(p, kind).await.is_err());
    }
    s.update(p, json!({"enabled":false})).unwrap();
    for kind in ["live", "movie", "series"] {
        assert!(s.acquire_playback_for_kind(p, kind).await.is_err());
    }
    s.update(p, json!({"enabled":true})).unwrap();
    let permit = s.acquire_playback_for_kind(p, "movie").await.unwrap();
    drop(permit);
    assert!(s.acquire_playback_for_kind(p + 999, "movie").await.is_err());
}

#[test]
fn scoped_admission_rechecks_after_blocking_worker_queue() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let s = service();
        let p = add_provider(&s);
        let (release, blocked) = std::sync::mpsc::channel();
        let (started, ready) = tokio::sync::oneshot::channel();
        let worker = tokio::task::spawn_blocking(move || {
            let _ = started.send(());
            let _ = blocked.recv();
        });
        ready.await.unwrap();
        let mut admission = Box::pin(s.acquire_playback_for_kind(p, "movie"));
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut admission)
                .await
                .is_err()
        );
        s.update(p, json!({"enable_movies":false})).unwrap();
        release.send(()).unwrap();
        worker.await.unwrap();
        assert!(admission.await.is_err());
        let permit = s.acquire_playback_for_kind(p, "series").await.unwrap();
        drop(permit);
    });
}

#[tokio::test]
async fn configured_connection_limits_are_shared_and_release() {
    let s = service();
    let p = add_provider(&s);
    assert_eq!(s.list().unwrap()[0]["max_connections"], 1);
    let permit = s.acquire_playback(p).await.unwrap();
    assert!(s.clone().acquire_playback(p).await.is_err());
    assert!(s.update(p, json!({"max_connections":2})).is_err());
    s.update(p, json!({"enabled":false})).unwrap();
    assert!(s.acquire_playback(p).await.is_err());
    s.update(p, json!({"enabled":true})).unwrap();
    let p2 = s.add(json!({"name":"Two","url":"https://example.com","username":"u","password":"p","max_connections":2})).unwrap()["id"].as_i64().unwrap();
    let one = s.acquire_playback(p2).await.unwrap();
    let two = s.acquire_playback(p2).await.unwrap();
    assert!(s.acquire_playback(p2).await.is_err());
    drop(permit);
    assert_eq!(
        s.update(p, json!({"max_connections":2})).unwrap()["max_connections"],
        2
    );
    let resized_one = s.acquire_playback(p).await.unwrap();
    let resized_two = s.acquire_playback(p).await.unwrap();
    assert!(s.acquire_playback(p).await.is_err());
    drop((resized_one, resized_two));
    assert!(s.update(p, json!({"enabled":"false"})).is_err());
    assert!(s.update(p, json!({"unexpected":1})).is_err());
    assert!(s.update(p, json!({})).is_err());
    drop((one, two));
    s.delete(p).unwrap();
    assert!(s.acquire_playback(p).await.is_err());
    for limit in [
        json!(0),
        json!(-1),
        json!(33),
        json!(1.5),
        json!("2"),
        Value::Null,
    ] {
        assert!(s.add(json!({"name":"Bad","url":"https://example.com","username":"u","password":"p","max_connections":limit})).is_err());
    }
}

#[test]
fn schema_is_idempotent_and_credentials_are_not_returned() {
    let s = service();
    init(&s.lock().unwrap()).unwrap();
    let id = add_provider(&s);
    let list = s.list().unwrap();
    assert_eq!(list[0]["id"], id);
    assert_eq!(list[0]["enabled"], true);
    assert!(list[0].get("password").is_none());
    assert!(!list.to_string().contains("SUPER_SECRET"));
    assert_eq!(s.list().unwrap()[0]["url"], "https://example.com/base");
}
#[test]
fn validates_provider_fields_and_urls_without_echoing_secrets() {
    let s = service();
    for url in [
        "file:///tmp/test",
        "https://user:SUPER_SECRET@example.com",
        "https://example.com/?password=SUPER_SECRET",
        "https://example.com/#SUPER_SECRET",
    ] {
        let error = s
            .add(json!({"name":"n","url":url,"username":"u","password":"p"}))
            .unwrap_err();
        assert!(!error.contains("SUPER_SECRET"));
    }
    assert!(s
        .add(json!({"name":"","url":"https://example.com","username":"u","password":"p"}))
        .is_err());
    assert!(s
        .add(json!({"name":"n","url":"https://example.com","username":"u"}))
        .is_err());
}

#[test]
fn live_filter_escapes_wildcards_and_paginates() {
    let s = service();
    let p = add_provider(&s);
    for (id, name) in [(1, "News 100%"), (2, "News Other"), (3, "Sports")] {
        s.lock().unwrap().execute("INSERT INTO provider_live(id,provider_id,stream_id,name,category,category_id) VALUES(?1,?2,?3,?4,'General','7')",params![format!("iptv:{p}:{id}"),p,id.to_string(),name]).unwrap();
    }
    let data = s.live(None, Some("%".into()), 0, 100).unwrap();
    assert_eq!(data["total"], 1);
    assert_eq!(data["channels"][0]["name"], "News 100%");
    let data = s.live(Some("7".into()), None, 1, 1).unwrap();
    assert_eq!(data["total"], 3);
    assert_eq!(data["channels"].as_array().unwrap().len(), 1);
    assert!(s.channel_url("iptv:999:1").is_err());
    assert!(s
        .channel_url(&format!("iptv:{p}:1"))
        .unwrap()
        .ends_with("/live/user/SUPER_SECRET/1.ts"));
}
#[test]
fn live_categories_use_complete_scoped_groups_and_exact_ids() {
    let s = service();
    let first = add_provider(&s);
    let second = s.add(json!({"name":"Second","url":"https://second.example.com/base","username":"user","password":"SUPER_SECRET"})).unwrap()["id"].as_i64().unwrap();
    let hidden = s.add(json!({"name":"hidden","url":"https://hidden.example.com/base","username":"user","password":"SUPER_SECRET"})).unwrap()["id"].as_i64().unwrap();
    let disabled = s.add(json!({"name":"disabled","url":"https://disabled.example.com/base","username":"user","password":"SUPER_SECRET"})).unwrap()["id"].as_i64().unwrap();
    {
        let db = s.lock().unwrap();
        db.execute("UPDATE providers SET enable_live=0 WHERE id=?1", [hidden])
            .unwrap();
        db.execute("UPDATE providers SET enabled=0 WHERE id=?1", [disabled])
            .unwrap();
        for (provider, stream, category, category_id) in [
            (first, 1, Some("News"), Some("7")),
            (first, 2, Some(" News "), Some("7")),
            (second, 3, Some("News"), Some("91")),
            (first, 4, Some("7"), Some("99")),
            (first, 5, None, None),
            (first, 6, Some(""), Some("29")),
            (first, 7, Some("Actualités / UK"), Some("30")),
            (hidden, 8, Some("Hidden"), Some("8")),
            (disabled, 9, Some("Disabled"), Some("9")),
        ] {
            db.execute("INSERT INTO provider_live(id,provider_id,stream_id,name,category,category_id) VALUES(?1,?2,?3,?4,?5,?6)",
                    params![format!("iptv:{provider}:{stream}"),provider,stream.to_string(),format!("Channel {stream}"),category,category_id]).unwrap();
        }
    }
    let categories = s.live_categories(0, 100).unwrap();
    assert_eq!(categories["total"], 5);
    let rows = categories["categories"].as_array().unwrap();
    for category in rows {
        let channels = s
            .live(Some(category["id"].as_str().unwrap().into()), None, 0, 100)
            .unwrap();
        assert_eq!(channels["total"], category["count"]);
    }
    let news = rows.iter().find(|r| r["name"] == "News").unwrap();
    assert_eq!(news["count"], 3);
    assert_eq!(
        s.live(Some("category:7".into()), None, 0, 100).unwrap()["total"],
        1
    );
    assert_eq!(
        s.live(Some("category:".into()), None, 0, 100).unwrap()["total"],
        1
    );
    assert_eq!(
        s.live(Some("category:Actualités / UK".into()), None, 0, 100)
            .unwrap()["total"],
        1
    );
    // Existing clients can still filter by upstream category ID.
    assert_eq!(s.live(Some("7".into()), None, 0, 100).unwrap()["total"], 3);
    assert_eq!(
        s.live(
            Some("category:News".into()),
            Some("Channel 3".into()),
            0,
            100
        )
        .unwrap()["total"],
        1
    );
}

#[test]
fn live_category_pagination_is_stable_and_capped() {
    let s = service();
    let provider = add_provider(&s);
    {
        let db = s.lock().unwrap();
        for i in 0..105 {
            db.execute("INSERT INTO provider_live(id,provider_id,stream_id,name,category) VALUES(?1,?2,?3,'Channel',?4)",
                    params![format!("iptv:{provider}:{i}"),provider,i.to_string(),format!("Category {i:03}")]).unwrap();
        }
    }
    let all = s.live_categories(0, usize::MAX).unwrap();
    assert_eq!(all["total"], 105);
    assert_eq!(all["categories"].as_array().unwrap().len(), 100);
    let page = s.live_categories(100, 20).unwrap();
    assert_eq!(page["total"], 105);
    assert_eq!(page["categories"].as_array().unwrap().len(), 5);
    assert_eq!(page["categories"][0]["id"], "category:Category 100");
    assert_eq!(page["categories"][4]["name"], "Category 104");
    assert!(s.live_categories(usize::MAX, 100).unwrap()["categories"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn mock_xtream_survives_abandoned_connection() {
    let (url, _, actions, task) = mock_xtream().await;
    let socket = tokio::net::TcpStream::connect(url.trim_start_matches("http://"))
        .await
        .unwrap();
    drop(socket);
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
        .get(format!("{url}/player_api.php"))
        .query(&[
            ("username", "u/+"),
            ("password", "SECRET&?"),
            ("action", "get_live_categories"),
        ])
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    assert_eq!(actions.lock().unwrap().as_slice(), ["get_live_categories"]);
    task.abort();
}

#[tokio::test]
async fn network_errors_never_contain_credential_urls() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let s = service();
    let p=s.add(json!({"name":"Unavailable","url":format!("http://{address}"),"username":"PRIVATE_USER","password":"PRIVATE_PASSWORD"})).unwrap()["id"].as_i64().unwrap();
    let error = s.sync(p).await.unwrap_err();
    assert!(!error.contains("PRIVATE"));
    assert!(!error.contains("http"));
}

#[tokio::test]
async fn movie_streams_are_raw_internal_urls() {
    let s = service();
    let p = add_provider(&s);
    insert_candidate(&s, p, "10", "movie");
    let streams = s
        .streams(json!({"type":"movie","id":"tt1234567","name":"Amelie","year":2001}))
        .await
        .unwrap();
    assert_eq!(streams.len(), 1);
    assert!(streams[0]["url"]
        .as_str()
        .unwrap()
        .ends_with("/movie/user/SUPER_SECRET/10.mkv"));
    assert!(streams[0].get("id").is_none());
    assert_eq!(streams[0]["source"], format!("iptv:{p}"));
}
