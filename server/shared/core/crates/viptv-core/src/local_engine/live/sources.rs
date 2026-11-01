use super::*;
#[uniffi::export]
impl LocalEngine {
    pub fn live_sources(&self, profile: String, channel_id: String) -> Result<String, CoreError> {
        #[cfg(not(feature = "live-runtime"))]
        {
            let _ = (profile, channel_id);
            Err(CoreError::InvalidInput)
        }
        #[cfg(feature = "live-runtime")]
        {
            let (scope, connection, channel, _) =
                self.live_channel(&profile, &channel_id, deadline())?;
            self.live_validate(&scope)?;
            let mut grants = self.playable.lock().map_err(|_| CoreError::Bridge)?;
            let mut output = Vec::new();
            for source in channel.sources {
                let id = uuid::Uuid::new_v4().to_string();
                let candidate = viptv_engine::stremio::SourceCandidate {
                    id: id.clone(),
                    provider_id: connection.id.clone(),
                    label: channel.channel.card.title.clone(),
                    detail: None,
                    quality_height: None,
                    audio_languages: vec![],
                    presentation: None,
                    ..Default::default()
                };
                grants.insert(
                    id,
                    crate::local_engine::sources::Playable {
                        scope: scope.clone(),
                        metadata_origin: None,
                        provider_origin: None,
                        metadata_card: None,
                        candidate: candidate.clone(),
                        kind: "live".into(),
                        media_id: channel_id.clone(),
                        url: source.url,
                        headers: source.headers.into_iter().collect(),
                        required_audio_index: None,
                        job_id: None,
                        expires: Some(
                            std::time::Instant::now() + std::time::Duration::from_secs(300),
                        ),
                    },
                );
                output.push(candidate);
            }
            serde_json::to_string(&output).map_err(|_| CoreError::Bridge)
        }
    }
}
