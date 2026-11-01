//! Resolve provenance only through current host-owned connector records.
use super::*;
use viptv_engine::{
    capabilities::Capability,
    identity::{EpisodeCoordinates, MediaIdentity},
    presentation::Card,
    profile::IntegrationConnection,
};

/// Private grant context; presentation and credential fields never enter this value.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(super) struct PlaybackMediaContext {
    kind: String,
    id: String,
    identity: Option<MediaIdentity>,
    parent_identity: Option<MediaIdentity>,
}

pub(super) fn playback_media_context(
    kind: &str,
    id: &str,
    card: Option<&Card>,
) -> PlaybackMediaContext {
    fn normalized(identity: &MediaIdentity) -> MediaIdentity {
        let mut identity = identity.clone();
        identity
            .ids
            .sort_by(|a, b| (&a.namespace, &a.value).cmp(&(&b.namespace, &b.value)));
        identity.ids.dedup();
        identity
    }
    PlaybackMediaContext {
        kind: kind.into(),
        id: id.into(),
        identity: card.map(|card| normalized(&card.identity)),
        parent_identity: card
            .and_then(|card| card.parent_identity.as_ref())
            .map(normalized),
    }
}

pub(super) fn shared_lookup(kind: &str, id: &str) -> bool {
    if id.starts_with("simkl:") {
        return false;
    }
    let parts = id.split(':').collect::<Vec<_>>();
    let coordinates = match parts.as_slice() {
        [_, season, episode] | ["simkl", _, _, season, episode] => season
            .parse::<u32>()
            .ok()
            .zip(episode.parse::<u32>().ok())
            .map(|(season, episode)| EpisodeCoordinates {
                order: "aired".into(),
                season: Some(season),
                episode,
            }),
        _ => None,
    };
    viptv_engine::origin::shared_identity(&MediaIdentity {
        id: id.into(),
        kind: kind.into(),
        ids: vec![],
        episode: coordinates,
        edition: None,
    })
}

pub(super) fn current_origin(
    store: &Store,
    scope: &Scope,
    provider: &str,
    capability: Capability,
) -> Result<MetadataOrigin, CoreError> {
    let profile = store
        .profile(scope)
        .map_err(|_| CoreError::Bridge)?
        .filter(|profile| profile.scope == *scope)
        .ok_or(CoreError::InvalidInput)?;
    let connections = store
        .connections(&scope.account_id)
        .map_err(|_| CoreError::Bridge)?;
    let eligible = viptv_engine::registry::Registry::builtin()
        .connections_for(&profile, &connections, capability)
        .map_err(|_| CoreError::InvalidInput)?;
    if !eligible
        .iter()
        .any(|connection| connection.id == provider && connection.plugin_id == "stremio")
    {
        return Err(CoreError::InvalidInput);
    }
    store
        .metadata_origin(scope, provider)
        .map_err(|_| CoreError::InvalidInput)
}

pub(super) fn require_origin(
    store: &Store,
    scope: &Scope,
    origin: &MetadataOrigin,
    capability: Capability,
) -> Result<IntegrationConnection, CoreError> {
    let id = origin
        .connection_id
        .as_deref()
        .ok_or(CoreError::InvalidInput)?;
    if current_origin(store, scope, id, capability)? != *origin {
        return Err(CoreError::InvalidInput);
    }
    store
        .connections(&scope.account_id)
        .map_err(|_| CoreError::Bridge)?
        .into_iter()
        .find(|connection| connection.id == id)
        .ok_or(CoreError::InvalidInput)
}

/// Authority captured for a metadata operation, independent of a public lookup's issuer.
#[derive(Clone)]
pub(super) struct MetadataFence {
    pub(super) origin: MetadataOrigin,
    pub(super) revision: u64,
    pub(super) capability: Capability,
}
impl MetadataFence {
    pub(super) fn validate(&self, store: &Store, scope: &Scope) -> Result<(), CoreError> {
        let connection = require_origin(store, scope, &self.origin, self.capability)?;
        if connection.revision != self.revision {
            return Err(CoreError::InvalidInput);
        }
        Ok(())
    }
}

