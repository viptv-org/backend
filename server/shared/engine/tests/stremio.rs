use viptv_engine::{capabilities::Capability, stremio::*};

#[test]
fn manifest_catalogs_and_capabilities_are_independent() {
    let m=parse_manifest(r#"{"id":"a","name":"Provider","version":"1","resources":["catalog","meta","stream"],"catalogs":[{"id":"top","type":"movie","name":"Popular","extra":[{"name":"search"}]},{"id":"filtered","type":"series","extra":[{"name":"genre","isRequired":true}]}]}"#).unwrap();
    assert_eq!(m.catalogs.len(), 2);
    assert!(m.capabilities.contains(&Capability::Discovery));
    assert!(m.capabilities.contains(&Capability::Sources));
    assert!(m.catalogs[0].search);
    assert_eq!(
        catalog_url("https://example.com/manifest.json", &m.catalogs[0]).unwrap(),
        "https://example.com/catalog/movie/top.json"
    );
    assert!(m.catalogs[1].required_extras);
}

#[test]
fn catalog_projection_does_not_use_a_poster_as_landscape_or_merge_unrelated_ids() {
    let cards=cards(r#"{"metas":[{"id":"tt123","type":"movie","name":"A","poster":"https://images.example/poster"},{"id":"tt123","type":"movie","name":"A duplicate"},{"id":"tt124","type":"movie","name":"B","background":"https://images.example/backdrop"}]}"#,"addon:7").unwrap();
    assert_eq!(cards.len(), 2);
    assert_eq!(cards[0].landscape, None);
    assert_eq!(cards[0].identity.ids[0].namespace, "imdb");
    assert_eq!(
        cards[1].landscape.as_deref(),
        Some("https://images.example/backdrop")
    );
}

#[test]
fn manifest_url_rejects_embedded_userinfo_and_unbounded_or_wrong_documents() {
    assert!(manifest_url("https://example.com/manifest.json").is_ok());
    assert!(manifest_url("https://name:password@example.com/manifest.json").is_err());
    assert!(manifest_url("https://example.com/catalog.json").is_err());
    assert!(parse_manifest("{}").is_err());
}

#[test]
fn details_keep_episode_identity_and_reject_other_titles() {
    let raw = r#"{"meta":{"id":"tt123","type":"series","name":"A series","videos":[{"id":"tt123:2:3","season":2,"episode":3,"title":"Episode three","thumbnail":"https://images.example/still"}]}}"#;
    let detail = detail(raw, "addon:a", "series", "tt123", 0).unwrap();
    assert_eq!(detail.item.identity.id, "tt123");
    assert_eq!(
        detail.episodes[0].parent_identity,
        Some(detail.item.identity.clone())
    );
    assert_eq!(detail.episodes[0].identity.id, "tt123:2:3");
    assert_eq!(
        detail.episodes[0].identity.episode.as_ref().unwrap().season,
        Some(2)
    );
    assert_eq!(
        detail.episodes[0].landscape.as_deref(),
        Some("https://images.example/still")
    );
    assert!(viptv_engine::stremio::detail(raw, "addon:a", "series", "tt999", 0).is_err());
}

#[test]
fn detail_season_index_uses_the_complete_verified_sorted_sequence_without_inventing_seasons(){
 let mut videos=Vec::new();for season in [2,1]{for episode in (1..=30).rev(){videos.push(serde_json::json!({"id":format!("opaque-{season}-{episode}"),"title":format!("Episode {episode}"),"season":season,"episode":episode}));}}
 videos.push(serde_json::json!({"id":"special","title":"Special","season":0,"episode":1}));videos.push(serde_json::json!({"id":"unknown-season","title":"Known episode, unknown season","episode":12}));videos.push(videos[0].clone());let body=serde_json::json!({"meta":{"id":"parent","name":"Series","type":"series","videos":videos}}).to_string();
 let first=detail(&body,"addon","series","parent",0).unwrap();assert_eq!(first.episodes.len(),24);assert_eq!(first.next_cursor.as_deref(),Some("24"));assert_eq!(first.seasons.iter().map(|s|(s.season,s.episode_count,s.offset)).collect::<Vec<_>>(),vec![(Some(0),1,0),(Some(1),30,1),(Some(2),30,31),(None,1,61)]);
 let second=detail(&body,"addon","series","parent",31).unwrap();assert_eq!(second.episodes[0].identity.id,"opaque-2-1");assert_eq!(second.episodes[23].identity.id,"opaque-2-24");assert_eq!(second.next_cursor.as_deref(),Some("55"));assert!(second.episodes.iter().all(|e|e.parent_identity.as_ref()==Some(&first.item.identity)));
 let terminal=detail(&body,"addon","series","parent",55).unwrap();assert_eq!(terminal.episodes.len(),7);assert!(terminal.next_cursor.is_none());let unknown=terminal.episodes.last().unwrap().identity.episode.as_ref().unwrap();assert_eq!(unknown.season,None);assert_eq!(unknown.episode,12);
}
