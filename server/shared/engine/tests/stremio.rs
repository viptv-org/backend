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
