use super::*;
use serde_json::{Value, json};
use viptv_engine::{capabilities::Capability, stremio};

pub(super) struct PendingDetail {
    pub(super) scope: viptv_engine::profile::Scope,
    created: std::time::Instant,
    pub(super) kind: String,
    pub(super) id: String,
    pub(super) provider: String,
    pub(super) origin: MetadataOrigin,
    pub(super) producer_revision: u64,
    issuer: Option<origins::MetadataFence>,
    pub(super) expected: Option<viptv_engine::presentation::Card>,
}

#[uniffi::export]
impl LocalEngine {
    pub fn plan_detail(
        &self,
        profile: String,
        kind: String,
        id: String,
        preferred: String,
    ) -> Result<String, CoreError> {
        if !origins::shared_lookup(&kind, &id) {
            return Err(CoreError::InvalidInput);
        }
        self.plan_detail_owned(profile, kind, id, preferred, None)
    }
    pub fn plan_detail_for_origin(
        &self,
        profile: String,
        kind: String,
        id: String,
        card_json: String,
    ) -> Result<String, CoreError> {
        let expected = origins::expected_card(&card_json)?;
        let origin = expected
            .metadata_origin
            .clone()
            .ok_or(CoreError::InvalidInput)?;
        let preferred = origin
            .connection_id
            .clone()
            .ok_or(CoreError::InvalidInput)?;
        self.plan_detail_owned(profile, kind, id, preferred, Some(expected))
    }

    pub fn project_detail(
        &self,
        operation: String,
        body: String,
        offset: u32,
    ) -> Result<String, CoreError> {
        self.project_detail_page(operation, body, offset, 24)
    }
    pub fn project_detail_page(
        &self,
        operation: String,
        body: String,
        offset: u32,
        limit: u32,
    ) -> Result<String, CoreError> {
        if offset > 100_000 || !(1..=128).contains(&limit) {
            return Err(CoreError::InvalidInput);
        }
        let pending = self
            .details
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .remove(&operation)
            .ok_or(CoreError::InvalidInput)?;
        if !self.metadata_request_current(&pending, &operation)? {
            return Err(CoreError::InvalidInput);
        }
        {
            let store = self.store.lock().map_err(|_| CoreError::Bridge)?;
            self.validate_metadata_producers(&store, &pending)?;
        }
        let parsed = stremio::detail_page(
            &body,
            &pending.provider,
            &pending.kind,
            &pending.id,
            offset as usize,
            limit as usize,
        );
        let mut detail = match parsed {
            Ok(detail) => detail,
            Err(error) => {
                if matches!(error, viptv_engine::Error::Unresolved) {
                    let store = self.store.lock().map_err(|_| CoreError::Bridge)?;
                    if store
                        .profile(&pending.scope)
                        .map_err(|_| CoreError::Bridge)?
                        .is_some_and(|profile| profile.scope == pending.scope)
                        && store
                            .validate_metadata_origin(&pending.scope, &pending.origin)
                            .is_ok()
                        && self.validate_metadata_producers(&store, &pending).is_ok()
                        && self.metadata_request_current(&pending, &operation)?
                    {
                        self.forget_metadata_target(
                            &pending.scope,
                            &pending.provider,
                            &pending.kind,
                            &pending.id,
                        )?;
                    }
                }
                return Err(CoreError::InvalidInput);
            }
        };
        if !origins::stremio_card_allowed(&detail.item) {
            return Err(CoreError::InvalidInput);
        }
        detail.episodes.retain(origins::stremio_card_allowed);
        detail.recommendations.retain(origins::stremio_card_allowed);
        origins::stamp_card(&mut detail.item, &pending.origin);
        for card in detail
            .episodes
            .iter_mut()
            .chain(detail.recommendations.iter_mut())
        {
            origins::stamp_card(card, &pending.origin);
        }
        let expected_episode =
            (|| -> Result<Option<viptv_engine::presentation::Card>, CoreError> {
                let expected_episode = if let Some(expected) = pending.expected.as_ref() {
                    if expected.identity.id == detail.item.identity.id {
                        origins::consistent_metadata_card(
                            expected,
                            &detail.item,
                            origins::public_metadata_identity(expected),
                        )?;
                        None
                    } else {
                        let root: Value =
                            serde_json::from_str(&body).map_err(|_| CoreError::InvalidInput)?;
                        let meta = if root["meta"].is_object() {
                            &root["meta"]
                        } else {
                            &root
                        };
                        let mut episode =
                            stremio::episode_cards(meta, &pending.provider, &detail.item)
                                .map_err(|_| CoreError::InvalidInput)?
                                .into_iter()
                                .find(|card| card.identity.id == expected.identity.id)
                                .ok_or(CoreError::InvalidInput)?;
                        origins::stamp_card(&mut episode, &pending.origin);
                        origins::consistent_metadata_card(
                            expected,
                            &episode,
                            origins::public_metadata_identity(expected),
                        )?;
                        Some(episode)
                    }
                } else {
                    None
                };
                Ok(expected_episode)
            })();
        if detail.recommendations.is_empty() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |t| t.as_secs() as i64);
            detail.recommendations = self
                .store
                .lock()
                .map_err(|_| CoreError::Bridge)?
                .related_catalog_cards(&pending.scope, &detail.item, now)
                .unwrap_or_default();
        }
        let store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        if !store
            .profile(&pending.scope)
            .map_err(|_| CoreError::Bridge)?
            .is_some_and(|profile| profile.scope == pending.scope)
        {
            return Err(CoreError::Bridge);
        }
        if !self.metadata_request_current(&pending, &operation)? {
            return Err(CoreError::InvalidInput);
        }
        self.validate_metadata_producers(&store, &pending)?;
        let expected_episode = match expected_episode {
            Ok(value) => value,
            Err(error) => {
                self.forget_metadata_target(
                    &pending.scope,
                    &pending.provider,
                    &pending.kind,
                    &pending.id,
                )?;
                return Err(error);
            }
        };
        self.remember_detail_document(
            &pending.scope,
            &pending.kind,
            &pending.id,
            &pending.provider,
            pending.producer_revision,
            body,
            &detail,
        )?;
        self.remember_detail_cards(&pending.scope, &detail)?;
        if let Some(episode) = expected_episode {
            let proof = viptv_engine::presentation::DetailSnapshot {
                item: episode,
                recommendations: vec![],
                episodes: vec![],
                next_cursor: None,
                seasons: vec![],
            };
            self.remember_detail_cards(&pending.scope, &proof)?;
        }
        let mut snapshots = self
            .detail_snapshots
            .lock()
            .map_err(|_| CoreError::Bridge)?;
        snapshots.retain(|_, (scope, created, _, _)| {
            created.elapsed() < std::time::Duration::from_secs(300)
                && (scope.profile_id != pending.scope.profile_id
                    || scope.revision == pending.scope.revision)
        });
        if snapshots.len() >= 128 {
            snapshots.clear();
        }
        snapshots.insert(
            (
                pending.scope.profile_id.clone(),
                pending.provider.clone(),
                pending.kind,
                pending.id,
                offset,
                limit,
            ),
            (
                pending.scope,
                std::time::Instant::now(),
                detail.clone(),
                pending.producer_revision,
            ),
        );
        serde_json::to_string(&detail).map_err(|_| CoreError::Bridge)
    }
}

