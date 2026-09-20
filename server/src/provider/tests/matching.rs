use super::*;

#[tokio::test]
async fn unspecified_episode_containers_use_transport_route_not_live_kind() {
    let s = service();
    let p = add_provider(&s);
    let id = insert_candidate(&s, p, "9562", "series");
    s.override_match(json!({"vod_id":id,"type":"series","metadata_id":"opaque:series:bb"}))
        .unwrap();
    let info = json!({"episodes":{"1":[
        {"id":"2008185","episode_num":1,"container_extension":null},
        {"id":"2008186","episode_num":2},
        {"id":"2008187","episode_num":3,"container_extension":""},
        {"id":"2008188","episode_num":4,"container_extension":"  "},
        {"id":"2008189","episode_num":5,"container_extension":"mp4"},
        {"id":"2008190","episode_num":6,"container_extension":"mkv"},
        {"id":"2008191","episode_num":7,"container_extension":false}
    ]}});
    s.lock().unwrap().execute("INSERT INTO provider_cache(provider_id,cache_key,expires_at,payload) VALUES(?1,'get_series_info:9562',?2,?3)",params![p,crate::util::now()+300,info.to_string()]).unwrap();
    s.update(p, json!({"enable_live":false})).unwrap();
    for (episode, stream, ext) in [
        (1, "2008185", "ts"),
        (2, "2008186", "ts"),
        (3, "2008187", "ts"),
        (4, "2008188", "ts"),
        (5, "2008189", "mp4"),
        (6, "2008190", "mkv"),
        (7, "2008191", "mp4"),
    ] {
        let request = json!({"type":"series","id":format!("opaque:series:bb:1:{episode}")});
        let rows = s.streams(request).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0]["url"]
            .as_str()
            .unwrap()
            .ends_with(&format!("/series/user/SUPER_SECRET/{stream}.{ext}")));
    }
    let movie =
        candidate_from_json(p, "movie", &json!({"stream_id":2318,"name":"Inception"})).unwrap();
    assert_eq!(movie.extension, "mp4");
    assert!(s.resolve_candidate(movie, 0, 0).await.unwrap()[0]["url"]
        .as_str()
        .unwrap()
        .ends_with("/movie/user/SUPER_SECRET/2318.mp4"));
    s.update(p, json!({"enable_live":true,"enable_series":false}))
        .unwrap();
    assert!(s
        .streams(json!({"type":"series","id":"opaque:series:bb:1:1"}))
        .await
        .unwrap()
        .is_empty());
    assert!(s.acquire_playback_for_kind(p, "series").await.is_err());
}

#[test]
fn normalization_is_conservative_and_unicode_aware() {
    assert_eq!(normalize("  Amélie: THE   Film! "), "amelie the film");
    assert_eq!(title_year("Amélie (2001)"), ("Amélie".into(), Some(2001)));
    assert_eq!(title_year("Amélie [2001]"), ("Amélie".into(), Some(2001)));
    assert_eq!(title_year("Amélie 2001"), ("Amélie".into(), Some(2001)));
    assert_eq!(title_year("2001"), ("2001".into(), None));
    assert_ne!(normalize("Film 4K"), normalize("Film"));
    assert_eq!(
        title_year("Film (Extended)"),
        ("Film (Extended)".into(), None)
    );
}

