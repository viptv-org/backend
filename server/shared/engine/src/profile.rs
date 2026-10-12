use crate::capabilities::{Capability, ExecutionMode};
use facet::Facet;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Facet)]
pub struct Scope {
    pub connection_id: String,
    pub account_id: String,
    pub profile_id: String,
    pub revision: u64,
}

/// Credentials live in a host vault, never in the public connection record.
#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
pub struct IntegrationConnection {
    pub id: String,
    pub plugin_id: String,
    pub account_id: String,
    pub private_profile_id: Option<String>,
    pub name: String,
    pub enabled: bool,
    pub revision: u64,
    pub capabilities: Vec<Capability>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
pub struct ProfileAssignment {
    pub connection_id: String,
    pub enabled: Vec<Capability>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
pub struct ProfileConfiguration {
    pub scope: Scope,
    pub mode: ExecutionMode,
    pub name: String,
    pub browsing_plugin: String,
    pub assignments: Vec<ProfileAssignment>,
}

impl ProfileConfiguration {
    pub fn local(connection_id: String, profile_id: String, name: String) -> Self {
        Self {
            scope: Scope {
                account_id: connection_id.clone(),
                connection_id,
                profile_id,
                revision: 0,
            },
            mode: ExecutionMode::Local,
            name,
            browsing_plugin: "stremio".into(),
            assignments: vec![],
        }
    }
}
