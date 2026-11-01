//! Four scoped metadata bodies (at most 8 MiB) allow paging without repeat HTTP.
use super::*;
use viptv_engine::stremio;
#[derive(Clone)]
pub(super) struct Document {
    scope: Scope,
    created: std::time::Instant,
    provider: String,
    origin: MetadataOrigin,
    producer_revision: u64,
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
        producer_revision: u64,
        body: String,
        detail: &viptv_engine::presentation::DetailSnapshot,
    ) -> Result<(), CoreError> {
        // A fresh body supersedes every proof/page from its prior version.
        self.verified_cards
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .retain(|(owner, producer, _, _), (cached, card)| {
                let lookup = card.parent_identity.as_ref().unwrap_or(&card.identity);
                owner != &scope.profile_id
                    || producer != provider
                    || lookup.kind != kind
                    || lookup.id != id
                    || cached != scope
            });
        self.detail_snapshots
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .retain(|(owner, producer, target_kind, target_id, _, _), _| {
                owner != &scope.profile_id
                    || producer != provider
                    || target_kind != kind
                    || target_id != id
            });

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
                origin: detail
                    .item
                    .metadata_origin
                    .clone()
                    .ok_or(CoreError::InvalidInput)?,
                producer_revision,
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
        if !origins::shared_lookup(&kind, &id) {
            return Ok(None);
        }
        self.detail_page_owned(profile, kind, id, provider, None, offset, limit)
    }
    pub fn detail_page_for_origin(
        &self,
        profile: String,
        kind: String,
        id: String,
        card_json: String,
        offset: u32,
        limit: u32,
    ) -> Result<Option<String>, CoreError> {
        let expected = origins::expected_card(&card_json)?;
        let provider = expected
            .metadata_origin
            .as_ref()
            .and_then(|origin| origin.connection_id.clone())
            .ok_or(CoreError::InvalidInput)?;
        self.detail_page_owned(profile, kind, id, provider, Some(expected), offset, limit)
    }
}
impl LocalEngine {
    fn detail_page_owned(
        &self,
        profile: String,
        kind: String,
        id: String,
        provider: String,
        expected: Option<viptv_engine::presentation::Card>,
        offset: u32,
        limit: u32,
    ) -> Result<Option<String>, CoreError> {
        if kind == "movie" && offset != 0 {
            return Ok(None);
        }
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
        let issuer = if let Some(expected) = expected.as_ref() {
            let target = expected
                .parent_identity
                .as_ref()
                .unwrap_or(&expected.identity);
            if target.kind != kind || target.id != id {
                return Err(CoreError::InvalidInput);
            }
            Some(origins::metadata_issuer(&store, &current.scope, expected)?)
        } else {
            None
        };
        // An eligible public Search-only issuer may use an unambiguous producer's cache.
        let provider = if issuer.as_ref().is_some_and(|issuer| {
            issuer.capability != viptv_engine::capabilities::Capability::Metadata
        }) {
            String::new()
        } else {
            provider
        };
        let connections = store.connections("local").map_err(|_| CoreError::Bridge)?;
        let eligible = viptv_engine::registry::Registry::builtin()
            .connections_for(
                &current,
                &connections,
                viptv_engine::capabilities::Capability::Metadata,
            )
            .map_err(|_| CoreError::InvalidInput)?;
        let producer_revision = |origin: &str| {
            eligible
                .iter()
                .find(|connection| connection.id == origin)
                .map(|connection| connection.revision)
        };
        let allowed = |origin: &str| producer_revision(origin).is_some();
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
            for ((owner, origin, target_kind, target_id, _, _), (scope, created, _, revision)) in
                snapshots.iter()
            {
                if owner == &profile
                    && target_kind == &kind
                    && target_id == &id
                    && scope == &current.scope
                    && created.elapsed().as_secs() < 300
                    && producer_revision(origin) == Some(*revision)
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
                    && producer_revision(origin) == Some(document.producer_revision)
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
            .filter(|(scope, created, _, revision)| {
                scope == &current.scope
                    && created.elapsed() < std::time::Duration::from_secs(300)
                    && producer_revision(&provider) == Some(*revision)
            })
            .map(|(_, _, snapshot, _)| snapshot.clone());
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
                        && store
                            .validate_metadata_origin(&current.scope, &doc.origin)
                            .is_ok()
                        && producer_revision(&provider) == Some(doc.producer_revision)
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
                if !origins::stremio_card_allowed(&detail.item) {
                    return Ok(None);
                }
                detail.episodes.retain(origins::stremio_card_allowed);
                detail.recommendations.retain(origins::stremio_card_allowed);
                origins::stamp_card(&mut detail.item, &document.origin);
                for card in detail
                    .episodes
                    .iter_mut()
                    .chain(detail.recommendations.iter_mut())
                {
                    origins::stamp_card(card, &document.origin);
                }
                if detail.recommendations.is_empty() {
                    detail.recommendations = document.recommendations;
                }
                Some(detail)
            } else {
                None
            }
        };
        if let Some(snapshot) = snapshot {
            if let Some(origin) = snapshot.item.metadata_origin.as_ref() {
                if store
                    .validate_metadata_origin(&current.scope, origin)
                    .is_err()
                {
                    return Ok(None);
                }
            } else {
                return Ok(None);
            }
            if let Some(expected) = expected.as_ref() {
                let actual = if expected.identity.id == snapshot.item.identity.id {
                    Some(snapshot.item.clone())
                } else {
                    let document = self
                        .detail_documents
                        .lock()
                        .map_err(|_| CoreError::Bridge)?
                        .get(&(profile.clone(), provider.clone(), kind.clone(), id.clone()))
                        .filter(|doc| {
                            doc.scope == current.scope
                                && producer_revision(&provider) == Some(doc.producer_revision)
                                && doc.created.elapsed().as_secs() < 300
                                && store
                                    .validate_metadata_origin(&current.scope, &doc.origin)
                                    .is_ok()
                        })
                        .cloned();
                    if let Some(document) = document {
                        let root: serde_json::Value = serde_json::from_str(&document.body)
                            .map_err(|_| CoreError::InvalidInput)?;
                        let meta = if root["meta"].is_object() {
                            &root["meta"]
                        } else {
                            &root
                        };
                        let mut target = stremio::episode_cards(meta, &provider, &snapshot.item)
                            .map_err(|_| CoreError::InvalidInput)?
                            .into_iter()
                            .find(|card| card.identity.id == expected.identity.id);
                        if let Some(card) = target.as_mut() {
                            origins::stamp_card(card, &document.origin);
                        }
                        target
                    } else {
                        None
                    }
                };
                let Some(actual) = actual else {
                    return Ok(None);
                };
                origins::consistent_metadata_card(
                    expected,
                    &actual,
                    origins::public_metadata_identity(expected),
                )?;
                if actual.identity.id != snapshot.item.identity.id {
                    let proof = viptv_engine::presentation::DetailSnapshot {
                        item: actual,
                        recommendations: vec![],
                        episodes: vec![],
                        next_cursor: None,
                        seasons: vec![],
                    };
                    self.remember_detail_cards(&current.scope, &proof)?;
                }
            }
            self.remember_detail_cards(&current.scope, &snapshot)?;
            Ok(Some(
                serde_json::to_string(&snapshot).map_err(|_| CoreError::Bridge)?,
            ))
        } else {
            Ok(None)
        }
    }
}

