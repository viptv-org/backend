use viptv_engine::{Error, capabilities::*, identity::*, profile::*, query::*, registry::Registry};

fn connection(id: &str, plugin: &str, private: Option<&str>) -> IntegrationConnection {
    IntegrationConnection {
        id: id.into(),
        plugin_id: plugin.into(),
        account_id: "device".into(),
        private_profile_id: private.map(str::to_owned),
        name: id.into(),
        enabled: true,
        revision: 0,
        capabilities: vec![
            Capability::Metadata,
            Capability::Discovery,
            Capability::Sources,
        ],
    }
}

#[test]
fn multiple_addons_one_browsing_integration_and_private_profiles() {
    let r = Registry::builtin();
    let mut p = ProfileConfiguration::local("device".into(), "alice".into(), "Alice".into());
    let mut connections = vec![
        connection("a", "stremio", None),
        connection("b", "stremio", Some("alice")),
    ];
    p.assignments = connections
        .iter()
        .map(|c| ProfileAssignment {
            connection_id: c.id.clone(),
            enabled: vec![Capability::Metadata, Capability::Sources],
        })
        .collect();
    assert_eq!(
        r.connections_for(&p, &connections, Capability::Metadata)
            .unwrap()
            .len(),
        2
    );
    connections[1].private_profile_id = Some("bob".into());
    assert_eq!(r.validate(&p, &connections), Err(Error::Forbidden));
}

#[test]
fn simkl_never_becomes_a_local_provider() {
    let mut p = ProfileConfiguration::local("device".into(), "alice".into(), "Alice".into());
    p.browsing_plugin = "simkl".into();
    assert_eq!(
        Registry::builtin().validate(&p, &[]),
        Err(Error::Unavailable)
    );
}

#[test]
fn title_matches_do_not_identify_episodes_and_conflicts_remain_visible() {
    let ep = EpisodeCoordinates {
        order: "aired".into(),
        season: Some(1),
        episode: 2,
    };
    let i = MediaIdentity {
        id: "x".into(),
        kind: "series".into(),
        ids: vec![],
        episode: Some(ep.clone()),
        edition: None,
    };
    let mut e = MappingEvidence {
        source: "test".into(),
        target: ExternalId {
            namespace: "imdb".into(),
            value: "tt1".into(),
        },
        confidence: MappingConfidence::Candidate,
        episode: Some(ep),
        version: "1".into(),
    };
    assert_eq!(resolve(&i, "imdb", &[e.clone()]), Err(Error::Unresolved));
    e.confidence = MappingConfidence::Explicit;
    e.episode = None;
    assert_eq!(resolve(&i, "imdb", &[e.clone()]), Err(Error::Unresolved));
    e.episode = i.episode.clone();
    assert_eq!(resolve(&i, "imdb", &[e.clone()]).unwrap().value, "tt1");
    let mut conflicting = e.clone();
    conflicting.target.value = "tt2".into();
    assert_eq!(
        resolve(&i, "imdb", &[e, conflicting]),
        Err(Error::Unresolved)
    );
}

#[test]
fn filters_are_validated_instead_of_silently_ignored() {
    let c = QueryControl {
        id: "year".into(),
        label: "Year".into(),
        kind: ControlKind::Number,
        required: false,
        choices: vec![],
        minimum: Some(1900.),
        maximum: Some(2100.),
    };
    let query = |id: &str, v: &str| QueryValue {
        id: id.into(),
        values: vec![v.into()],
    };
    assert!(validate(&[c.clone()], &[query("year", "2026")]).is_ok());
    for q in [
        query("rating", "8"),
        query("year", "NaN"),
        query("year", "1800"),
    ] {
        assert_eq!(validate(&[c.clone()], &[q]), Err(Error::InvalidQuery));
    }
}

#[cfg(feature = "runtime")]
#[test]
fn sqlite_isolates_profiles_rejects_stale_writes_and_deduplicates_events() {
    use viptv_engine::store::Store;
    let mut store = Store::memory().unwrap();
    let mut p = ProfileConfiguration::local("device".into(), "alice".into(), "Alice".into());
    store.save_profile(&p, None).unwrap();
    p.scope.revision = 1;
    store.save_profile(&p, Some(0)).unwrap();
    assert_eq!(store.save_profile(&p, Some(0)), Err(Error::Conflict));
    assert!(store.observe(&p.scope, "event", "{}").unwrap());
    assert!(!store.observe(&p.scope, "event", "{}").unwrap());
    p.scope.profile_id = "bob".into();
    assert!(store.profile(&p.scope).unwrap().is_none());
    assert_eq!(
        store.observe(&p.scope, "event", "{}"),
        Err(Error::Forbidden)
    );
}

#[cfg(feature = "runtime")]
#[tokio::test(start_paused = true)]
async fn home_deadline_does_not_wait_for_slow_rows_or_leak_provider_errors() {
    use std::time::Duration;
    use viptv_engine::{home::*, presentation::RowState};
    let p = ProfileConfiguration::local("device".into(), "alice".into(), "Alice".into());
    let requests = vec![
        RowRequest {
            id: "fast".into(),
            title: "Fast".into(),
            provider_id: "a".into(),
            load: Box::pin(async { Ok(vec![]) }),
        },
        RowRequest {
            id: "slow".into(),
            title: "Slow".into(),
            provider_id: "b".into(),
            load: Box::pin(async {
                tokio::time::sleep(Duration::from_secs(20)).await;
                Ok(vec![])
            }),
        },
        RowRequest {
            id: "fail".into(),
            title: "Failed".into(),
            provider_id: "c".into(),
            load: Box::pin(async { Err("secret upstream URL".into()) }),
        },
    ];
    let result = assemble(p.scope, requests, Duration::from_secs(2)).await;
    assert!(!result.complete);
    assert_eq!(result.rows[0].state, RowState::Empty);
    assert_eq!(result.rows[1].state, RowState::Pending);
    assert_eq!(result.rows[2].state, RowState::Failed);
    assert!(!serde_json::to_string(&result).unwrap().contains("secret"));
}
