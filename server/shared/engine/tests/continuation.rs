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

fn released(index: u32, season: u32, number: u32, at: Option<u64>) -> EpisodeAvailability {
    let mut entry = episode(index, season, number);
    entry.released_at_millis = at;
    entry
}
fn never_watched(_: &Card) -> bool {
    false
}

#[test]
fn a_release_after_the_last_finished_episode_is_new_and_later_releases_only_count() {
    // Owner flow: finish E1 at t=1000. E2 airs at 2000, E3 at 3000.
    let mut show = vec![
        released(1, 1, 1, Some(500)),
        released(2, 1, 2, Some(2000)),
        released(3, 1, 3, Some(3000)),
    ];
    let current = show[0].card.identity.clone();
    assert!(new_episodes(&current, 1000, &show, never_watched, 1500).is_none());
    let first = new_episodes(&current, 1000, &show, never_watched, 2500).unwrap();
    assert_eq!(first.count, 1);
    assert_eq!(first.next.identity.id, "unparsed-child-2");
    assert!(first.next.progress.is_none() && first.next.added_at_millis.is_none());
    assert_eq!(first.first_released_at_millis, 2000);
    assert_eq!(new_episodes_badge(first.count), "New episode");
    // A later release only increments the count; the promotion time stays E2's.
    let second = new_episodes(&current, 1000, &show, never_watched, 3500).unwrap();
    assert_eq!(second.count, 2);
    assert_eq!(second.first_released_at_millis, 2000);
    assert_eq!(second.released_at_millis, vec![2000, 3000]);
    assert_eq!(second.next.identity.id, "unparsed-child-2");
    assert_eq!(new_episodes_badge(second.count), "+2 new episodes");
    assert!(is_new_episodes_badge("New episode") && is_new_episodes_badge("+2 new episodes"));
    assert!(!is_new_episodes_badge("Watched") && !is_new_episodes_badge("+x new episodes"));
    // Episodes already finished out of order neither count nor become the target.
    let skipped = new_episodes(
        &current,
        1000,
        &show,
        |card| card.identity.id == "unparsed-child-2",
        3500,
    )
    .unwrap();
    assert_eq!(
        (skipped.count, skipped.next.identity.id.as_str()),
        (1, "unparsed-child-3")
    );
    // Finishing E2 after both releases leaves a backlog, not a new episode.
    let after = show[1].card.identity.clone();
    assert!(new_episodes(&after, 3600, &show, never_watched, 4000).is_none());
    show.push(released(4, 1, 4, Some(5000)));
    assert!(new_episodes(&after, 3600, &show, never_watched, 6000).is_none());
}

#[test]
fn a_backlog_that_was_already_available_is_not_new() {
    let show = vec![
        released(1, 1, 1, Some(10)),
        released(2, 1, 2, Some(20)),
        released(3, 1, 3, Some(5000)),
    ];
    // E2 had aired before E1 was finished: still behind when E3 airs, so no promotion.
    assert!(new_episodes(&show[0].card.identity, 1000, &show, never_watched, 6000).is_none());
}

#[test]
fn a_new_season_counts_but_specials_other_orders_and_unproven_releases_do_not() {
    let mut finale = released(12, 1, 12, Some(10));
    finale.card.identity.id = "finale".into();
    let special = released(100, 0, 1, Some(2000));
    let mut absolute = released(101, 1, 13, Some(2000));
    absolute.card.identity.episode.as_mut().unwrap().order = "absolute".into();
    let undated = released(102, 2, 2, None);
    let mut future = released(103, 2, 3, Some(9000));
    let premiere = released(104, 2, 1, Some(3000));
    let show = vec![
        finale.clone(),
        special,
        absolute,
        undated,
        future.clone(),
        premiere,
    ];
    let result = new_episodes(&finale.card.identity, 1000, &show, never_watched, 4000).unwrap();
    assert_eq!(result.next.identity.id, "unparsed-child-104");
    assert_eq!(result.count, 1);
    // An episode that is only flagged as aired counts once its predecessor proves the release.
    let mut aired = show.clone();
    aired[3].aired = Some(true);
    assert_eq!(
        new_episodes(&finale.card.identity, 1000, &aired, never_watched, 4000)
            .unwrap()
            .count,
        2
    );
    // But it can never be the proof itself.
    let only_flagged = vec![finale.clone(), aired[3].clone()];
    assert!(
        new_episodes(
            &finale.card.identity,
            1000,
            &only_flagged,
            never_watched,
            4000
        )
        .is_none()
    );
    future.aired = Some(false);
    future.released_at_millis = Some(10);
    let announced = vec![finale.clone(), future];
    assert!(new_episodes(&finale.card.identity, 1000, &announced, never_watched, 4000).is_none());
}

#[test]
fn new_episode_detection_fails_closed_on_ambiguous_metadata() {
    let first = released(1, 1, 1, Some(10));
    let second = released(2, 1, 2, Some(2000));
    let mut duplicate = released(3, 1, 2, Some(2000));
    assert!(
        new_episodes(
            &first.card.identity,
            1000,
            &[first.clone(), second.clone(), duplicate.clone()],
            never_watched,
            3000
        )
        .is_none()
    );
    duplicate.card.identity.episode.as_mut().unwrap().episode = 3;
    duplicate.card.parent_identity.as_mut().unwrap().id = "foreign parent".into();
    assert!(
        new_episodes(
            &first.card.identity,
            1000,
            &[first.clone(), second.clone(), duplicate],
            never_watched,
            3000
        )
        .is_none()
    );
    let mut moved = first.card.identity.clone();
    moved.episode.as_mut().unwrap().episode = 5;
    assert!(
        new_episodes(
            &moved,
            1000,
            &[first.clone(), second.clone()],
            never_watched,
            3000
        )
        .is_none()
    );
    assert!(new_episodes(&first.card.identity, 1000, &[second], never_watched, 3000).is_none());
}
