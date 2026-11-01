use super::*;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use viptv_engine::{
    capabilities::Capability,
    presentation::{Card, RowState},
    registry::Registry,
    stremio::{
        self, SubtitleCandidate, SubtitleExtras, SubtitleFile, SubtitleProvider, SubtitleSnapshot,
    },
};

struct Request {
    origin: MetadataOrigin,
    provider: String,
    name: String,
    url: Option<String>,
}
pub(super) struct PendingSubtitles {
    metadata_origin: Option<MetadataOrigin>,
    media_context: origins::PlaybackMediaContext,
    scope: Scope,
    requests: Vec<Request>,
}
#[derive(Clone)]
pub(super) struct SelectedSubtitle {
    pub(super) metadata_origin: Option<MetadataOrigin>,
    pub(super) media_context: origins::PlaybackMediaContext,
    pub(super) provider_origin: Option<MetadataOrigin>,
    pub(super) scope: Scope,
    pub(super) file: SubtitleFile,
    pub(super) created: Instant,
}

#[uniffi::export]
impl LocalEngine {
    /// Extras are optional actual file evidence; hosts must not invent hashes or sizes.
    pub fn plan_subtitles(
        &self,
        profile: String,
        kind: String,
        id: String,
        extras_json: String,
    ) -> Result<String, CoreError> {
        if !origins::shared_lookup(&kind, &id) {
            return Err(CoreError::InvalidInput);
        }
        self.plan_subtitles_owned(profile, kind, id, extras_json, None)
    }
    pub fn plan_subtitles_for_origin(
        &self,
        profile: String,
        kind: String,
        id: String,
        extras_json: String,
        card_json: String,
    ) -> Result<String, CoreError> {
        let expected = origins::expected_card(&card_json)?;
        if expected.identity.kind != kind || expected.identity.id != id {
            return Err(CoreError::InvalidInput);
        }
        let (_, card) = self.expected_source_card(&profile, &expected, false)?;
        self.plan_subtitles_owned(profile, kind, id, extras_json, Some(card))
    }

    pub fn project_subtitles(
        &self,
        operation: String,
        responses_json: String,
    ) -> Result<String, CoreError> {
        if responses_json.len() > 12 * 2 * 1024 * 1024 {
            return Err(CoreError::InvalidInput);
        }
        let values: Value =
            serde_json::from_str(&responses_json).map_err(|_| CoreError::InvalidInput)?;
        let responses = values.as_array().ok_or(CoreError::InvalidInput)?;
        let pending = self
            .subtitle_jobs
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .remove(&operation)
            .ok_or(CoreError::InvalidInput)?;
        let store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        let current = store
            .profile(&pending.scope)
            .map_err(|_| CoreError::Bridge)?
            .ok_or(CoreError::InvalidInput)?;
        if current.scope.revision != pending.scope.revision {
            return Err(CoreError::Bridge);
        }
        if let Some(origin) = pending.metadata_origin.as_ref() {
            store
                .validate_metadata_origin(&pending.scope, origin)
                .map_err(|_| CoreError::InvalidInput)?;
        }
        for request in &pending.requests {
            store
                .validate_metadata_origin(&pending.scope, &request.origin)
                .map_err(|_| CoreError::InvalidInput)?;
        }
        let mut selected = self
            .subtitle_selections
            .lock()
            .map_err(|_| CoreError::Bridge)?;
        selected.retain(|_, selection| {
            selection.scope.profile_id != pending.scope.profile_id
                && selection.created.elapsed() < Duration::from_secs(600)
        });
        let mut snapshot = SubtitleSnapshot {
            scope: pending.scope.clone(),
            candidates: vec![],
            providers: vec![],
            complete: true,
        };
        for request in pending.requests {
            let result = responses
                .iter()
                .find(|v| v["provider"].as_str() == Some(&request.provider))
                .and_then(|v| v["body"].as_str())
                .and_then(|raw| stremio::read_subtitles(raw).ok());
            let state = match &result {
                None => RowState::Failed,
                Some(files) if files.is_empty() => RowState::Empty,
                _ => RowState::Ready,
            };
            snapshot.providers.push(SubtitleProvider {
                provider_id: request.provider.clone(),
                message: (state == RowState::Failed)
                    .then(|| "Subtitles from this addon are unavailable. Try again.".into()),
                state,
            });
            for file in result
                .unwrap_or_default()
                .into_iter()
                .take(80 - snapshot.candidates.len())
            {
                let id = uuid::Uuid::new_v4().to_string();
                snapshot.candidates.push(SubtitleCandidate {
                    id: id.clone(),
                    provider_id: request.provider.clone(),
                    language: file.language.clone(),
                    label: format!("{} · {}", file.language, request.name),
                });
                selected.insert(
                    id,
                    SelectedSubtitle {
                        metadata_origin: pending.metadata_origin.clone(),
                        media_context: pending.media_context.clone(),
                        provider_origin: Some(request.origin.clone()),
                        scope: pending.scope.clone(),
                        file,
                        created: Instant::now(),
                    },
                );
            }
        }
        serde_json::to_string(&snapshot).map_err(|_| CoreError::Bridge)
    }

