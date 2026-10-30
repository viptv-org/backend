use super::*;
fn lease(profile: i64, session: &str) -> ResourceLease {
    ResourceLease {
        policy_revision: 0,
        principal: auth::Principal::Account {
            account_id: 1,
            profile_id: Some(profile),
            role: "owner".into(),
            session_id: Some(session.into()),
        },
        session_id: Some(session.into()),
    }
}
fn selection(owner: &ResourceLease, now: Instant) -> Selection {
    Selection {
        metadata_proof:None,
        scope: viptv_engine::profile::Scope {
            connection_id: "server".into(),
            account_id: "1".into(),
            profile_id: "1".into(),
            revision: 0,
        },
        lease: owner.clone(),
        addon_revision: "revision".into(),
        kind: "movie".into(),
        media_id: "opaque-title".into(),
        url: "https://provider.invalid/caption".into(),
        language: "eng".into(),
        label: "English".into(),
        created: now,
        content_vtt: None,
        retained_until: None,
    }
}
#[test]
fn active_heartbeat_keeps_exact_id_and_cached_text_for_a_long_movie_then_expires() {
    let store = Selections::default();
    let owner = lease(1, "s1");
    let now = Instant::now();
    store
        .insert(&owner, vec![("selected".into(), selection(&owner, now))])
        .unwrap();
    store
        .retain_at(
            "selected",
            &owner,
            Arc::from("WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nCue\n"),
            now,
        )
        .unwrap();
    for seconds in (0..=4 * 3600).step_by(60) {
        store
            .keep_alive_at(
                &["selected".into()],
                &owner,
                now + Duration::from_secs(seconds),
            )
            .unwrap();
    }
    let long = now + Duration::from_secs(4 * 3600);
    assert!(store.get_at("selected", &lease(2, "s1"), long).is_err());
    assert!(store
        .get_at("selected", &lease(1, "foreign"), long)
        .is_err());
    store
        .insert(
            &owner,
            vec![("new-selection".into(), selection(&owner, now))],
        )
        .unwrap();
    let cached = store
        .get_at("selected", &owner, long + Duration::from_secs(150))
        .unwrap();
    assert!(cached.content_vtt.unwrap().contains("Cue"));
    assert!(store
        .get_at("selected", &owner, long + Duration::from_secs(181))
        .is_err());
    assert!(store.get_at("new-selection", &owner, long).is_err());
}
#[test]
fn cache_bounds_immutable_text_and_unbound_candidate_expiry_are_enforced() {
    let store = Selections::default();
    let owner = lease(1, "s1");
    let now = Instant::now();
    store
        .insert(
            &owner,
            (0..17)
                .map(|n| (format!("caption-{n}"), selection(&owner, now)))
                .collect(),
        )
        .unwrap();
    let text: Arc<str> = Arc::from("x".repeat(2 * 1024 * 1024));
    for n in 0..16 {
        store
            .retain_at(&format!("caption-{n}"), &owner, text.clone(), now)
            .unwrap();
    }
    assert_eq!(
        store
            .retain_at("caption-16", &owner, text.clone(), now)
            .unwrap_err()
            .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        store
            .retain_at("caption-0", &owner, Arc::from("changed"), now)
            .unwrap_err()
            .0,
        StatusCode::CONFLICT
    );
    assert!(store
        .retain_at("caption-0", &lease(2, "s1"), text, now)
        .is_err());
    assert!(store
        .get_at("caption-16", &owner, now + Duration::from_secs(601))
        .is_err());
}
