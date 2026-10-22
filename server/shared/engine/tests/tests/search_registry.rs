use viptv_engine::{
    Error,
    capabilities::{Capability, ExecutionMode},
    profile::{IntegrationConnection, ProfileAssignment, ProfileConfiguration},
    registry::Registry,
};

fn connection(id: &str, plugin: &str) -> IntegrationConnection {
    IntegrationConnection {
        id: id.into(),
        plugin_id: plugin.into(),
        account_id: "account".into(),
        private_profile_id: None,
        name: id.into(),
        enabled: true,
        revision: 0,
        capabilities: vec![
            Capability::Search,
            Capability::Metadata,
            Capability::Discovery,
        ],
    }
}
fn fixture() -> (ProfileConfiguration, Vec<IntegrationConnection>) {
    let mut profile =
        ProfileConfiguration::local("account".into(), "profile".into(), "Profile".into());
    profile.mode = ExecutionMode::Server;
    profile.browsing_plugin = "simkl".into();
    let connections = vec![
        connection("simkl:1", "simkl"),
        connection("addon:1", "stremio"),
        connection("addon:2", "stremio"),
    ];
    profile.assignments = connections
        .iter()
        .map(|c| ProfileAssignment {
            connection_id: c.id.clone(),
            enabled: c.capabilities.clone(),
        })
        .collect();
    (profile, connections)
}

#[test]
fn search_uses_all_assigned_plugins_while_metadata_and_discovery_keep_default() {
    let (mut profile, connections) = fixture();
    let registry = Registry::builtin();
    for default in ["simkl", "stremio"] {
        profile.browsing_plugin = default.into();
        assert_eq!(
            registry
                .connections_for(&profile, &connections, Capability::Search)
                .unwrap()
                .iter()
                .map(|c| c.id.as_str())
                .collect::<Vec<_>>(),
            vec!["simkl:1", "addon:1", "addon:2"]
        );
        for capability in [Capability::Metadata, Capability::Discovery] {
            let selected = registry
                .connections_for(&profile, &connections, capability)
                .unwrap();
            assert!(selected.iter().all(|c| c.plugin_id == default));
            assert_eq!(selected.len(), if default == "simkl" { 1 } else { 2 });
        }
    }
}

#[test]
fn search_still_requires_enabled_profile_assignment_and_capability() {
    let (mut profile, mut connections) = fixture();
    let registry = Registry::builtin();
    connections[1].enabled = false;
    profile.assignments[2]
        .enabled
        .retain(|c| *c != Capability::Search);
    assert_eq!(
        registry
            .connections_for(&profile, &connections, Capability::Search)
            .unwrap()
            .iter()
            .map(|c| c.id.as_str())
            .collect::<Vec<_>>(),
        vec!["simkl:1"]
    );
    profile.assignments.remove(0);
    assert!(
        registry
            .connections_for(&profile, &connections, Capability::Search)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn search_does_not_cross_account_profile_or_execution_mode() {
    let (profile, connections) = fixture();
    let registry = Registry::builtin();
    for (account, private) in [("other", None), ("account", Some("other-profile"))] {
        let mut scoped = connections.clone();
        scoped[1].account_id = account.into();
        scoped[1].private_profile_id = private.map(str::to_owned);
        assert_eq!(
            registry
                .connections_for(&profile, &scoped, Capability::Search)
                .err(),
            Some(Error::Forbidden)
        );
    }
    let mut local = profile;
    local.mode = ExecutionMode::Local;
    local.browsing_plugin = "stremio".into();
    assert_eq!(
        registry
            .connections_for(&local, &connections, Capability::Search)
            .err(),
        Some(Error::Unavailable)
    );
}
