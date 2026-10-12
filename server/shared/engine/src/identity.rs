use crate::Error;
use facet::Facet;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Facet)]
pub struct ExternalId {
    pub namespace: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Facet)]
pub struct EpisodeCoordinates {
    pub order: String,
    pub season: Option<u32>,
    pub episode: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Facet)]
pub struct MediaIdentity {
    pub id: String,
    pub kind: String,
    pub ids: Vec<ExternalId>,
    pub episode: Option<EpisodeCoordinates>,
    pub edition: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Facet)]
#[repr(C)]
pub enum MappingConfidence {
    Explicit,
    Candidate,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
pub struct MappingEvidence {
    pub source: String,
    pub target: ExternalId,
    pub confidence: MappingConfidence,
    pub episode: Option<EpisodeCoordinates>,
    pub version: String,
}

/// A title match alone cannot resolve an episode. Conflicting exact mappings stay unresolved.
pub fn resolve(
    identity: &MediaIdentity,
    namespace: &str,
    evidence: &[MappingEvidence],
) -> Result<ExternalId, Error> {
    let mut targets: Vec<ExternalId> = identity
        .ids
        .iter()
        .filter(|i| i.namespace == namespace)
        .cloned()
        .collect();
    for e in evidence
        .iter()
        .filter(|e| e.target.namespace == namespace && e.confidence == MappingConfidence::Explicit)
    {
        if identity.episode.is_some() && e.episode != identity.episode {
            continue;
        }
        targets.push(e.target.clone());
    }
    let target = targets.first().ok_or(Error::Unresolved)?;
    if targets.iter().any(|t| t != target) {
        return Err(Error::Unresolved);
    }
    Ok(target.clone())
}
