#[cfg(feature = "vsources-runtime")]
#[path = "sources/sdk.rs"]
mod sdk;
use super::*;
#[cfg(feature = "vsources-runtime")]
pub(super) use sdk::sdk_playable;
use serde_json::{Value, json};
use viptv_engine::{
    capabilities::Capability,
    profile::Scope,
    stremio::{self, SourceCandidate},
};

pub(super) struct Request {
    provider: String,
    provider_name: String,
    url: String,
}
pub(super) struct PendingSources {
    scope: Scope,
    kind: String,
    media_id: String,
    requests: Vec<Request>,
    created: std::time::Instant,
    completed: std::collections::HashSet<String>,
    candidates: Vec<SourceCandidate>,
}
pub(super) struct Playable {
    pub(super) scope: Scope,
    pub(super) candidate: SourceCandidate,
    pub(super) kind: String,
    pub(super) media_id: String,
    pub(super) url: String,
    pub(super) headers: Vec<(String, String)>,
    /// Zero-based among audio streams; never an absolute demuxer stream ID.
    pub(super) required_audio_index: Option<u32>,
    pub(super) job_id: Option<String>,
    pub(super) expires: Option<std::time::Instant>,
}

#[uniffi::export]
impl LocalEngine {
    pub fn plan_sources(
        &self,
        profile: String,
        kind: String,
        id: String,
    ) -> Result<String, CoreError> {
        if id.len() > 512 || !["movie", "series"].contains(&kind.as_str()) {
            return Err(CoreError::InvalidInput);
        }
        let store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        let selected = store
            .profile(&local_scope(&profile))
            .map_err(|_| CoreError::Bridge)?
            .ok_or(CoreError::InvalidInput)?;
        let connections = store.connections("local").map_err(|_| CoreError::Bridge)?;
        let mut requests = Vec::new();
        for connection in connections
            .iter()
            .filter(|c| {
                c.enabled
                    && selected.assignments.iter().any(|a| {
                        a.connection_id == c.id && a.enabled.contains(&Capability::Sources)
                    })
            })
            .take(12)
        {
            let Ok(secret) = self.addon(&connection.id) else {
                continue;
            };
            if let Some(url) =
                stremio::stream_url(&secret.manifest_url, &secret.manifest, &kind, &id)
            {
                requests.push(Request {
                    provider: connection.id.clone(),
                    provider_name: connection.name.clone(),
                    url,
                });
            }
        }
        let operation = uuid::Uuid::new_v4().to_string();
        let wires = requests
            .iter()
            .map(|r| json!({"provider":r.provider,"url":r.url}))
            .collect::<Vec<_>>();
        let mut jobs = self.source_jobs.lock().map_err(|_| CoreError::Bridge)?;
        jobs.retain(|_, pending| {
            pending.scope.profile_id != profile && pending.created.elapsed().as_secs() < 120
        });
        if jobs.len() >= 12 {
            return Err(CoreError::InvalidInput);
        }
        // A new discovery replaces pending work, not an already selected grant.
        let mut grants = self.playable.lock().map_err(|_| CoreError::Bridge)?;
        grants.retain(|_, grant| {
            !grant
                .expires
                .is_some_and(|expiry| std::time::Instant::now() >= expiry)
                && (grant.scope.profile_id != profile
                    || grant.scope.revision == selected.scope.revision)
        });
        drop(grants);
        jobs.insert(
            operation.clone(),
            PendingSources {
                scope: selected.scope,
                kind,
                media_id: id,
                requests,
                created: std::time::Instant::now(),
                completed: Default::default(),
                candidates: Vec::new(),
            },
        );
        Ok(json!({"operation":operation,"requests":wires}).to_string())
    }

    pub fn project_sources(
        &self,
        operation: String,
        responses: String,
    ) -> Result<String, CoreError> {
        self.project_sources_partial(operation, responses, true)
    }

