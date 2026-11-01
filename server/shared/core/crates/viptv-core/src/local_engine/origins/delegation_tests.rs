use super::*;
use viptv_engine::{capabilities::Capability, presentation::HomeSnapshot};

fn split_fixture() -> Fixture {
    let mut f = fixture();
    f.engine.remove_addon(f.providers[1].clone()).unwrap();
    let installed: Value = serde_json::from_str(&f.engine.install_addon("default".into(), "https://search.example/manifest.json".into(),
        json!({"id":"search-only","name":"Search only","version":"1","resources":["catalog"],"types":["movie","series"],"catalogs":[{"id":"search","type":"movie","extra":[{"name":"search"}]}]}).to_string()).unwrap()).unwrap();
    f.providers[1] = installed["id"].as_str().unwrap().into();
    let mut store = f.engine.store.lock().unwrap();
    let mut selected = store.profile(&local_scope("default")).unwrap().unwrap();
    let previous = selected.scope.revision;
    selected
        .assignments
        .iter_mut()
        .find(|a| a.connection_id == f.providers[0])
        .unwrap()
        .enabled
        .retain(|cap| {
            matches!(
                cap,
                Capability::Metadata | Capability::Sources | Capability::Subtitles
            )
        });
    selected.scope.revision += 1;
    store.save_profile(&selected, Some(previous)).unwrap();
    drop(store);
    f
}
fn public_search_card(f: &Fixture) -> Card {
    let plan: Value = serde_json::from_str(
        &f.engine
            .plan_search("default".into(), "public".into())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(plan["requests"].as_array().unwrap().len(), 1);
    let snapshot: HomeSnapshot = serde_json::from_str(&f.engine.project_home(plan["id"].as_str().unwrap().into(),
        json!([{"id":plan["requests"][0]["id"],"body":json!({"metas":[{"id":"tt1111111","type":"movie","name":"Public search"}]}).to_string()}]).to_string()).unwrap()).unwrap();
    let card = snapshot.rows[0].cards[0].clone();
    assert_eq!(card.provider_id, f.providers[1]);
    card
}
fn plan(f: &Fixture, card: &Card) -> Result<Value, CoreError> {
    let target = card.parent_identity.as_ref().unwrap_or(&card.identity);
    f.engine
        .plan_detail_for_origin(
            "default".into(),
            target.kind.clone(),
            target.id.clone(),
            serde_json::to_string(card).unwrap(),
        )
        .map(|raw| serde_json::from_str(&raw).unwrap())
}
fn project(f: &Fixture, plan: &Value, body: &Value) -> Result<DetailSnapshot, CoreError> {
    f.engine
        .project_detail_page(
            plan["operation"].as_str().unwrap().into(),
            body.to_string(),
            0,
            24,
        )
        .map(|raw| serde_json::from_str(&raw).unwrap())
}
fn movie() -> Value {
    json!({"meta":{"id":"tt1111111","type":"movie","name":"Verified","tmdb_id":42}})
}
fn recreate(f: &Fixture, id: &str) {
    let connection = f
        .engine
        .store
        .lock()
        .unwrap()
        .connections("local")
        .unwrap()
        .into_iter()
        .find(|c| c.id == id)
        .unwrap();
    let addon = f.engine.addon(id).unwrap();
    f.engine.remove_addon(id.into()).unwrap();
    f.engine
        .secrets
        .put(
            format!("{}:{id}", f.engine.namespace),
            serde_json::to_string(&addon).unwrap(),
        )
        .unwrap();
    f.engine
        .store
        .lock()
        .unwrap()
        .install("default", &connection)
        .unwrap();
}

#[test]
fn canonical_search_only_issuer_delegates_to_metadata_and_returns_actual_producer_origin() {
    let f = split_fixture();
    let card = public_search_card(&f);
    assert!(
        f.engine
            .detail_page_for_origin(
                "default".into(),
                "movie".into(),
                card.identity.id.clone(),
                serde_json::to_string(&card).unwrap(),
                0,
                24
            )
            .unwrap()
            .is_none()
    );
    let planned = plan(&f, &card).unwrap();
    assert!(
        planned["url"]
            .as_str()
            .unwrap()
            .starts_with("https://first.example/meta/")
    );
    let actual = project(&f, &planned, &movie()).unwrap().item;
    assert_eq!(actual.identity.id, card.identity.id);
    assert_eq!(actual.provider_id, f.providers[0]);
    assert_ne!(actual.metadata_origin, card.metadata_origin);
    assert!(f.engine.verified_source_card("default", &actual).is_ok());
    let cached: DetailSnapshot = serde_json::from_str(
        &f.engine
            .detail_page_for_origin(
                "default".into(),
                "movie".into(),
                card.identity.id.clone(),
                serde_json::to_string(&card).unwrap(),
                0,
                24,
            )
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(cached.item.metadata_origin, actual.metadata_origin);
    let mut wrong_tmdb = card.clone();
    wrong_tmdb
        .identity
        .ids
        .push(viptv_engine::identity::ExternalId {
            namespace: "tmdb".into(),
            value: "99".into(),
        });
    let mut wrong_edition = card.clone();
    wrong_edition.identity.edition = Some("Director cut".into());
    for asserted in [wrong_tmdb, wrong_edition] {
        assert!(
            project(&f, &plan(&f, &asserted).unwrap(), &movie()).is_err(),
            "All expected claims still constrain delegated metadata"
        );
    }
}

#[test]
fn disabled_or_reinstalled_issuer_and_delegate_reject_late_delegated_results() {
    for issuer in [true, false] {
        for reinstall in [false, true] {
            let f = split_fixture();
            let card = public_search_card(&f);
            let cached = project(&f, &plan(&f, &card).unwrap(), &movie()).unwrap();
            let planned = plan(&f, &card).unwrap();
            let affected = f.providers[if issuer { 1 } else { 0 }].clone();
            if reinstall {
                recreate(&f, &affected);
            } else {
                f.engine
                    .set_addon_enabled("default".into(), affected, false)
                    .unwrap();
            }
            assert!(project(&f, &planned, &movie()).is_err());
            let stale_cache = f.engine.detail_page_for_origin(
                "default".into(),
                "movie".into(),
                card.identity.id.clone(),
                serde_json::to_string(&card).unwrap(),
                0,
                24,
            );
            if issuer {
                assert!(stale_cache.is_err());
            } else {
                assert!(stale_cache.unwrap().is_none());
            }
            assert!(
                f.engine
                    .verified_source_card("default", &cached.item)
                    .is_err()
            );
            if issuer || !reinstall {
                assert!(plan(&f, &card).is_err());
            } else {
                assert!(project(&f, &plan(&f, &card).unwrap(), &movie()).is_ok());
            }
            if issuer && reinstall {
                let fresh = hint(&f.engine, &f.providers[1], "movie", "tt1111111");
                assert_ne!(fresh.metadata_origin, card.metadata_origin);
                assert!(project(&f, &plan(&f, &fresh).unwrap(), &movie()).is_ok());
            }
        }
    }
}

#[test]
fn search_only_opaque_or_noncanonical_episode_never_delegates_even_with_imdb_hint() {
    let f = split_fixture();
    for (kind, id) in [("movie", "private-title"), ("series", "tt1111111:01:1")] {
        let mut card = hint(&f.engine, &f.providers[1], kind, id);
        card.identity.ids.push(viptv_engine::identity::ExternalId {
            namespace: "imdb".into(),
            value: "tt1111111".into(),
        });
        assert!(plan(&f, &card).is_err());
        assert!(
            f.engine
                .detail_page_for_origin(
                    "default".into(),
                    kind.into(),
                    id.into(),
                    serde_json::to_string(&card).unwrap(),
                    0,
                    24
                )
                .is_err()
        );
    }
}

#[test]
fn canonical_episode_beyond_page_zero_is_verified_from_full_delegated_parent_document() {
    let f = split_fixture();
    let mut expected = hint(&f.engine, &f.providers[1], "series", "tt1111111:1:1499");
    expected.identity.episode = Some(EpisodeCoordinates {
        order: "aired".into(),
        season: Some(1),
        episode: 1499,
    });
    expected.parent_identity = Some(MediaIdentity {
        id: "tt1111111".into(),
        kind: "series".into(),
        ids: vec![viptv_engine::identity::ExternalId {
            namespace: "tmdb".into(),
            value: "42".into(),
        }],
        episode: None,
        edition: None,
    });
    let body = |bad: bool| json!({"meta":{"id":"tt1111111","type":"series","name":"Public series","tmdb_id":42,"videos":(1..=1500).map(|episode| json!({"id":format!("tt1111111:1:{episode}"),"title":format!("Episode {episode}"),"season":1,"episode":if bad && episode==1499 {1500} else {episode}})).collect::<Vec<_>>()}});
    let actual = project(&f, &plan(&f, &expected).unwrap(), &body(false)).unwrap();
    assert_eq!(actual.episodes.len(), 24);
    assert!(
        !actual
            .episodes
            .iter()
            .any(|e| e.identity.id == expected.identity.id)
    );
    let mut upgraded = expected.clone();
    upgraded.metadata_origin = actual.item.metadata_origin.clone();
    upgraded.provider_id = actual.item.provider_id.clone();
    let (_, proof) = f.engine.verified_source_card("default", &upgraded).unwrap();
    assert_eq!(proof.identity.episode.as_ref().unwrap().episode, 1499);
    assert_eq!(
        proof
            .parent_identity
            .as_ref()
            .unwrap()
            .ids
            .iter()
            .find(|id| id.namespace == "tmdb")
            .unwrap()
            .value,
        "42"
    );
    assert!(
        f.engine
            .detail_page_for_origin(
                "default".into(),
                "series".into(),
                "tt1111111".into(),
                serde_json::to_string(&expected).unwrap(),
                0,
                24
            )
            .unwrap()
            .is_some()
    );
    assert!(project(&f, &plan(&f, &expected).unwrap(), &body(true)).is_err());
    assert!(f.engine.verified_source_card("default", &upgraded).is_err());
}
