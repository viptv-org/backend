use viptv_engine::{
    Error, identity::MediaIdentity, presentation::Card, profile::ProfileConfiguration, store::Store,
};

fn card() -> Card {
    Card {
        metadata_origin: None,
        identity: MediaIdentity {
            id: "fixture-film".into(),
            kind: "movie".into(),
            ids: vec![],
            episode: None,
            edition: None,
        },
        provider_id: "addon:one".into(),
        title: "Fixture".into(),
        subtitle: None,
        description: None,
        poster: Some("https://images.example/signed.jpg?token=private".into()),
        landscape: Some("https://images.example/public.jpg".into()),
        logo: None,
        progress: None,
        badges: vec![],
        parent_identity: None,
        added_at_millis: None,
        category: None,
        metadata: None,
    }
}

#[test]
fn history_is_profile_scoped_completion_survives_rewatch_and_private_art_is_not_stored() {
    let mut store = Store::memory().unwrap();
    let a = ProfileConfiguration::local("local".into(), "alice".into(), "Alice".into());
    let b = ProfileConfiguration::local("local".into(), "bob".into(), "Bob".into());
    store.save_profile(&a, None).unwrap();
    store.save_profile(&b, None).unwrap();
    store
        .record_playback(&a.scope, &card(), 30_000, 100_000, 1000)
        .unwrap();
    let first = store
        .playback(&a.scope, "movie", "fixture-film")
        .unwrap()
        .unwrap();
    assert!(first.can_resume());
    let mut short = first.clone();
    short.duration_millis = 5_000;
    short.position_millis = 2_000;
    assert_eq!(short.card().subtitle.as_deref(), Some("3s left"));
    assert_eq!(first.poster, None);
    assert_eq!(
        first.landscape.as_deref(),
        Some("https://images.example/public.jpg")
    );
    assert!(store.history(&b.scope, 10).unwrap().is_empty());
    store
        .record_playback(&b.scope, &card(), 20_000, 100_000, 900)
        .unwrap();
    store.clear_history(&a.scope).unwrap();
    assert!(store.history(&a.scope, 10).unwrap().is_empty());
    assert_eq!(store.history(&b.scope, 10).unwrap().len(), 1);
    store
        .record_playback(&a.scope, &card(), 97_000, 100_000, 2000)
        .unwrap();
    let completed = store.history(&a.scope, 10).unwrap().pop().unwrap();
    assert!(!completed.can_resume());
    assert_eq!(completed.completed_at_millis, Some(2000));
    store
        .record_playback(&a.scope, &card(), 10_000, 100_000, 3000)
        .unwrap();
    let rewatch = store
        .playback(&a.scope, "movie", "fixture-film")
        .unwrap()
        .unwrap();
    assert!(rewatch.can_resume());
    assert_eq!(rewatch.completed_at_millis, Some(2000));
    assert_eq!(
        store.record_playback(&b.scope, &card(), 170_000, 100_000, 3000),
        Err(Error::InvalidQuery)
    );
    assert_eq!(store.history(&b.scope, 10).unwrap().len(), 1);
}

#[test]
fn opaque_episode_parent_survives_local_history_and_legacy_json_defaults() {
    let mut store = Store::memory().unwrap();
    let profile = ProfileConfiguration::local("local".into(), "alice".into(), "Alice".into());
    store.save_profile(&profile, None).unwrap();
    let mut episode = card();
    let mut parent = episode.identity.clone();
    parent.id = "opaque-parent".into();
    parent.kind = "series".into();
    episode.identity.id = "opaque-episode".into();
    episode.identity.kind = "series".into();
    episode.identity.episode = Some(viptv_engine::identity::EpisodeCoordinates {
        order: "aired".into(),
        season: Some(1),
        episode: 2,
    });
    episode.parent_identity = Some(parent.clone());
    store
        .record_playback(&profile.scope, &episode, 10_000, 100_000, 1000)
        .unwrap();
    let record = store.history(&profile.scope, 10).unwrap().pop().unwrap();
    assert_eq!(record.parent_identity, Some(parent.clone()));
    assert_eq!(record.card().parent_identity, Some(parent));
    let mut old = serde_json::to_value(record).unwrap();
    old.as_object_mut().unwrap().remove("parent_identity");
    assert!(
        serde_json::from_value::<viptv_engine::playback::PlaybackRecord>(old)
            .unwrap()
            .parent_identity
            .is_none()
    );
    let mut old = serde_json::to_value(episode).unwrap();
    old.as_object_mut().unwrap().remove("parent_identity");
    assert!(
        serde_json::from_value::<Card>(old)
            .unwrap()
            .parent_identity
            .is_none()
    );
}
#[test]
fn resume_caption_uses_typed_episode_coordinates_before_remaining_time_and_never_invents_season() {
    use viptv_engine::{identity::EpisodeCoordinates, playback::resume_caption};
    let mut identity = card().identity;
    assert_eq!(resume_caption(&identity, 22 * 60000), "22m left");
    identity.kind = "series".into();
    identity.episode = Some(EpisodeCoordinates {
        order: "aired".into(),
        season: Some(1),
        episode: 1,
    });
    assert_eq!(resume_caption(&identity, 22 * 60000), "S1 E1 · 22m left");
    identity.episode.as_mut().unwrap().season = None;
    assert_eq!(resume_caption(&identity, 3000), "E1 · 3s left");
    identity.kind = "movie".into();
    assert_eq!(resume_caption(&identity, 22 * 60000), "22m left");
}
