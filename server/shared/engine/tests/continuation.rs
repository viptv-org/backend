use viptv_engine::{
    continuation::*,
    identity::{EpisodeCoordinates, MediaIdentity},
    presentation::Card,
};
fn episode(index: u32, season: u32, number: u32) -> EpisodeAvailability {
    let parent = MediaIdentity {
        id: "opaque parent".into(),
        kind: "series".into(),
        ids: vec![],
        episode: None,
        edition: None,
    };
    let mut card=viptv_engine::stremio::cards(&serde_json::json!({"metas":[{"id":format!("unparsed-child-{index}"),"type":"series","name":"Episode"}]}).to_string(),"metadata").unwrap().remove(0);
    card.identity.episode = Some(EpisodeCoordinates {
        order: "aired".into(),
        season: Some(season),
        episode: number,
    });
    card.parent_identity = Some(parent);
    card.progress = Some(0.5);
    card.added_at_millis = Some(1);
    EpisodeAvailability {
        card,
        released_at_millis: Some(10),
        aired: None,
    }
}
#[test]
fn next_uses_complete_large_sequence_exact_ids_and_preserves_specials_family() {
    let entries = (1..=1242)
        .map(|n| {
            episode(
                n,
                if n <= 1000 { 1 } else { 2 },
                if n <= 1000 { n } else { n - 1000 },
            )
        })
        .collect::<Vec<_>>();
    let next = next_episode(&entries[999].card.identity, &entries, 20);
    assert_eq!(next.status, ContinuationStatus::Next);
    let card = next.item.unwrap();
    assert_eq!(card.identity.id, "unparsed-child-1001");
    assert_eq!(card.identity.episode.unwrap().season, Some(2));
    assert!(card.progress.is_none() && card.added_at_millis.is_none());
    assert_eq!(
        next_episode(&entries[1241].card.identity, &entries, 20).status,
        ContinuationStatus::CaughtUp
    );
    let special = episode(0, 0, 1);
    let all = vec![special.clone(), entries[0].clone()];
    assert_eq!(
        next_episode(&special.card.identity, &all, 20).status,
        ContinuationStatus::CaughtUp
    );
}
#[test]
fn unknown_airing_conflicting_coordinates_parent_and_duplicate_ids_fail_closed() {
    let first = episode(1, 1, 1);
    let mut second = episode(2, 1, 2);
    second.released_at_millis = Some(100);
    assert_eq!(
        next_episode(&first.card.identity, &[first.clone(), second.clone()], 20).status,
        ContinuationStatus::Upcoming
    );
    second.released_at_millis = None;
    assert_eq!(
        next_episode(&first.card.identity, &[first.clone(), second.clone()], 20).status,
        ContinuationStatus::Unavailable
    );
    second.aired = Some(true);
    assert_eq!(
        next_episode(&first.card.identity, &[first.clone(), second.clone()], 20).status,
        ContinuationStatus::Next
    );
    let mut forged = first.card.identity.clone();
    forged.episode.as_mut().unwrap().episode = 4;
    assert_eq!(
        next_episode(&forged, &[first.clone(), second.clone()], 20).status,
        ContinuationStatus::Unavailable
    );
    let mut duplicate = second.clone();
    duplicate.card.identity.id = "another opaque child".into();
    assert_eq!(
        next_episode(
            &first.card.identity,
            &[first.clone(), second.clone(), duplicate],
            20
        )
        .status,
        ContinuationStatus::Unavailable
    );
    second.card.parent_identity.as_mut().unwrap().id = "foreign parent".into();
    assert_eq!(
        next_episode(&first.card.identity, &[first.clone(), second], 20).status,
        ContinuationStatus::Unavailable
    );
}
#[test]
fn continuation_intents_are_typed_and_reject_unrecognized_control_fields() {
    let identity = episode(1, 1, 1).card.identity;
    assert!(validate_identity(&identity).is_ok());
    let value = serde_json::json!({"identity":identity,"parent_identity":null,"url":"https://untrusted.invalid"});
    assert!(serde_json::from_value::<NextEpisodeRequest>(value).is_err());
    let mut bad = identity;
    bad.id = "wrong\nidentity".into();
    assert!(validate_identity(&bad).is_err());
}

#[test]
fn home_browse_actions_are_explicit_and_old_rows_decode_without_them(){
 use viptv_engine::{home_browse::*,presentation::HomeRow};
 let old=serde_json::json!({"id":"opaque-row:do-not-parse","title":"Shelf","provider_id":"metadata","state":"Ready","cards":[],"next_cursor":null,"message":null});let row:HomeRow=serde_json::from_value(old).unwrap();assert!(row.browse.is_none());
 let action=HomeBrowseAction::discover("series","catalog:with/opaque-id",Some(("connection_id","opaque connection".into())));assert_eq!(action.target,HomeBrowseTarget::Discover);assert_eq!(action.query[1].values[0],"catalog:with/opaque-id");assert_eq!(action.query[2].id,"connection_id");
 let library=HomeBrowseAction::library();assert_eq!(library.target,HomeBrowseTarget::Library);assert!(library.query.iter().any(|value|value.id=="source"&&value.values==vec!["favorites"]));assert_eq!(HomeBrowseAction::continuation().target,HomeBrowseTarget::Continue);
}