    pub fn open_subtitle(&self, candidate_id: String) -> Result<String, CoreError> {
        let store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        let selected = self
            .subtitle_selections
            .lock()
            .map_err(|_| CoreError::Bridge)?;
        let selected = selected.get(&candidate_id).ok_or(CoreError::InvalidInput)?;
        if let Some(origin) = selected.provider_origin.as_ref() {
            store
                .validate_metadata_origin(&selected.scope, origin)
                .map_err(|_| CoreError::InvalidInput)?;
        }
        if let Some(origin) = selected.metadata_origin.as_ref() {
            store
                .validate_metadata_origin(&selected.scope, origin)
                .map_err(|_| CoreError::InvalidInput)?;
        }
        if selected.created.elapsed() >= Duration::from_secs(600) {
            return Err(CoreError::InvalidInput);
        }
        let current = store
            .profile(&selected.scope)
            .map_err(|_| CoreError::Bridge)?
            .ok_or(CoreError::InvalidInput)?;
        if current.scope.revision != selected.scope.revision {
            return Err(CoreError::Bridge);
        }
        Ok(json!({"metadata_origin":selected.metadata_origin,"media_context":selected.media_context,"url":selected.file.url,"language":selected.file.language,"profile_id":selected.scope.profile_id,"revision":selected.scope.revision}).to_string())
    }
}

impl LocalEngine {
    pub(super) fn forget_subtitle_profile(&self, profile: &str) -> Result<(), CoreError> {
        self.subtitle_jobs
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .retain(|_, pending| pending.scope.profile_id != profile);
        self.subtitle_selections
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .retain(|_, selection| selection.scope.profile_id != profile);
        Ok(())
    }
}

impl LocalEngine {
    fn plan_subtitles_owned(
        &self,
        profile: String,
        kind: String,
        id: String,
        extras_json: String,
        metadata_card: Option<Card>,
    ) -> Result<String, CoreError> {
        let metadata_origin = metadata_card
            .as_ref()
            .and_then(|card| card.metadata_origin.clone());
        let media_context = origins::playback_media_context(&kind, &id, metadata_card.as_ref());
        if extras_json.len() > 2048 {
            return Err(CoreError::InvalidInput);
        }
        let extras: SubtitleExtras =
            serde_json::from_str(&extras_json).map_err(|_| CoreError::InvalidInput)?;
        extras.validate().map_err(|_| CoreError::InvalidInput)?;
        if id.is_empty()
            || id.len() > 512
            || id.chars().any(char::is_control)
            || !["movie", "series"].contains(&kind.as_str())
        {
            return Err(CoreError::InvalidInput);
        }
        let store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        let selected = store
            .profile(&local_scope(&profile))
            .map_err(|_| CoreError::Bridge)?
            .ok_or(CoreError::InvalidInput)?;
        if let Some(origin) = metadata_origin.as_ref() {
            store
                .validate_metadata_origin(&selected.scope, origin)
                .map_err(|_| CoreError::InvalidInput)?;
        }
        let connections = store.connections("local").map_err(|_| CoreError::Bridge)?;
        let registry = Registry::builtin();
        let enabled = registry
            .connections_for(&selected, &connections, Capability::Subtitles)
            .map_err(|_| CoreError::Bridge)?;
        let mut requests = Vec::new();
        for connection in enabled
            .into_iter()
            .filter(|c| {
                c.plugin_id == "stremio"
                    && (origins::shared_lookup(&kind, &id)
                        || metadata_origin
                            .as_ref()
                            .and_then(|origin| origin.connection_id.as_deref())
                            == Some(c.id.as_str()))
            })
            .take(12)
        {
            let url = match self.addon(&connection.id) {
                Ok(addon) => match stremio::subtitle_url(
                    &addon.manifest_url,
                    &addon.manifest,
                    &kind,
                    &id,
                    &extras,
                )
                .map_err(|_| CoreError::InvalidInput)?
                {
                    Some(url) => Some(url),
                    None => continue,
                },
                Err(_) => None,
            };
            requests.push(Request {
                origin: store
                    .metadata_origin(&selected.scope, &connection.id)
                    .map_err(|_| CoreError::InvalidInput)?,
                provider: connection.id.clone(),
                name: connection.name.clone(),
                url,
            });
        }
        let operation = uuid::Uuid::new_v4().to_string();
        let wire: Vec<_> = requests
            .iter()
            .map(|r| json!({"provider":r.provider,"url":r.url}))
            .collect();
        let mut jobs = self.subtitle_jobs.lock().map_err(|_| CoreError::Bridge)?;
        jobs.retain(|_, job| job.scope.profile_id != profile);
        jobs.insert(
            operation.clone(),
            PendingSubtitles {
                scope: selected.scope,
                metadata_origin,
                media_context,
                requests,
            },
        );
        Ok(json!({"operation":operation,"requests":wire}).to_string())
    }
}