    /// Append completed providers to one operation. IDs are stable across updates;
    /// replaying a provider cannot replace its previously issued private grants.
    pub fn project_sources_partial(
        &self,
        operation: String,
        responses: String,
        finish: bool,
    ) -> Result<String, CoreError> {
        if responses.len() > 12 * 3 * 1024 * 1024 {
            return Err(CoreError::InvalidInput);
        }
        let values: Value =
            serde_json::from_str(&responses).map_err(|_| CoreError::InvalidInput)?;
        let items = values
            .as_array()
            .filter(|items| items.len() <= 12)
            .ok_or(CoreError::InvalidInput)?;
        // Consistent lock order: store -> pending operations -> playable grants.
        // Revision validation and grant publication share this critical section.
        let store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        let mut jobs = self.source_jobs.lock().map_err(|_| CoreError::Bridge)?;
        let pending = jobs.get_mut(&operation).ok_or(CoreError::InvalidInput)?;
        let current = store
            .profile(&pending.scope)
            .map_err(|_| CoreError::Bridge)?
            .ok_or(CoreError::InvalidInput)?;
        if current.scope != pending.scope || pending.created.elapsed().as_secs() >= 120 {
            jobs.remove(&operation);
            return Err(CoreError::InvalidInput);
        }
        let mut seen = std::collections::HashSet::new();
        for item in items {
            let provider = item["provider"].as_str().ok_or(CoreError::InvalidInput)?;
            if !seen.insert(provider)
                || !pending
                    .requests
                    .iter()
                    .any(|request| request.provider == provider)
            {
                return Err(CoreError::InvalidInput);
            }
        }
        let mut playable = self.playable.lock().map_err(|_| CoreError::Bridge)?;
        for item in items {
            let provider = item["provider"].as_str().ok_or(CoreError::InvalidInput)?;
            if !pending.completed.insert(provider.to_owned()) {
                continue;
            }
            let (index, request) = pending
                .requests
                .iter()
                .enumerate()
                .find(|(_, r)| r.provider == provider)
                .ok_or(CoreError::InvalidInput)?;
            let sources = item["body"]
                .as_str()
                .and_then(|body| stremio::read_streams(body).ok())
                .unwrap_or_default();
            // Reserve a share for every planned provider, independent of completion
            // order; one 80-result addon cannot hide all later providers.
            let count = pending.requests.len();
            let budget = 80 / count + usize::from(index < 80 % count);
            for mut source in sources.into_iter().take(budget) {
                if playable.len() >= 4096 {
                    break;
                }
                source
                    .presentation
                    .get_or_insert_with(Default::default)
                    .provider_name = Some(request.provider_name.clone());
                let id = uuid::Uuid::new_v4().to_string();
                let candidate = SourceCandidate {
                    id: id.clone(),
                    provider_id: request.provider.clone(),
                    label: source.label,
                    detail: source.detail,
                    quality_height: source.quality_height,
                    audio_languages: source.audio_languages,
                    presentation: source.presentation,
                };
                playable.insert(
                    id,
                    Playable {
                        scope: pending.scope.clone(),
                        candidate: candidate.clone(),
                        kind: pending.kind.clone(),
                        media_id: pending.media_id.clone(),
                        url: source.url,
                        headers: source.headers,
                        required_audio_index: None,
                        job_id: None,
                        expires: Some(
                            std::time::Instant::now() + std::time::Duration::from_secs(1800),
                        ),
                    },
                );
                pending.candidates.push(candidate);
            }
        }
        let output = serde_json::to_string(&pending.candidates).map_err(|_| CoreError::Bridge)?;
        if finish {
            jobs.remove(&operation);
        }
        Ok(output)
    }

    /// Stop accepting results. Issued choices remain usable when selection itself
    /// cancels discovery; revision and expiry still fence every subsequent open.
    pub fn cancel_source_discovery(
        &self,
        profile: String,
        operation: String,
    ) -> Result<(), CoreError> {
        let mut jobs = self.source_jobs.lock().map_err(|_| CoreError::Bridge)?;
        if jobs
            .get(&operation)
            .is_some_and(|job| job.scope.profile_id != profile)
        {
            return Err(CoreError::InvalidInput);
        }
        jobs.remove(&operation);
        Ok(())
    }

    pub fn open_source(&self, id: String) -> Result<String, CoreError> {
        let store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        let play = self.playable.lock().map_err(|_| CoreError::Bridge)?;
        let source = play.get(&id).ok_or(CoreError::InvalidInput)?;
        if source
            .expires
            .is_some_and(|expiry| std::time::Instant::now() >= expiry)
        {
            return Err(CoreError::InvalidInput);
        }
        let current = store
            .profile(&source.scope)
            .map_err(|_| CoreError::Bridge)?
            .ok_or(CoreError::Bridge)?;
        if current.scope.revision != source.scope.revision {
            return Err(CoreError::Bridge);
        }
        let resume = store
            .playback(&source.scope, &source.kind, &source.media_id)
            .map_err(|_| CoreError::Bridge)?
            .filter(|record| record.can_resume())
            .map(|record| record.position_millis)
            .unwrap_or(0);
        serde_json::to_string(&json!({"url":source.url,"headers":source.headers,"required_audio_index":source.required_audio_index,"profile_id":source.scope.profile_id,"revision":source.scope.revision,"position_millis":if source.kind=="live"{0}else{resume},"live":source.kind=="live"}))
            .map_err(|_| CoreError::Bridge)
    }
}

#[cfg(test)]
#[path = "sources/tests.rs"]
mod tests;
