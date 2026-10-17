//! Four scoped metadata bodies (at most 8 MiB) allow paging without repeat HTTP.
use super::*;
use viptv_engine::stremio;
#[derive(Clone)]
pub(super) struct Document {
    scope: Scope,
    created: std::time::Instant,
    provider: String,
    body: Arc<str>,
    recommendations: Vec<viptv_engine::presentation::Card>,
}
impl LocalEngine {
    pub(super) fn remember_detail_cards(
        &self,
        scope: &Scope,
        detail: &viptv_engine::presentation::DetailSnapshot,
    ) -> Result<(), CoreError> {
        let mut verified = self.verified_cards.lock().map_err(|_| CoreError::Bridge)?;
        verified.retain(|(profile, _, _, _), (cached, _)| {
            profile != &scope.profile_id || cached.revision == scope.revision
        });
        if verified.len() > 4096 {
            verified.clear();
        }
        for card in std::iter::once(&detail.item).chain(detail.episodes.iter()) {
            verified.insert(
                (
                    scope.profile_id.clone(),
                    card.provider_id.clone(),
                    card.identity.kind.clone(),
                    card.identity.id.clone(),
                ),
                (scope.clone(), card.clone()),
            );
        }
        Ok(())
    }
    pub(super) fn remember_detail_document(
        &self,
        scope: &Scope,
        kind: &str,
        id: &str,
        provider: &str,
        body: String,
        detail: &viptv_engine::presentation::DetailSnapshot,
    ) -> Result<(), CoreError> {
        if kind != "series" {
            return Ok(());
        }
        let mut documents = self
            .detail_documents
            .lock()
            .map_err(|_| CoreError::Bridge)?;
        documents.retain(|(profile, _, _, _), doc| {
            doc.created.elapsed() < std::time::Duration::from_secs(300)
                && (profile != &scope.profile_id || doc.scope == *scope)
        });
        let key = (
            scope.profile_id.clone(),
            provider.into(),
            kind.into(),
            id.into(),
        );
        if documents.len() >= 4 && !documents.contains_key(&key) {
            if let Some(oldest) = documents
                .iter()
                .min_by_key(|(_, doc)| doc.created)
                .map(|(key, _)| key.clone())
            {
                documents.remove(&oldest);
            }
        }
        documents.insert(
            key,
            Document {
                scope: scope.clone(),
                created: std::time::Instant::now(),
                provider: provider.into(),
                body: body.into(),
                recommendations: detail.recommendations.clone(),
            },
        );
        Ok(())
    }
}
#[uniffi::export]
impl LocalEngine {
    pub fn cached_detail(
        &self,
        profile: String,
        kind: String,
        id: String,
        offset: u32,
    ) -> Result<Option<String>, CoreError> {
        self.detail_page(profile, kind, id, offset, 24)
    }
    /// A null result asks the native host to run the existing planned HTTP fetch.
    pub fn detail_page(
        &self,
        profile: String,
        kind: String,
        id: String,
        offset: u32,
        limit: u32,
    ) -> Result<Option<String>, CoreError> {
        self.detail_page_for_provider(profile, kind, id, String::new(), offset, limit)
    }

    /// Cache lookup for one metadata producer. An empty legacy origin is reusable
    /// only when exactly one enabled producer has a current cache for this target.
    pub fn detail_page_for_provider(
        &self,
        profile: String,
        kind: String,
        id: String,
        provider: String,
        offset: u32,
        limit: u32,
    ) -> Result<Option<String>, CoreError> {
        if !(1..=128).contains(&limit)
            || id.is_empty()
            || id.len() > 512
            || !matches!(kind.as_str(), "movie" | "series")
            || offset > 100000
        {
            return Err(CoreError::InvalidInput);
        }
        let store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        let current = store
            .profile(&local_scope(&profile))
            .map_err(|_| CoreError::Bridge)?
            .ok_or(CoreError::InvalidInput)?;
        let connections = store.connections("local").map_err(|_| CoreError::Bridge)?;
        let eligible = viptv_engine::registry::Registry::builtin()
            .connections_for(
                &current,
                &connections,
                viptv_engine::capabilities::Capability::Metadata,
            )
            .map_err(|_| CoreError::InvalidInput)?;
        let allowed = |origin: &str| eligible.iter().any(|connection| connection.id == origin);
        let provider = if !provider.is_empty() && provider != current.browsing_plugin {
            if !allowed(&provider) {
                return Ok(None);
            }
            provider
        } else {
            let mut origins = std::collections::HashSet::new();
            let snapshots = self
                .detail_snapshots
                .lock()
                .map_err(|_| CoreError::Bridge)?;
            for ((owner, origin, target_kind, target_id, _, _), (scope, created, _)) in
                snapshots.iter()
            {
                if owner == &profile
                    && target_kind == &kind
                    && target_id == &id
                    && scope == &current.scope
                    && created.elapsed().as_secs() < 300
                    && allowed(origin)
                {
                    origins.insert(origin.clone());
                }
            }
            drop(snapshots);
            let documents = self
                .detail_documents
                .lock()
                .map_err(|_| CoreError::Bridge)?;
            for ((owner, origin, target_kind, target_id), document) in documents.iter() {
                if owner == &profile
                    && target_kind == &kind
                    && target_id == &id
                    && document.scope == current.scope
                    && document.created.elapsed().as_secs() < 300
                    && allowed(origin)
                {
                    origins.insert(origin.clone());
                }
            }
            if origins.len() != 1 {
                return Ok(None);
            }
            origins.into_iter().next().ok_or(CoreError::InvalidInput)?
        };
        let snapshot = self
            .detail_snapshots
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .get(&(
                profile.clone(),
                provider.clone(),
                kind.clone(),
                id.clone(),
                offset,
                limit,
            ))
            .filter(|(scope, created, _)| {
                scope == &current.scope && created.elapsed() < std::time::Duration::from_secs(300)
            })
            .map(|(_, _, snapshot)| snapshot.clone());
        let snapshot = if let Some(snapshot) = snapshot {
            Some(snapshot)
        } else {
            let document = self
                .detail_documents
                .lock()
                .map_err(|_| CoreError::Bridge)?
                .get(&(profile.clone(), provider.clone(), kind.clone(), id.clone()))
                .filter(|doc| {
                    doc.scope == current.scope
                        && doc.created.elapsed() < std::time::Duration::from_secs(300)
                })
                .cloned();
            if let Some(document) = document {
                let mut detail = stremio::detail_page(
                    &document.body,
                    &document.provider,
                    &kind,
                    &id,
                    offset as usize,
                    limit as usize,
                )
                .map_err(|_| CoreError::InvalidInput)?;
                if detail.recommendations.is_empty() {
                    detail.recommendations = document.recommendations;
                }
                Some(detail)
            } else {
                None
            }
        };
        if let Some(snapshot) = snapshot {
            self.remember_detail_cards(&current.scope, &snapshot)?;
            Ok(Some(
                serde_json::to_string(&snapshot).map_err(|_| CoreError::Bridge)?,
            ))
        } else {
            Ok(None)
        }
    }
}