impl LocalEngine {
    pub(super) fn has_current_metadata_document(
        &self,
        store: &Store,
        scope: &Scope,
        card: &viptv_engine::presentation::Card,
    ) -> Result<bool, CoreError> {
        let lookup = card.parent_identity.as_ref().unwrap_or(&card.identity);
        let docs = self
            .detail_documents
            .lock()
            .map_err(|_| CoreError::Bridge)?;
        Ok(docs
            .get(&(
                scope.profile_id.clone(),
                card.provider_id.clone(),
                lookup.kind.clone(),
                lookup.id.clone(),
            ))
            .is_some_and(|doc| {
                doc.scope == *scope
                    && doc.created.elapsed().as_secs() < 300
                    && card.metadata_origin.as_ref() == Some(&doc.origin)
                    && origins::require_origin(
                        store,
                        scope,
                        &doc.origin,
                        viptv_engine::capabilities::Capability::Metadata,
                    )
                    .is_ok_and(|connection| connection.revision == doc.producer_revision)
            }))
    }
}

impl LocalEngine {
    pub(super) fn forget_metadata_target(
        &self,
        scope: &Scope,
        provider: &str,
        kind: &str,
        id: &str,
    ) -> Result<(), CoreError> {
        self.detail_documents
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .remove(&(
                scope.profile_id.clone(),
                provider.into(),
                kind.into(),
                id.into(),
            ));
        self.detail_snapshots
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .retain(|(owner, producer, target_kind, target_id, _, _), _| {
                owner != &scope.profile_id
                    || producer != provider
                    || target_kind != kind
                    || target_id != id
            });
        self.verified_cards
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .retain(|(owner, producer, _, _), (_, card)| {
                let target = card.parent_identity.as_ref().unwrap_or(&card.identity);
                owner != &scope.profile_id
                    || producer != provider
                    || target.kind != kind
                    || target.id != id
            });
        Ok(())
    }
}
