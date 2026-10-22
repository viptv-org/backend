use facet::Facet;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Facet)]
#[repr(C)]
pub enum ExecutionMode {
    Local,
    Server,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Facet)]
#[repr(C)]
pub enum Capability {
    Discovery,
    Search,
    Metadata,
    Sources,
    Subtitles,
    Tracking,
    Authorization,
    Mapping,
    LiveTv,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Facet)]
#[repr(C)]
pub enum Availability {
    Available,
    NeedsConfiguration,
    NeedsAuthorization,
    TemporarilyUnavailable,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Facet)]
pub struct CapabilityState {
    pub capability: Capability,
    pub availability: Availability,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
pub struct PluginDescriptor {
    pub id: String,
    pub name: String,
    pub contract_version: u32,
    pub locations: Vec<ExecutionMode>,
    pub capabilities: Vec<Capability>,
}

pub fn builtins() -> Vec<PluginDescriptor> {
    use Capability::*;
    use ExecutionMode::*;
    [
        (
            "stremio",
            "Stremio addons",
            vec![Local, Server],
            vec![Discovery, Search, Metadata, Sources, Subtitles],
        ),
        (
            "simkl",
            "Simkl",
            vec![Server],
            vec![
                Discovery,
                Search,
                Metadata,
                Tracking,
                Authorization,
                Mapping,
            ],
        ),
        ("vsources", "Sources", vec![Local, Server], vec![Sources]),
        (
            "xtream",
            "Xtream",
            vec![Local, Server],
            vec![LiveTv, Sources, Metadata],
        ),
        (
            "m3u",
            "M3U playlist",
            vec![Local, Server],
            vec![LiveTv, Sources],
        ),
        (
            "identity",
            "Identity mappings",
            vec![Local, Server],
            vec![Mapping],
        ),
    ]
    .into_iter()
    .map(|(id, name, locations, capabilities)| PluginDescriptor {
        id: id.into(),
        name: name.into(),
        contract_version: crate::CONTRACT_VERSION,
        locations,
        capabilities,
    })
    .collect()
}
