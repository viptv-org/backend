use super::*;

#[tokio::test]
async fn scopes_hide_queens_live_but_allow_both_vod_providers() {
    let s = service();
    let queens = add_provider(&s);
    let other = s.add(json!({"name":"Other live provider","url":"https://other.example.com","username":"u","password":"p"})).unwrap()["id"].as_i64().unwrap();
    for p in [queens, other] {
        s.store_index(
            p,
            json!([]),
            json!([{"stream_id":11,"name":"World News"}]),
            json!([{"stream_id":2318,"name":"Inception (2010)"}]),
            json!([{"series_id":9562,"name":"Breaking Bad (2008)"}]),
        )
        .unwrap();
    }
    s.update(queens, json!({"enable_live":false})).unwrap();
    let hidden = format!("iptv:{queens}:11");
    let visible = format!("iptv:{other}:11");
    assert_eq!(s.live(None, None, 0, 100).unwrap()["total"], 1);
    assert_eq!(
        s.live(None, Some("World".into()), 0, 100).unwrap()["channels"][0]["id"],
        visible
    );
    assert!(s.channel_source(&hidden).is_err());
    assert!(s.channel_url(&hidden).is_err());
    assert!(s.guide(hidden.clone()).await.is_err());
    assert!(s.streams(json!({"type":"live","id":hidden})).await.is_err());
    assert!(s.channel_source(&visible).is_ok());
    assert_eq!(
        s.streams(json!({"type":"movie","id":"tt1375666","name":"Inception","year":2010}))
            .await
            .unwrap()
            .len(),
        2
    );
    let req =
        request(json!({"type":"series","id":"tt0903747:1:1","name":"Breaking Bad","year":2008}));
    assert_eq!(
        s.candidates_filtered(Some("series"), Some(&req))
            .unwrap()
            .len(),
        2
    );
    let queued = s
        .candidates(Some("movie"))
        .unwrap()
        .into_iter()
        .find(|c| c.provider_id == queens)
        .unwrap();
    s.update(queens, json!({"enable_movies":false,"enable_series":false}))
        .unwrap();
    assert!(s.resolve_candidate(queued.clone(), 0, 0).await.is_err());
    assert!(s.enrich_candidate(queued).await.is_err());
    assert_eq!(s.candidates(Some("movie")).unwrap().len(), 1);
    assert_eq!(
        s.candidates_filtered(Some("series"), Some(&req))
            .unwrap()
            .len(),
        1
    );
    // Even a sync result already in flight cannot delete disabled scopes or manual mappings.
    s.override_match(json!({"type":"movie","vod_id":format!("iptv:{queens}:movie:2318"),"metadata_id":"tt1375666"})).unwrap();
    s.store_index(queens, json!([]), json!([]), json!([]), json!([]))
        .unwrap();
    s.update(
        queens,
        json!({"enable_live":true,"enable_movies":true,"enable_series":true}),
    )
    .unwrap();
    assert!(s.channel_source(&hidden).is_ok());
    assert_eq!(s.candidates(Some("movie")).unwrap().len(), 2);
    assert_eq!(s.candidates(Some("series")).unwrap().len(), 2);
    assert!(s
        .candidates(Some("movie"))
        .unwrap()
        .iter()
        .any(|c| c.provider_id == queens && c.override_id.as_deref() == Some("tt1375666")));
}

#[tokio::test]
async fn scoped_sync_skips_live_and_queued_details_recheck_scope() {
    let (url, _, actions, task) = mock_xtream().await;
    let s = service();
    let p = s.add(json!({"name":"VOD only","url":url,"username":"u/+","password":"SECRET&?","enable_live":false})).unwrap()["id"].as_i64().unwrap();
    s.sync(p).await.unwrap();
    assert_eq!(actions.lock().unwrap().len(), 2);
    assert!(actions
        .lock()
        .unwrap()
        .iter()
        .all(|a| a == "get_vod_streams" || a == "get_series"));
    let provider = s.provider(p).unwrap();
    let permit = s.semaphore.acquire_many(4).await.unwrap();
    let mut future = Box::pin(s.cached_api(&provider, "get_vod_info", "2318"));
    assert!(tokio::time::timeout(Duration::from_millis(20), &mut future)
        .await
        .is_err());
    s.update(p, json!({"enable_movies":false})).unwrap();
    drop(permit);
    assert!(future.await.is_err());
    assert_eq!(actions.lock().unwrap().len(), 2);
    s.update(p, json!({"enable_movies":true})).unwrap();
    s.cached_api(&provider, "get_vod_info", "2318")
        .await
        .unwrap();
    s.update(p, json!({"enable_movies":false})).unwrap();
    assert!(s
        .cached_api(&provider, "get_vod_info", "2318")
        .await
        .is_err());
    s.update(p, json!({"enable_movies":true})).unwrap();
    let (other_url, _, _, other_task) = mock_xtream().await;
    let other = s
        .add(json!({"name":"Live and VOD","url":other_url,"username":"u/+","password":"SECRET&?"}))
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    s.sync(other).await.unwrap();
    assert_eq!(s.live(None, None, 0, 100).unwrap()["total"], 1);
    for kind in ["movie", "series"] {
        let rows = s.candidates(Some(kind)).unwrap();
        assert_eq!(rows.len(), 2);
        for row in rows {
            assert_eq!(s.resolve_candidate(row, 1, 2).await.unwrap().len(), 1);
        }
    }
    other_task.abort();
    let _ = other_task.await;
    task.abort();
    let _ = task.await;
}