pub(super) struct PendingEnrichment {
    scope: viptv_engine::profile::Scope,
    snapshot: viptv_engine::presentation::HomeSnapshot,
    requests: Vec<String>,
}
fn enrich_snapshot(
    snapshot: &mut viptv_engine::presentation::HomeSnapshot,
    cards: &[viptv_engine::presentation::Card],
) {
    for original in snapshot
        .hero
        .iter_mut()
        .chain(snapshot.featured.iter_mut())
        .chain(snapshot.rows.iter_mut().flat_map(|r| r.cards.iter_mut()))
    {
        let identity = original
            .parent_identity
            .as_ref()
            .unwrap_or(&original.identity);
        if let Some(metadata) = cards.iter().find(|c| {
            c.provider_id == original.provider_id
                && (c.metadata_origin == original.metadata_origin
                    || original.metadata_origin.is_none()
                        && origins::shared_lookup(&identity.kind, &identity.id))
                && origins::presentation_matches(original, c)
                && c.identity.id == identity.id
                && c.identity.kind == identity.kind
        }) {
            *original = origins::enrich_verified_card(original, metadata);
        }
    }
}
#[uniffi::export]
impl LocalEngine {
    pub fn plan_home_enrichment(
        &self,
        profile: String,
        home_json: String,
    ) -> Result<String, CoreError> {
        if home_json.len() > 2 * 1024 * 1024 {
            return Err(CoreError::InvalidInput);
        }
        let mut snapshot: viptv_engine::presentation::HomeSnapshot =
            serde_json::from_str(&home_json).map_err(|_| CoreError::InvalidInput)?;
        let current: ProfileConfiguration =
            serde_json::from_str(&self.profile(profile.clone())?).map_err(|_| CoreError::Bridge)?;
        // Legacy library cards stored only the plugin name. Resolve that alias
        // only when the profile has one enabled metadata producer; ambiguity
        // never authorizes merging one addon's opaque ID with another's.
        let connections = self
            .store
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .connections("local")
            .map_err(|_| CoreError::Bridge)?;
        let eligible = viptv_engine::registry::Registry::builtin()
            .connections_for(&current, &connections, Capability::Metadata)
            .map_err(|_| CoreError::InvalidInput)?;
        if eligible.len() == 1 {
            for card in snapshot
                .hero
                .iter_mut()
                .chain(snapshot.featured.iter_mut())
                .chain(
                    snapshot
                        .rows
                        .iter_mut()
                        .flat_map(|row| row.cards.iter_mut()),
                )
            {
                let identity = card.parent_identity.as_ref().unwrap_or(&card.identity);
                if card.provider_id == current.browsing_plugin
                    && origins::shared_lookup(&identity.kind, &identity.id)
                {
                    card.provider_id = eligible[0].id.clone();
                }
            }
        }
        if snapshot.scope != current.scope
            || snapshot.featured.len() > 5
            || snapshot.rows.len() > 14
            || snapshot.rows.iter().any(|r| r.cards.len() > 24)
        {
            return Err(CoreError::InvalidInput);
        }
        if snapshot.featured.is_empty() {
            snapshot.featured = snapshot.hero.iter().cloned().collect();
        }
        let candidates = snapshot
            .featured
            .iter()
            .chain(
                snapshot
                    .rows
                    .iter()
                    .filter(|r| r.id == "continue")
                    .flat_map(|r| r.cards.iter().take(12)),
            )
            .cloned()
            .collect::<Vec<_>>();
        let verified = self.verified_cards.lock().map_err(|_| CoreError::Bridge)?;
        let mut cached = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut needed = Vec::new();
        for card in candidates {
            let identity = card.parent_identity.as_ref().unwrap_or(&card.identity);
            if !seen.insert((
                card.provider_id.clone(),
                identity.kind.clone(),
                identity.id.clone(),
            )) {
                continue;
            }
            if let Some((scope, metadata)) = verified
                .get(&(
                    profile.clone(),
                    card.provider_id.clone(),
                    identity.kind.clone(),
                    identity.id.clone(),
                ))
                .filter(|(scope, _)| scope == &current.scope)
            {
                let _ = scope;
                cached.push(metadata.clone());
            } else {
                let expected = if card.metadata_origin.is_some() {
                    Some(card.clone())
                } else {
                    None
                };
                needed.push((identity.clone(), card.provider_id.clone(), expected));
            }
        }
        drop(verified);
        enrich_snapshot(&mut snapshot, &cached);
        let mut retired = Vec::new();
        self.enrichments
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .retain(|_, pending| {
                if pending.scope.profile_id == profile {
                    retired.extend(pending.requests.iter().cloned());
                    false
                } else {
                    true
                }
            });
        {
            let mut pending = self.details.lock().map_err(|_| CoreError::Bridge)?;
            for id in retired {
                pending.remove(&id);
            }
        }
        let mut staged = Vec::new();
        let mut requests = Vec::new();
        let mut operations = Vec::new();
        for (identity, provider, origin) in needed.into_iter().take(17) {
            if let Ok(plan) = self.plan_detail_owned(
                profile.clone(),
                identity.kind,
                identity.id,
                provider,
                origin,
            ) {
                let request: Value = serde_json::from_str(&plan).map_err(|_| CoreError::Bridge)?;
                if let Some(operation) = request["operation"].as_str() {
                    if let Some(pending) = self
                        .details
                        .lock()
                        .map_err(|_| CoreError::Bridge)?
                        .remove(operation)
                    {
                        staged.push((operation.to_owned(), pending));
                    }
                    operations.push(operation.to_owned());
                    requests.push(request);
                }
            }
        }
        let store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        if !store
            .profile(&current.scope)
            .map_err(|_| CoreError::Bridge)?
            .is_some_and(|profile| profile.scope == current.scope)
        {
            return Err(CoreError::InvalidInput);
        }
        self.details
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .extend(staged);
        let operation = uuid::Uuid::new_v4().to_string();
        let prepared = snapshot.clone();
        let mut pending = self.enrichments.lock().map_err(|_| CoreError::Bridge)?;
        pending.retain(|_, p| p.scope.profile_id != profile);
        if pending.len() >= 12 {
            return Err(CoreError::Bridge);
        }
        pending.insert(
            operation.clone(),
            PendingEnrichment {
                scope: current.scope,
                snapshot,
                requests: operations,
            },
        );
        Ok(json!({"operation":operation,"requests":requests,"prepared":prepared}).to_string())
    }
    pub fn project_home_enrichment(
        &self,
        operation: String,
        raw_results: String,
    ) -> Result<String, CoreError> {
        if raw_results.len() > 34 * 1024 * 1024 {
            return Err(CoreError::InvalidInput);
        }
        let values: Value =
            serde_json::from_str(&raw_results).map_err(|_| CoreError::InvalidInput)?;
        let results = values.as_array().ok_or(CoreError::InvalidInput)?;
        let mut pending = self
            .enrichments
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .remove(&operation)
            .ok_or(CoreError::InvalidInput)?;
        let current: ProfileConfiguration =
            serde_json::from_str(&self.profile(pending.scope.profile_id.clone())?)
                .map_err(|_| CoreError::Bridge)?;
        if current.scope != pending.scope {
            return Err(CoreError::Bridge);
        }
        let mut cards = Vec::new();
        for request in &pending.requests {
            if let Some(body) = results
                .iter()
                .find(|r| r["operation"].as_str() == Some(request))
                .and_then(|r| r["body"].as_str())
            {
                if let Ok(detail) = self.project_detail(request.clone(), body.into(), 0) {
                    if let Ok(detail) =
                        serde_json::from_str::<viptv_engine::presentation::DetailSnapshot>(&detail)
                    {
                        cards.push(detail.item);
                    }
                }
            }
            self.details
                .lock()
                .map_err(|_| CoreError::Bridge)?
                .remove(request);
        }
        enrich_snapshot(&mut pending.snapshot, &cards);
        let current: ProfileConfiguration =
            serde_json::from_str(&self.profile(pending.scope.profile_id.clone())?)
                .map_err(|_| CoreError::Bridge)?;
        if current.scope != pending.scope {
            return Err(CoreError::Bridge);
        }
        serde_json::to_string(&pending.snapshot).map_err(|_| CoreError::Bridge)
    }
}