pub(super) fn public_metadata_identity(card: &Card) -> bool {
    shared_lookup(&card.identity.kind, &card.identity.id)
        && viptv_engine::origin::shared_identity(&card.identity)
        && card.parent_identity.as_ref().is_none_or(|parent| {
            shared_lookup(&parent.kind, &parent.id) && viptv_engine::origin::shared_identity(parent)
        })
}

pub(super) fn metadata_issuer(
    store: &Store,
    scope: &Scope,
    card: &Card,
) -> Result<MetadataFence, CoreError> {
    let origin = card
        .metadata_origin
        .as_ref()
        .ok_or(CoreError::InvalidInput)?;
    let mut capabilities = vec![Capability::Metadata];
    if public_metadata_identity(card) {
        capabilities.extend([Capability::Search, Capability::Discovery]);
    }
    for capability in capabilities {
        if let Ok(connection) = require_origin(store, scope, origin, capability) {
            return Ok(MetadataFence {
                origin: origin.clone(),
                revision: connection.revision,
                capability,
            });
        }
    }
    Err(CoreError::InvalidInput)
}

/// The caller must first validate both the issuer and the metadata producer.
/// Only intrinsically public lookup identities may change producer origin.
pub(super) fn consistent_metadata_card(
    expected: &Card,
    actual: &Card,
    allow_delegation: bool,
) -> Result<(), CoreError> {
    if expected.metadata_origin == actual.metadata_origin {
        return consistent_card(expected, actual);
    }
    if !allow_delegation
        || !public_metadata_identity(expected)
        || !public_metadata_identity(actual)
        || actual.metadata_origin.is_none()
    {
        return Err(CoreError::InvalidInput);
    }
    let mut expected = expected.clone();
    expected.metadata_origin = actual.metadata_origin.clone();
    consistent_card(&expected, actual)
}

pub(super) fn stremio_card_allowed(card: &viptv_engine::presentation::Card) -> bool {
    !card.identity.id.starts_with("simkl:")
        && card
            .parent_identity
            .as_ref()
            .is_none_or(|identity| !identity.id.starts_with("simkl:"))
}
pub(super) fn strip_reserved_claims(card: &mut viptv_engine::presentation::Card) {
    card.identity.ids.retain(|id| id.namespace != "simkl");
    if let Some(parent) = card.parent_identity.as_mut() {
        parent.ids.retain(|id| id.namespace != "simkl");
    }
}
pub(super) fn stamp_card(card: &mut viptv_engine::presentation::Card, origin: &MetadataOrigin) {
    strip_reserved_claims(card);
    card.metadata_origin = Some(origin.clone());
}

pub(super) fn expected_card(raw: &str) -> Result<viptv_engine::presentation::Card, CoreError> {
    if raw.len() > 64 * 1024 {
        return Err(CoreError::InvalidInput);
    }
    let card: viptv_engine::presentation::Card =
        serde_json::from_str(raw).map_err(|_| CoreError::InvalidInput)?;
    if !stremio_card_allowed(&card) {
        return Err(CoreError::InvalidInput);
    }
    card.validate_library()
        .map_err(|_| CoreError::InvalidInput)?;
    let origin = card
        .metadata_origin
        .as_ref()
        .ok_or(CoreError::InvalidInput)?;
    origin.validate().map_err(|_| CoreError::InvalidInput)?;
    if origin.connection_id.as_deref() != Some(&card.provider_id) {
        return Err(CoreError::InvalidInput);
    }
    Ok(card)
}

/// Expectations can reject changed metadata; they can never create its authority.
pub(super) fn consistent_card(
    expected: &viptv_engine::presentation::Card,
    actual: &viptv_engine::presentation::Card,
) -> Result<(), CoreError> {
    if expected.metadata_origin == actual.metadata_origin
        && viptv_engine::origin::metadata_assertions_match(expected, actual)
    {
        Ok(())
    } else {
        Err(CoreError::InvalidInput)
    }
}