#[test]
fn empty_vod_names_sync_without_synthetic_title_matches() {
    let s = service();
    let p = add_provider(&s);
    s.store_index(
        p,
        json!([]),
        json!([{"stream_id":1,"name":"Live"}]),
        json!([
            {"stream_id":10,"name":"Named (2001)"},
            {"stream_id":11,"name":"","year":2001,"imdb_id":"tt1234567","tmdb_id":42}
        ]),
        json!([
            {"series_id":20,"name":"Named series (2001)"},
            {"series_id":21,"name":" \t\n ","year":2001,"imdb":"tt7654321","tmdb":43}
        ]),
    )
    .unwrap();
    assert_eq!(s.candidates(None).unwrap().len(), 4);
    for (kind, stream, imdb, tmdb) in [
        ("movie", "11", "tt1234567", "tmdb:42"),
        ("series", "21", "tt7654321", "tmdb:43"),
    ] {
        let cs = s.candidates(Some(kind)).unwrap();
        let c = cs.iter().find(|c| c.stream_id == stream).unwrap();
        assert_eq!(c.name, format!("Untitled {kind} #{stream}"));
        assert!(c.normalized.is_empty());
        assert_eq!(c.imdb_id.as_deref(), Some(imdb));
        assert_eq!(c.tmdb_id.as_deref(), Some(tmdb));
        for id in [imdb, tmdb] {
            let req = request(json!({"id":id,"type":kind}));
            let filtered = s.candidates_filtered(Some(kind), Some(&req)).unwrap();
            assert_eq!(select_candidates(&filtered, &req).len(), 1);
        }
        for name in [c.name.as_str(), "", "!!!"] {
            let req = request(json!({"id":"unmapped","type":kind,"name":name,"year":2001}));
            assert!(select_candidates(&cs, &req).is_empty());
            assert!(s
                .candidates_filtered(Some(kind), Some(&req))
                .unwrap()
                .is_empty());
        }
    }
}

#[test]
fn malformed_vod_rows_still_reject_index_atomically() {
    let s = service();
    let p = add_provider(&s);
    let original = insert_candidate(&s, p, "99", "movie");
    s.store_index(
        p,
        json!([]),
        json!([{"stream_id":1,"name":"Live"}]),
        json!([{"stream_id":99,"name":"Amélie (2001)"}]),
        json!([]),
    )
    .unwrap();
    for kind in ["movie", "series"] {
        let key = if kind == "series" {
            "series_id"
        } else {
            "stream_id"
        };
        let mut invalid = vec![json!({key:10})];
        for name in [Value::Null, json!(123), json!(false), json!([]), json!({})] {
            invalid.push(json!({key:10,"name":name}));
        }
        invalid.push(json!({"name":""}));
        for id in [
            Value::Null,
            json!(""),
            json!("bad"),
            json!(-1),
            json!(1.5),
            json!(true),
            json!([]),
            json!({}),
            json!("123456789012345678901234567890123"),
        ] {
            invalid.push(json!({key:id,"name":""}));
        }
        for row in invalid {
            assert!(candidate_from_json(p, kind, &row).is_none(), "{row}");
            let rows = json!([{key:10,"name":""}, row]);
            let (movies, series) = if kind == "movie" {
                (rows, json!([]))
            } else {
                (json!([]), rows)
            };
            assert!(s
                .store_index(p, json!([]), json!([]), movies, series)
                .is_err());
            let cs = s.candidates(None).unwrap();
            assert_eq!(cs.len(), 1);
            assert_eq!(cs[0].id, original);
            let live: i64 = s
                .lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM provider_live", [], |r| r.get(0))
                .unwrap();
            assert_eq!(live, 1);
        }
    }
    assert!(s
        .store_index(
            p,
            json!([]),
            json!([{"stream_id":1,"name":""}]),
            json!([]),
            json!([])
        )
        .is_err());
    assert_eq!(s.candidates(None).unwrap()[0].id, original);
}

