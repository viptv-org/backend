use crate::identity::MediaIdentity;
use crate::presentation::Card;
use facet::Facet;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
pub struct PlaybackRecord {
    pub identity: MediaIdentity,
    pub provider_id: String,
    pub title: String,
    pub poster: Option<String>,
    pub landscape: Option<String>,
    pub position_millis: u64,
    pub duration_millis: u64,
    pub updated_at_millis: u64,
    pub completed_at_millis: Option<u64>,
    #[serde(default)]
    pub parent_identity: Option<MediaIdentity>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
pub struct HistoryEntry {
    pub card: Card,
    pub updated_at_millis: u64,
    pub completed_at_millis: Option<u64>,
}

impl From<&PlaybackRecord> for HistoryEntry {
    fn from(record: &PlaybackRecord) -> Self {
        Self {
            card: record.card(),
            updated_at_millis: record.updated_at_millis,
            completed_at_millis: record.completed_at_millis,
        }
    }
}

impl PlaybackRecord {
    pub fn card(&self) -> Card {
        let subtitle = if self.can_resume() {
            let remaining = self.duration_millis.saturating_sub(self.position_millis);
            if remaining < 60_000 {
                Some(format!("{}s left", remaining.div_ceil(1_000)))
            } else {
                Some(format!("{}m left", remaining.div_ceil(60_000)))
            }
        } else if self.completed_at_millis.is_some() {
            Some("Watched".into())
        } else {
            None
        };
        Card {
            identity: self.identity.clone(),
            provider_id: self.provider_id.clone(),
            title: self.title.clone(),
            subtitle,
            description: None,
            poster: self.poster.clone(),
            landscape: self.landscape.clone(),
            logo: None,
            progress: self.progress(),
            badges: vec![],
            parent_identity: self.parent_identity.clone(),
            added_at_millis:None,
        }
    }
    pub fn can_resume(&self) -> bool {
        self.position_millis > 0
            && self.duration_millis > 0
            && self.position_millis.saturating_mul(100) < self.duration_millis.saturating_mul(95)
    }
    pub fn progress(&self) -> Option<f64> {
        (self.duration_millis > 0)
            .then(|| (self.position_millis as f64 / self.duration_millis as f64).clamp(0.0, 1.0))
    }
}
