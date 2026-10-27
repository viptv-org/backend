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

/// Avatar worlds of the design ADM-004 catalog, version 1
/// (`design/assets/roku/roku/data/avatar-catalog.json` at design `3884fb3`,
/// vendored as `tests/avatar-catalog.json`), in catalog order with their sizes.
/// Positions are append-only: a world may grow, never reorder or shrink.
pub const AVATAR_WORLDS: &[(&str, u16)] = &[
    ("disney", 14),
    ("princesses", 10),
    ("animal-friends", 12),
    ("villains", 12),
    ("critters", 48),
    ("pixelbot", 48),
    ("voxel-bot", 48),
    ("pixel-art", 48),
    ("pixel-art-neutral", 48),
    ("sprouts", 48),
    ("planets", 48),
    ("clay", 48),
    ("moods", 48),
    ("thumbs", 48),
    ("lorelei", 48),
    ("notionists", 48),
];
/// Backend default world for a profile that never chose an avatar; clients
/// render its choice 1 while `avatar_choice` is `None`.
pub const DEFAULT_AVATAR_STYLE: &str = "critters";
/// Legacy ids the local engine stored before the catalog.
pub const LEGACY_AVATAR_IDS: &[&str] = &["vynxc", "zayne", "initial-pink", "initial-blue"];


/// Whether `choice` is a position of the catalog world `style`.
pub fn is_catalog_avatar(style: &str, choice: u16) -> bool {
    AVATAR_WORLDS
        .iter()
        .any(|(world, size)| *world == style && (1..=*size).contains(&choice))
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
pub struct ProfileConfiguration {
    pub scope: Scope,
    pub mode: ExecutionMode,
    pub name: String,
    /// Legacy bundled avatar id (one of [`LEGACY_AVATAR_IDS`]). Shown only
    /// while there is no catalog choice; choosing a catalog avatar clears it.
    #[serde(default)]
    pub avatar_id: Option<String>,
    /// Catalog world, one of [`AVATAR_WORLDS`]. Same field as the backend
    /// account profile. `None` (configurations saved before the catalog)
    /// means [`DEFAULT_AVATAR_STYLE`]; read it through [`Self::avatar_style`].
    /// Optional so generated Kotlin/TypeScript decoders accept older payloads.
    #[serde(default)]
    pub avatar_style: Option<String>,
    /// 1-based position inside `avatar_style`, or `None` when no catalog
    /// avatar was chosen (the backend projects `null` the same way; clients
    /// then show the default, choice 1 of `critters`). A choice takes
    /// precedence over `avatar_id`.
    #[serde(default)]
    pub avatar_choice: Option<u16>,
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
            avatar_id: None,
            avatar_style: Some(DEFAULT_AVATAR_STYLE.into()),
            avatar_choice: None,
            browsing_plugin: "stremio".into(),
            assignments: vec![],
        }
    }

    /// The catalog world, falling back to the backend default.
    pub fn avatar_style(&self) -> &str {
        self.avatar_style.as_deref().unwrap_or(DEFAULT_AVATAR_STYLE)
    }

    /// The legacy id to show: hidden once a catalog choice exists, matching
    /// the backend engine profile projection.
    pub fn shown_avatar_id(&self) -> Option<&str> {
        self.avatar_id
            .as_deref()
            .filter(|_| self.avatar_choice.is_none())
    }
}
