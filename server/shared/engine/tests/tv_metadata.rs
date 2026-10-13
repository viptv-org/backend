use serde_json::json;
use viptv_engine::{presentation::*, search, simkl};
#[test]
fn anime_search_subtype_and_detail_type_share_canonical_identity_and_category() {
    let search = simkl::card(
        &json!({"title":"Naruto","endpoint_type":"anime","type":"tv","ids":{"simkl_id":123}}),
        "anime",
    )
    .unwrap();
    let detail = simkl::card(
        &json!({"title":"Naruto","type":"anime","anime_type":"tv","ids":{"simkl":123}}),
        "anime",
    )
    .unwrap();
    assert_eq!(search.identity, detail.identity);
    assert_eq!(search.category.as_deref(), Some("anime"));
    assert_eq!(search.identity.kind, "series");
    let movie=simkl::card(&json!({"title":"Naruto movie","type":"movie","endpoint_type":"anime","ids":{"simkl_id":124}}),"anime").unwrap();
    assert_eq!(movie.identity.kind, "movie");
    assert_eq!(movie.category.as_deref(), Some("anime"));
    assert!(
        simkl::card(
            &json!({"title":"Wrong","type":"anime","endpoint_type":"anime","ids":{"simkl_id":125}}),
            "tv"
        )
        .is_err()
    );
}
#[test]
fn merged_search_ranks_exact_title_then_real_popularity_and_aliases() {
    let c = |title: &str, id: u64, votes: u64| {
        simkl::card(&json!({"title":title,"ids":{"simkl":id},"ratings":{"imdb":{"rating":8.4,"votes":votes}}}),"movie").unwrap()
    };
    let mut cards = vec![
        c("Naruto OVA", 1, 300),
        c("Naruto", 2, 4),
        c("Naruto", 3, 150000),
        c("Other", 4, 1000000),
    ];
    search::rank(&mut cards, "  NARUTO ");
    assert_eq!(
        cards
            .iter()
            .map(|c| c.identity.id.as_str())
            .collect::<Vec<_>>(),
        vec![
            "simkl:movie:3",
            "simkl:movie:2",
            "simkl:movie:1",
            "simkl:movie:4"
        ]
    );
}
#[test]
fn recommendations_use_documented_categories_and_deduplicate_without_self() {
    let related = json!({"users_recommendations":[{"title":"Naruto","type":"anime","anime_type":"tv","ids":{"simkl":1}},{"title":"Other","type":"anime","anime_type":"movie","ids":{"simkl":2}}],"similar":[{"title":"Other","type":"anime","anime_type":"movie","ids":{"simkl":2}}],"relations":[{"title":"Sequel","anime_type":"tv","ids":{"simkl":3}}]});
    let cards = simkl::recommendations(&related, "anime", "simkl:anime:1");
    assert_eq!(cards.len(), 2);
    assert_eq!(cards[0].identity.kind, "movie");
    assert_eq!(cards[1].identity.kind, "series");
}
#[cfg(feature = "runtime")]
#[test]
fn initial_hero_is_trending_and_never_personal_even_if_only_personal_rows_exist() {
    let c = |id| {
        simkl::card(
            &json!({"title":"Title","ids":{"simkl":id},"fanart":"12/345"}),
            "movie",
        )
        .unwrap()
    };
    let row = |id: &str, provider: &str, card: Card| HomeRow {
        id: id.into(),
        title: id.into(),
        provider_id: provider.into(),
        state: RowState::Ready,
        cards: vec![card],
        next_cursor: None,
        message: None,
    };
    let rows = vec![
        row("continue", "library", c(1)),
        row("other", "stremio", c(2)),
        row("simkl:movie:trending", "simkl", c(3)),
    ];
    assert_eq!(
        viptv_engine::home::select_hero(&rows).unwrap().identity.id,
        "simkl:movie:3"
    );
    assert!(viptv_engine::home::select_hero(&rows[..1]).is_none());
}
#[test]
fn anime_episodes_use_only_provider_asserted_tvdb_coordinates() {
    let parent=simkl::card(&json!({"title":"Naruto","type":"anime","anime_type":"tv","ids":{"simkl":1,"imdb":"tt0409591"}}),"anime").unwrap();
    let cards=simkl::episodes(&json!([{"title":"Mapped","type":"episode","aired":true,"episode":200,"tvdb":{"season":5,"episode":20},"ids":{"simkl_id":2}},{"title":"Unmapped","type":"episode","episode":201,"ids":{"simkl_id":3}},{"title":"Not aired","type":"episode","aired":false,"tvdb":{"season":5,"episode":21},"ids":{"simkl_id":4}}]),&parent).unwrap();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].identity.id, "simkl:anime:1:5:20");
    assert_eq!(
        simkl::source_identity(&parent, Some(&cards[0])).unwrap().id,
        "tt0409591:5:20"
    );
}
#[test]
fn continue_episode_uses_parent_title_and_logo_without_changing_exact_episode_or_resume() {
    let parent = simkl::card(
        &json!({"title":"Parent Show","ids":{"simkl":42},"overview":"Parent overview"}),
        "tv",
    )
    .unwrap();
    let mut episode = simkl::episodes(
        &json!([{"title":"Pilot","season":1,"episode":1,"ids":{"simkl":43}}]),
        &parent,
    )
    .unwrap()
    .remove(0);
    episode.progress = Some(0.4);
    episode.subtitle = Some("20m left".into());
    let original = episode.identity.clone();
    let enriched = viptv_engine::presentation::enrich_card(&episode, &parent);
    assert_eq!(enriched.title, "Parent Show");
    assert_eq!(enriched.identity, original);
    assert_eq!(enriched.progress, Some(0.4));
    assert_eq!(enriched.subtitle.as_deref(), Some("20m left"));
    assert_eq!(enriched.description.as_deref(), Some("Parent overview"));
}