impl LocalEngine {
    pub(super) fn merge_cached_presentation(
        &self,
        scope: &Scope,
        cards: &mut [viptv_engine::presentation::Card],
    ) -> Result<(), CoreError> {
        let verified = self.verified_cards.lock().map_err(|_| CoreError::Bridge)?;
        for card in cards {
            let identity = card.parent_identity.as_ref().unwrap_or(&card.identity);
            let exact = verified
                .get(&(
                    scope.profile_id.clone(),
                    card.provider_id.clone(),
                    identity.kind.clone(),
                    identity.id.clone(),
                ))
                .filter(|(cached_scope, metadata)| {
                    cached_scope == scope
                        && origins::presentation_matches(card, metadata)
                        && (metadata.metadata_origin == card.metadata_origin
                            || card.metadata_origin.is_none()
                                && origins::shared_lookup(&identity.kind, &identity.id))
                });
            let legacy = if exact.is_none()
                && card.provider_id == "stremio"
                && card.metadata_origin.is_none()
                && origins::shared_lookup(&identity.kind, &identity.id)
            {
                let mut matches = verified
                    .iter()
                    .filter(|((profile, _, kind, id), (cached_scope, _))| {
                        profile == &scope.profile_id
                            && kind == &identity.kind
                            && id == &identity.id
                            && cached_scope == scope
                    })
                    .map(|(_, value)| value);
                let first = matches.next();
                if matches.next().is_none() {
                    first
                } else {
                    None
                }
            } else {
                None
            };
            if let Some((cached_scope, metadata)) = exact.or(legacy) {
                let _ = cached_scope;
                if !origins::presentation_matches(card, metadata) {
                    continue;
                }
                card.provider_id = metadata.provider_id.clone();
                *card = origins::enrich_verified_card(card, metadata);
            }
        }
        Ok(())
    }
}