#[test]
fn metadata_ids_take_priority_and_conflicts_do_not_title_match() {
    let c =
        candidate(json!({"stream_id":10,"name":"Other (1999)","imdb_id":"tt1234567","tmdb":42}));
    let cs = vec![c];
    assert_eq!(
        select_candidates(&cs, &request(json!({"id":"tt1234567"}))).len(),
        1
    );
    assert_eq!(
        select_candidates(&cs, &request(json!({"id":"tmdb:42"}))).len(),
        1
    );
    assert_eq!(
        select_candidates(
            &cs,
            &request(json!({"id":"tt7654321","name":"Other","year":1999}))
        )
        .len(),
        0
    );
}
#[test]
fn fallback_requires_both_title_and_year() {
    let cs = vec![candidate(json!({"stream_id":"10","name":"Amélie (2001)"}))];
    assert_eq!(
        select_candidates(
            &cs,
            &request(json!({"id":"tt1234567","name":"Amelie","year":"2001"}))
        )
        .len(),
        1
    );
    for v in [
        json!({"id":"tt1234567","name":"Amelie"}),
        json!({"id":"tt1234567","name":"Amelie","year":2002}),
        json!({"id":"tt1234567","name":"Amelie 4K","year":2001}),
    ] {
        assert!(select_candidates(&cs, &request(v)).is_empty());
    }
}
#[test]
fn overrides_are_authoritative_and_type_checked() {
    let s = service();
    let p = add_provider(&s);
    let id = insert_candidate(&s, p, "10", "movie");
    assert_eq!(s.matches().unwrap().as_array().unwrap().len(), 1);
    assert!(s
        .override_match(json!({"vod_id":id,"metadata_id":"tt1234567","type":"series"}))
        .is_err());
    s.override_match(json!({"vod_id":id,"metadata_id":"imdb:tt1234567","type":"movie"}))
        .unwrap();
    let cs = s.candidates(Some("movie")).unwrap();
    assert_eq!(
        select_candidates(&cs, &request(json!({"id":"tt1234567"}))).len(),
        1
    );
    assert!(select_candidates(
        &cs,
        &request(json!({"id":"tt7654321","name":"Amelie","year":2001}))
    )
    .is_empty());
    assert!(s.matches().unwrap().as_array().unwrap().is_empty());
}
#[test]
fn deleting_provider_removes_all_owned_rows() {
    let s = service();
    let p = add_provider(&s);
    let id = insert_candidate(&s, p, "10", "movie");
    s.override_match(json!({"vod_id":id,"metadata_id":"tt1234567","type":"movie"}))
        .unwrap();
    s.delete(p).unwrap();
    assert_eq!(s.list().unwrap(), json!([]));
    assert_eq!(s.matches().unwrap(), json!([]));
    assert_eq!(
        s.lock()
            .unwrap()
            .query_row("SELECT count(*) FROM provider_matches", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(s.delete(p).is_err());
}

#[test]
fn credential_path_segments_are_encoded() {
    let p = Provider {
        id: 1,
        name: "p".into(),
        url: "https://example.com/prefix/player_api.php".into(),
        username: "a/b".into(),
        password: "p?# /".into(),
    };
    let url = media_url(&p, "movie", "12", "mkv").unwrap();
    assert_eq!(
        url,
        "https://example.com/prefix/movie/a%2Fb/p%3F%23%20%2F/12.mkv"
    );
    assert!(media_url(&p, "movie", "../evil", "mp4").is_err());
    assert_eq!(extension(Some("../../evil")), "mp4");
}
#[test]
fn series_episode_ids_support_namespaces_and_reject_conflicts() {
    let r = request(json!({"type":"series","id":"tmdb:42:0:3"}));
    assert!(r.ids.contains("tmdb:42"));
    assert_eq!((r.season, r.episode), (Some(0), Some(3)));
    assert!(MatchRequest::parse(&json!({"id":"tt1234567:1:2","season":3}), "series").is_err());
    assert!(MatchRequest::parse(&json!({"id":"tt1234567:1:2","season":-1}), "series").is_err());
    assert!(MatchRequest::parse(&json!({"id":"tt1234567","episode":"invalid"}), "series").is_err());
    let r = request(json!({"type":"series","id":"tmdb:42","season":1,"episode":2}));
    assert!(r.ids.contains("tmdb:42"));
}
#[test]
fn lazy_episode_selection_uses_numbers_not_array_position() {
    let v = json!({"episodes":{"1":[{"id":91,"episode_num":2,"season":1},{"id":90,"episode_num":1},{"id":92,"episode_num":2,"season":2}],"2":[{"id":100,"episode_num":2}]}});
    let rows = episode_rows(&v, 1, 2);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], 91);
    assert!(episode_rows(&v, 3, 1).is_empty());
    let v = json!({"episodes":[{"id":5,"episode_num":"3","season":"0"}]});
    assert_eq!(episode_rows(&v, 0, 3).len(), 1);
}
#[test]
fn epg_decoding_and_timestamps_are_safe() {
    assert_eq!(decode_epg(Some(&json!("TmV3cw=="))), "News");
    assert_eq!(decode_epg(Some(&json!("Breaking news!"))), "Breaking news!");
    assert_eq!(decode_epg(None), "");
    assert_eq!(timestamp(&json!("1700000000")), Some(1700000000));
    assert_eq!(timestamp(&json!(-1)), None);
}

#[test]
fn lazy_detail_validation_rejects_mismatches_and_preserves_known_evidence() {
    let c = candidate(json!({"stream_id":2318,"name":"Inception"}));
    for detail in [
        json!({"info": []}),
        json!({"info":{"releasedate":"2010-07-15"}}),
        json!({"info":{"name":"Inception","releasedate":"2010-99-99"}}),
        json!({"info":{"name":"Inception","releasedate":"2010-07-15","year":2011}}),
        json!({"info":{"name":"Inception","tmdb_id":"not-an-id"}}),
        json!({"info":{"name":"Inception"},"movie_data":{"name":"Another movie"}}),
        json!({"info":{"name":"Inception"},"movie_data":{"stream_id":999}}),
        json!({"info":{"name":"Inception"},"movie_data":[]}),
    ] {
        assert!(validated_details(&c, &detail).is_none(), "{detail}");
    }
    let known = candidate(json!({"stream_id":2318,"name":"Inception","year":2011,"tmdb_id":999}));
    assert!(validated_details(
        &known,
        &json!({"info":{"name":"Inception","releasedate":"2010-07-15","tmdb_id":27205}})
    )
    .is_none());
    let empty = candidate(json!({"stream_id":2318,"name":""}));
    assert!(validated_details(&empty, &json!({"info":{"name":"Inception","year":2010}})).is_none());
}