#[tokio::test]
async fn lazy_queens_details_match_sparse_movies_and_series() {
    let (url, _, actions, task) = mock_xtream().await;
    let s = service();
    let p = s
        .add(json!({"name":"Queens fixture","url":url,"username":"u/+","password":"SECRET&?"}))
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    let id = sparse_row(&s, p, "2318", "movie", "Inception");
    let req = json!({"type":"movie","id":"tt1375666","name":"Inception","year":2010});
    assert!(s
        .candidates_filtered(Some("movie"), Some(&request(req.clone())))
        .unwrap()
        .is_empty());
    // Details are fetched but a wrong requested year cannot become an ID shortcut.
    assert!(s
        .streams(json!({"type":"movie","id":"tmdb:27205","name":"Inception","year":2011}))
        .await
        .unwrap()
        .is_empty());
    let streams = s.streams(req.clone()).await.unwrap();
    assert_eq!(streams.len(), 1);
    assert!(streams[0]["url"].as_str().unwrap().ends_with("/2318.mp4"));
    let row = s.candidates(Some("movie")).unwrap().remove(0);
    assert_eq!(row.year, Some(2010));
    assert_eq!(row.tmdb_id.as_deref(), Some("tmdb:27205"));
    assert!(row.imdb_id.is_none()); // Never manufacture an IMDb/TMDB crosswalk.
    assert_eq!(
        s.streams(json!({"type":"movie","id":"tt1375666","tmdb_id":"27205"}))
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(s
        .streams(json!({"type":"movie","id":"tmdb:999","name":"Inception","year":2010}))
        .await
        .unwrap()
        .is_empty());
    // Simulate index refresh losing metadata: the detail cache still prevents another fetch.
    s.lock()
        .unwrap()
        .execute(
            "UPDATE provider_vod SET year=NULL,tmdb_id=NULL WHERE id=?1",
            [&id],
        )
        .unwrap();
    assert_eq!(s.streams(req.clone()).await.unwrap().len(), 1);
    assert_eq!(
        actions
            .lock()
            .unwrap()
            .iter()
            .filter(|a| *a == "get_vod_info")
            .count(),
        1
    );
    s.override_match(json!({"vod_id":id,"metadata_id":"tt9999999","type":"movie"}))
        .unwrap();
    assert!(s.streams(req).await.unwrap().is_empty());
    sparse_row(&s, p, "9562", "series", "Breaking Bad");
    let req = json!({"type":"series","id":"tt0903747:1:1","name":"Breaking Bad","year":2008});
    let streams = s.streams(req.clone()).await.unwrap();
    assert_eq!(streams.len(), 1);
    assert!(streams[0]["url"].as_str().unwrap().ends_with("/942671.mp4"));
    assert_eq!(s.streams(req).await.unwrap().len(), 1);
    assert_eq!(
        actions
            .lock()
            .unwrap()
            .iter()
            .filter(|a| *a == "get_series_info")
            .count(),
        1
    );
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn lazy_details_are_bounded_cached_and_fail_closed() {
    let (url, _, actions, task) = mock_xtream().await;
    let s = service();
    let p = s
        .add(json!({"name":"Queens fixture","url":url,"username":"u/+","password":"SECRET&?"}))
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    for id in 4000..4040 {
        sparse_row(&s, p, &id.to_string(), "movie", "Inception");
    }
    let req = json!({"type":"movie","id":"tt1375666","name":"Inception","year":2010});
    assert_eq!(
        s.sparse_candidates("movie", &request(req.clone()), None)
            .unwrap()
            .len(),
        MAX_LAZY_DETAILS
    );
    let mut emitted = Vec::new();
    s.stream_batches(req.clone(), |_, result| {
        if let Ok(rows) = result {
            emitted.extend(rows);
        }
    })
    .await
    .unwrap();
    assert!(emitted.is_empty());
    assert_eq!(actions.lock().unwrap().len(), MAX_LAZY_DETAILS);
    s.stream_batches(req, |_, result| {
        if let Ok(rows) = result {
            emitted.extend(rows);
        }
    })
    .await
    .unwrap();
    assert!(emitted.is_empty());
    // Structurally valid malformed/mismatched details are cached, transport/schema failures are not.
    assert!(actions.lock().unwrap().len() <= MAX_LAZY_DETAILS + 2);
    assert!(s
        .candidates(Some("movie"))
        .unwrap()
        .iter()
        .filter(|c| c.stream_id != "4002")
        .all(|c| c.year.is_none()));
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn sync_is_atomic_preserves_overrides_and_resolves_series_lazily() {
    let (url, fail, actions, task) = mock_xtream().await;
    let s = service();
    let p = s
        .add(json!({"name":"Fixture","url":url,"username":"u/+","password":"SECRET&?"}))
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    let result = s.sync(p).await.unwrap();
    assert_eq!(result, json!({"provider_id":p,"live":1,"vod":1,"series":1}));
    assert!(!actions
        .lock()
        .unwrap()
        .iter()
        .any(|a| a == "get_series_info"));
    let vod_id = format!("iptv:{p}:movie:22");
    s.override_match(json!({"vod_id":vod_id,"type":"movie","metadata_id":"tt7654321"}))
        .unwrap();
    s.sync(p).await.unwrap();
    assert_eq!(
        s.candidates(Some("movie")).unwrap()[0]
            .override_id
            .as_deref(),
        Some("tt7654321")
    );
    fail.store(true, std::sync::atomic::Ordering::SeqCst);
    let error = s.sync(p).await.unwrap_err();
    assert!(!error.contains("SECRET"));
    assert_eq!(s.live(None, None, 0, 100).unwrap()["total"], 1);
    assert_eq!(s.candidates(Some("movie")).unwrap().len(), 1);
    let streams = s
        .streams(json!({"type":"series","id":"tt1234567:1:2"}))
        .await
        .unwrap();
    assert_eq!(streams.len(), 1);
    assert!(streams[0]["url"]
        .as_str()
        .unwrap()
        .ends_with("/series/u%2F+/SECRET&%3F/44.mp4"));
    let cached = s
        .streams(json!({"type":"series","id":"tt1234567:1:2"}))
        .await
        .unwrap();
    assert_eq!(streams, cached);
    assert_eq!(
        actions
            .lock()
            .unwrap()
            .iter()
            .filter(|a| a.as_str() == "get_series_info")
            .count(),
        1
    );
    let guide = s.guide(format!("iptv:{p}:11")).await.unwrap();
    assert_eq!(guide["programs"].as_array().unwrap().len(), 1);
    assert_eq!(guide["programs"][0]["title"], "News");
    assert_eq!(guide["programs"][0]["start"], 1700000000i64);
    assert_eq!(s.guide(format!("iptv:{p}:11")).await.unwrap(), guide);
    assert_eq!(
        actions
            .lock()
            .unwrap()
            .iter()
            .filter(|a| a.as_str() == "get_short_epg")
            .count(),
        1
    );
    s.lock()
        .unwrap()
        .execute("UPDATE provider_cache SET expires_at=0", [])
        .unwrap();
    assert_eq!(s.guide(format!("iptv:{p}:11")).await.unwrap(), guide);
    assert_eq!(
        actions
            .lock()
            .unwrap()
            .iter()
            .filter(|a| a.as_str() == "get_short_epg")
            .count(),
        2
    );
    task.abort();
    let _ = task.await;
}

#[test]
fn sqlite_lookup_filters_types_and_honors_metadata_and_overrides() {
    let s = service();
    let p = add_provider(&s);
    let movie = insert_candidate(&s, p, "10", "movie");
    insert_candidate(&s, p, "11", "series");
    let r = request(json!({"id":"tt1234567","name":"Amelie","year":2001}));
    let matches = s.candidates_filtered(Some("movie"), Some(&r)).unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].id, movie);
    s.lock()
        .unwrap()
        .execute(
            "UPDATE provider_vod SET imdb_id='tt1234567' WHERE id=?1",
            [&movie],
        )
        .unwrap();
    let r = request(json!({"id":"tt1234567"}));
    assert_eq!(
        s.candidates_filtered(Some("movie"), Some(&r))
            .unwrap()
            .len(),
        1
    );
    s.override_match(json!({"vod_id":movie,"metadata_id":"tt7654321","type":"movie"}))
        .unwrap();
    let rows = s.candidates_filtered(Some("movie"), Some(&r)).unwrap();
    assert!(select_candidates(&rows, &r).is_empty());
    let r = request(json!({"id":"tt7654321"}));
    assert_eq!(
        s.candidates_filtered(Some("movie"), Some(&r))
            .unwrap()
            .len(),
        1
    );
}