impl LocalEngine {
    pub(super) fn forget_metadata_profile(&self, profile: &str) -> Result<(), CoreError> {
        self.details
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .retain(|_, pending| pending.scope.profile_id != profile);
        self.metadata_requests
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .retain(|(owner, _, _, _), _| owner != profile);
        self.enrichments
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .retain(|_, pending| pending.scope.profile_id != profile);
        self.detail_documents
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .retain(|(owner, _, _, _), _| owner != profile);
        self.detail_snapshots
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .retain(|(owner, _, _, _, _, _), _| owner != profile);
        self.verified_cards
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .retain(|(owner, _, _, _), _| owner != profile);
        Ok(())
    }
}

impl LocalEngine {
    fn plan_detail_owned(
        &self,
        profile: String,
        kind: String,
        id: String,
        preferred: String,
        expected: Option<viptv_engine::presentation::Card>,
    ) -> Result<String, CoreError> {
        if id.starts_with("simkl:") {
            return Err(CoreError::InvalidInput);
        }
        let store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        let selected = store
            .profile(&local_scope(&profile))
            .map_err(|_| CoreError::Bridge)?
            .ok_or(CoreError::InvalidInput)?;
        let issuer = if let Some(card) = expected.as_ref() {
            let target = card.parent_identity.as_ref().unwrap_or(&card.identity);
            if target.kind != kind || target.id != id {
                return Err(CoreError::InvalidInput);
            }
            Some(origins::metadata_issuer(&store, &selected.scope, card)?)
        } else {
            if !origins::shared_lookup(&kind, &id) {
                return Err(CoreError::InvalidInput);
            }
            None
        };
        let public_delegation = expected
            .as_ref()
            .is_some_and(origins::public_metadata_identity);

        let connections = store.connections("local").map_err(|_| CoreError::Bridge)?;
        let mut eligible = connections
            .iter()
            .filter(|c| {
                c.enabled
                    && c.plugin_id == selected.browsing_plugin
                    && selected.assignments.iter().any(|a| {
                        a.connection_id == c.id && a.enabled.contains(&Capability::Metadata)
                    })
            })
            .collect::<Vec<_>>();
        if preferred.starts_with("addon:") && !public_delegation {
            // A provider-owned opaque ID must never fall through to a sibling.
            eligible.retain(|c| c.id == preferred);
        } else if !public_delegation
            && !preferred.is_empty()
            && preferred != selected.browsing_plugin
        {
            return Err(CoreError::InvalidInput);
        }
        eligible.sort_by_key(|c| if c.id == preferred { 0 } else { 1 });
        for connection in eligible {
            let origin = origins::current_origin(
                &store,
                &selected.scope,
                &connection.id,
                Capability::Metadata,
            )?;
            let Ok(stored) = self.addon(&connection.id) else {
                continue;
            };
            if let Some(url) = stremio::meta_url(&stored.manifest_url, &stored.manifest, &kind, &id)
            {
                let operation = uuid::Uuid::new_v4().to_string();
                let mut pending = self.details.lock().map_err(|_| CoreError::Bridge)?;
                pending.retain(|_, p| p.scope.profile_id != profile);
                if pending.len() >= 64 {
                    return Err(CoreError::Bridge);
                }
                let key = (
                    profile.clone(),
                    connection.id.clone(),
                    kind.clone(),
                    id.clone(),
                );
                let mut latest = self
                    .metadata_requests
                    .lock()
                    .map_err(|_| CoreError::Bridge)?;
                if !latest.contains_key(&key)
                    && latest
                        .keys()
                        .filter(|(owner, _, _, _)| owner == &profile)
                        .count()
                        >= 128
                {
                    latest.retain(|(owner, _, _, _), _| owner != &profile);
                }
                latest.insert(key, operation.clone());
                pending.insert(
                    operation.clone(),
                    PendingDetail {
                        scope: selected.scope.clone(),
                        created: std::time::Instant::now(),
                        kind: kind.clone(),
                        id: id.clone(),
                        provider: connection.id.clone(),
                        origin,
                        producer_revision: connection.revision,
                        issuer: issuer.clone(),
                        expected,
                    },
                );
                return Ok(json!({"operation":operation,"url":url}).to_string());
            }
        }
        Err(CoreError::InvalidInput)
    }
}

impl LocalEngine {
    fn validate_metadata_producers(
        &self,
        store: &Store,
        pending: &PendingDetail,
    ) -> Result<(), CoreError> {
        if !store
            .profile(&pending.scope)
            .map_err(|_| CoreError::Bridge)?
            .is_some_and(|p| p.scope == pending.scope)
        {
            return Err(CoreError::InvalidInput);
        }
        let producer =
            origins::require_origin(store, &pending.scope, &pending.origin, Capability::Metadata)?;
        if producer.revision != pending.producer_revision {
            return Err(CoreError::InvalidInput);
        }
        if let Some(issuer) = pending.issuer.as_ref() {
            issuer.validate(store, &pending.scope)?;
        }
        Ok(())
    }

    fn metadata_request_current(
        &self,
        pending: &PendingDetail,
        operation: &str,
    ) -> Result<bool, CoreError> {
        Ok(pending.created.elapsed().as_secs() < 120
            && self
                .metadata_requests
                .lock()
                .map_err(|_| CoreError::Bridge)?
                .get(&(
                    pending.scope.profile_id.clone(),
                    pending.provider.clone(),
                    pending.kind.clone(),
                    pending.id.clone(),
                ))
                .is_some_and(|current| current == operation))
    }
}
