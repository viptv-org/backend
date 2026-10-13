use crate::identity::MediaIdentity;
use crate::presentation::Card;
use facet::Facet;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, Facet)]
pub struct PlaybackRecord {
    #[serde(default)]
    pub times_watched: Option<u64>,
    #[serde(default)]
    pub presentation: Option<Card>,
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
            Some(resume_caption(
                &self.identity,
                self.duration_millis.saturating_sub(self.position_millis),
            ))
        } else if self.completed_at_millis.is_some() {
            Some("Watched".into())
        } else {
            None
        };
        let mut card = self.presentation.clone().unwrap_or_else(|| Card {
            identity: self.identity.clone(),
            provider_id: self.provider_id.clone(),
            title: self.title.clone(),
            subtitle: subtitle.clone(),
            description: None,
            poster: self.poster.clone(),
            landscape: self.landscape.clone(),
            logo: None,
            progress: self.progress(),
            badges: vec![],
            parent_identity: self.parent_identity.clone(),
            added_at_millis: None,
            category: None,
            metadata: None,
        });
        card.identity = self.identity.clone();
        card.parent_identity = self.parent_identity.clone();
        card.progress = self.progress();
        card.subtitle = subtitle;
        card.added_at_millis = None;
        card
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

/// Captions use typed episode coordinates, never provider identifier or title parsing.
pub fn resume_caption(identity: &MediaIdentity, remaining_millis: u64) -> String {
    let remaining = if remaining_millis < 60000 {
        format!("{}s left", remaining_millis.div_ceil(1000))
    } else {
        format!("{}m left", remaining_millis.div_ceil(60000))
    };
    match identity
        .episode
        .as_ref()
        .filter(|episode| identity.kind == "series" && episode.episode > 0)
    {
        Some(episode) => match episode.season {
            Some(season) => format!("S{season} E{} · {remaining}", episode.episode),
            None => format!("E{} · {remaining}", episode.episode),
        },
        None => remaining,
    }
}