pub(super) fn needs_identity_proof(
    card: &viptv_engine::presentation::Card,
) -> Result<bool, CoreError> {
    let mut unasserted = card.clone();
    unasserted.identity.ids.clear();
    unasserted.identity.episode = None;
    unasserted.identity.edition = None;
    if let Some(parent) = unasserted.parent_identity.as_mut() {
        parent.ids.clear();
        parent.episode = None;
        parent.edition = None;
    }
    Ok(!viptv_engine::origin::metadata_assertions_match(
        card,
        &unasserted,
    ))
}

impl LocalEngine {
    pub(super) fn expected_source_card(
        &self,
        profile: &str,
        hint: &viptv_engine::presentation::Card,
        require_proof: bool,
    ) -> Result<(Scope, viptv_engine::presentation::Card), CoreError> {
        let store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        let selected = store
            .profile(&local_scope(profile))
            .map_err(|_| CoreError::Bridge)?
            .ok_or(CoreError::InvalidInput)?;
        let origin = hint
            .metadata_origin
            .as_ref()
            .ok_or(CoreError::InvalidInput)?;
        if origin.connection_id.as_deref() != Some(&hint.provider_id) {
            return Err(CoreError::InvalidInput);
        }
        store
            .validate_metadata_origin(&selected.scope, origin)
            .map_err(|_| CoreError::InvalidInput)?;
        if let Some(card) = self.fresh_metadata_card(&store, &selected.scope, hint)? {
            return Ok((selected.scope, card));
        }
        let asserted = needs_identity_proof(hint)?;
        if require_proof || asserted {
            return Err(CoreError::InvalidInput);
        }
        // A catalog-only connector may still stream its own private opaque ID.
        // Unverified external aliases are never used by this fallback.
        Ok((selected.scope, hint.clone()))
    }
    pub(super) fn fresh_metadata_card(
        &self,
        store: &Store,
        scope: &Scope,
        hint: &viptv_engine::presentation::Card,
    ) -> Result<Option<viptv_engine::presentation::Card>, CoreError> {
        let origin = hint
            .metadata_origin
            .as_ref()
            .ok_or(CoreError::InvalidInput)?;
        if origin.connection_id.as_deref() != Some(hint.provider_id.as_str()) {
            return Err(CoreError::InvalidInput);
        }
        store
            .validate_metadata_origin(scope, origin)
            .map_err(|_| CoreError::InvalidInput)?;
        let cached = self
            .verified_cards
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .get(&(
                scope.profile_id.clone(),
                hint.provider_id.clone(),
                hint.identity.kind.clone(),
                hint.identity.id.clone(),
            ))
            .filter(|(cached, _)| cached == scope)
            .map(|(_, card)| card.clone());
        if let Some(card) = cached {
            if self.has_current_metadata_document(store, scope, &card)? {
                consistent_card(hint, &card)?;
                return Ok(Some(card));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests;

pub(super) fn presentation_matches(
    original: &viptv_engine::presentation::Card,
    metadata: &viptv_engine::presentation::Card,
) -> bool {
    let mut expected = original.clone();
    if let Some(parent) = expected.parent_identity.take() {
        expected.identity = parent;
    }
    if expected.metadata_origin.is_none()
        && !shared_lookup(&expected.identity.kind, &expected.identity.id)
    {
        return false;
    }
    viptv_engine::origin::metadata_assertions_match(&expected, metadata)
}

/// Presentation callers pass only host-verified metadata after scope/origin
/// validation. Carry its assertions without rewriting the provider lookup ID.
pub(super) fn enrich_verified_card(
    original: &viptv_engine::presentation::Card,
    metadata: &viptv_engine::presentation::Card,
) -> viptv_engine::presentation::Card {
    if metadata.metadata_origin.is_none() || !presentation_matches(original, metadata) {
        return original.clone();
    }
    let mut card = viptv_engine::presentation::enrich_card(original, metadata);
    if let Some(parent) = card.parent_identity.as_mut() {
        parent.ids = metadata.identity.ids.clone();
    } else {
        card.identity.ids = metadata.identity.ids.clone();
    }
    card
}
