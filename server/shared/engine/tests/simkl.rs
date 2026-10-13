use serde_json::json;
use viptv_engine::simkl;

#[test]
fn simkl_titles_and_episodes_keep_distinct_exact_identities() {
    let parent = simkl::card(
        &json!({"title":"Show","ids":{"simkl":42,"imdb":"tt123"},"poster":"42/abc","progress":0.7}),
        "tv",
    )
    .unwrap();
    assert_eq!(parent.identity.id, "simkl:tv:42");
    assert_eq!(parent.progress, None);
    let episodes = simkl::episodes(
        &json!([
            {"season":2,"episode":3,"ids":{"simkl_id":9},"title":"Three"},
            {"type":"special","ids":{"simkl_id":10},"title":"No coordinates"}
        ]),
        &parent,
    )
    .unwrap();
    assert_eq!(episodes.len(), 1);
    assert_eq!(episodes[0].identity.id, "simkl:tv:42:2:3");
    assert_eq!(episodes[0].identity.ids[0].namespace, "simkl_episode");
    assert_eq!(episodes[0].identity.ids[0].value, "9");
    assert_eq!(
        episodes[0].identity.episode.as_ref().unwrap().season,
        Some(2)
    );
    assert!(simkl::episodes(&json!([{"season":1,"episode":1,"ids":{"simkl":2}}, {"season":1,"episode":1,"ids":{"simkl":3}}]), &parent).is_err());
    assert!(
        simkl::card(
            &json!({"title":"Wrong kind","endpoint_type":"anime","ids":{"simkl_id":42}}),
            "movie"
        )
        .is_err()
    );
    assert!(simkl::card(&json!({"title":"No identity"}), "movie").is_err());
}

#[test]
fn conflicting_simkl_identifiers_and_content_types_are_rejected() {
    assert!(
        simkl::card(
            &json!({"title":"Ambiguous", "ids":{"simkl":1,"simkl_id":2}}),
            "movie"
        )
        .is_err()
    );
    assert!(
        simkl::card(
            &json!({"title":"Wrong kind", "type":"show", "ids":{"simkl":1}}),
            "movie"
        )
        .is_err()
    );
    let parent = simkl::card(&json!({"title":"Show", "ids":{"simkl":1}}), "tv").unwrap();
    assert!(simkl::episodes(&json!([{ "season":1,"episode":1,"ids":{"simkl":2} },{ "season":1,"episode":2,"ids":{"simkl":2} }]), &parent).is_err());
}

#[test]
fn source_targets_require_authoritative_ids_and_verified_episode_coordinates() {
    let movie = simkl::card(
        &json!({"title":"Movie","ids":{"simkl":10,"imdb":"tt1234567"}}),
        "movie",
    )
    .unwrap();
    assert_eq!(
        simkl::source_identity(&movie, None).unwrap().id,
        "tt1234567"
    );
    let mut missing = movie.clone();
    missing.identity.ids.retain(|id| id.namespace != "imdb");
    assert!(simkl::source_identity(&missing, None).is_err());
    let parent = simkl::card(
        &json!({"title":"TV","ids":{"simkl":20,"imdb":"tt7654321"}}),
        "tv",
    )
    .unwrap();
    let mut episode = simkl::episodes(
        &json!([{"season":2,"episode":4,"ids":{"simkl_id":30}}]),
        &parent,
    )
    .unwrap()
    .remove(0);
    let target = simkl::source_identity(&parent, Some(&episode)).unwrap();
    assert_eq!(target.id, "tt7654321:2:4");
    assert!(target.ids.is_empty());
    episode.identity.episode.as_mut().unwrap().episode = 5;
    assert!(simkl::source_identity(&parent, Some(&episode)).is_err());
    assert!(simkl::source_identity(&parent, None).is_err());
    let mut conflict = movie.clone();
    conflict
        .identity
        .ids
        .push(viptv_engine::identity::ExternalId {
            namespace: "imdb".into(),
            value: "tt9999999".into(),
        });
    assert!(simkl::source_identity(&conflict, None).is_err());
}
