use super::*;
use viptv_engine::presentation::{Card, HomeRow, RowState};
impl LocalEngine {
    pub(super) fn home_revision(&self, scope: &Scope) -> Result<u64, CoreError> {
        Ok(scope.revision.saturating_add(
            self.store
                .lock()
                .map_err(|_| CoreError::Bridge)?
                .library_revision(scope)
                .map_err(|_| CoreError::Bridge)?,
        ))
    }
    pub(super) fn favorites_row(&self, scope: &Scope) -> Result<Option<HomeRow>, CoreError> {
        let mut page = self
            .store
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .favorites(scope, 0, 12)
            .map_err(|_| CoreError::Bridge)?;
        self.merge_cached_presentation(scope, &mut page.cards)?;
        if page.cards.is_empty() {
            return Ok(None);
        }
        Ok(Some(HomeRow {
            browse: Some(viptv_engine::home_browse::HomeBrowseAction::library()),
            id: "favorites".into(),
            title: "My List".into(),
            provider_id: "library".into(),
            state: RowState::Ready,
            cards: page.cards,
            next_cursor: page.next_cursor,
            message: None,
        }))
    }
}
#[uniffi::export]
impl LocalEngine {
    pub fn library_membership(&self, profile: String) -> Result<String, CoreError> {
        let store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        let scope = store
            .profile(&local_scope(&profile))
            .map_err(|_| CoreError::Bridge)?
            .ok_or(CoreError::InvalidInput)?
            .scope;
        serde_json::to_string(
            &store
                .library_membership(&scope)
                .map_err(|_| CoreError::Bridge)?,
        )
        .map_err(|_| CoreError::Bridge)
    }
    pub fn remove_favorite_for_origin(
        &self,
        profile: String,
        revision: u64,
        card_json: String,
    ) -> Result<(), CoreError> {
        let expected = origins::expected_card(&card_json)?;
        let mut scope = local_scope(&profile);
        scope.revision = revision;
        let mut store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        if let Some(card) = store
            .favorite_for_origin(&scope, &expected)
            .map_err(|_| CoreError::InvalidInput)?
        {
            origins::consistent_card(&expected, &card)?;
            store
                .remove_favorite_stored(&scope, &card)
                .map_err(|_| CoreError::InvalidInput)
        } else {
            store
                .remove_favorite_for_origin(
                    &scope,
                    &expected.identity.kind,
                    &expected.identity.id,
                    expected
                        .metadata_origin
                        .as_ref()
                        .ok_or(CoreError::InvalidInput)?,
                )
                .map_err(|_| CoreError::InvalidInput)
        }
    }
    pub fn library_page(&self, profile: String, query_json: String) -> Result<String, CoreError> {
        use viptv_engine::{capabilities::Availability, library::*};
        if query_json.len() > 8192 {
            return Err(CoreError::InvalidInput);
        }
        let query: LibraryQuery =
            serde_json::from_str(&query_json).map_err(|_| CoreError::InvalidInput)?;
        if query.source != "favorites" {
            return Err(CoreError::InvalidInput);
        }
        let (scope, revision, mut entries) = {
            let store = self.store.lock().map_err(|_| CoreError::Bridge)?;
            let scope = store
                .profile(&local_scope(&profile))
                .map_err(|_| CoreError::Bridge)?
                .ok_or(CoreError::InvalidInput)?
                .scope;
            let revision = scope.revision.saturating_add(
                store
                    .library_revision(&scope)
                    .map_err(|_| CoreError::Bridge)?,
            );
            let entries = store
                .library_entries(&scope)
                .map_err(|_| CoreError::InvalidInput)?;
            (scope, revision, entries)
        };
        for entry in &mut entries {
            self.merge_cached_presentation(&scope, std::slice::from_mut(&mut entry.card))?;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| CoreError::Bridge)?
            .as_millis() as u64;
        let lower = added_lower_bound(&query, now);
        let snapshot = project(
            scope,
            revision,
            query,
            entries,
            vec![LibrarySource {
                id: "favorites".into(),
                label: "Saved titles".into(),
                availability: Availability::Available,
                reason: None,
            }],
            now,
            lower,
        )
        .map_err(|_| CoreError::InvalidInput)?;
        let store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        let current = store
            .profile(&snapshot.scope)
            .map_err(|_| CoreError::Bridge)?
            .ok_or(CoreError::InvalidInput)?;
        if current.scope != snapshot.scope
            || current.scope.revision.saturating_add(
                store
                    .library_revision(&snapshot.scope)
                    .map_err(|_| CoreError::Bridge)?,
            ) != snapshot.revision
        {
            return Err(CoreError::InvalidInput);
        }
        serde_json::to_string(&snapshot).map_err(|_| CoreError::Bridge)
    }
    pub fn library(&self, profile: String, offset: u32, limit: u32) -> Result<String, CoreError> {
        let (scope, mut page) = {
            let store = self.store.lock().map_err(|_| CoreError::Bridge)?;
            let scope = store
                .profile(&local_scope(&profile))
                .map_err(|_| CoreError::Bridge)?
                .ok_or(CoreError::InvalidInput)?
                .scope;
            let page = store
                .favorites(&scope, offset, limit)
                .map_err(|_| CoreError::InvalidInput)?;
            (scope, page)
        };
        self.merge_cached_presentation(&scope, &mut page.cards)?;
        serde_json::to_string(&page).map_err(|_| CoreError::Bridge)
    }
    pub fn save_favorite(
        &self,
        profile: String,
        revision: u64,
        card_json: String,
    ) -> Result<(), CoreError> {
        if card_json.len() > 64 * 1024 {
            return Err(CoreError::InvalidInput);
        }
        let mut card: Card =
            serde_json::from_str(&card_json).map_err(|_| CoreError::InvalidInput)?;
        if !origins::stremio_card_allowed(&card) {
            return Err(CoreError::InvalidInput);
        }
        origins::strip_reserved_claims(&mut card);
        let mut scope = local_scope(&profile);
        scope.revision = revision;
        let mut store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        if !store
            .profile(&scope)
            .map_err(|_| CoreError::Bridge)?
            .is_some_and(|profile| profile.scope == scope)
        {
            return Err(CoreError::InvalidInput);
        }
        if let Some(origin) = card.metadata_origin.as_ref() {
            if origin.connection_id.as_deref() != Some(card.provider_id.as_str()) {
                return Err(CoreError::InvalidInput);
            }
            match self.fresh_metadata_card(&store, &scope, &card) {
                Ok(Some(proof)) => {
                    return store
                        .save_favorite_verified(&scope, &proof)
                        .map_err(|_| CoreError::InvalidInput);
                }
                Ok(None) => {}
                Err(error) => {
                    let trusted = store
                        .favorite_for_origin(&scope, &card)
                        .map_err(|_| CoreError::InvalidInput)?
                        .ok_or(error)?;
                    origins::consistent_card(&card, &trusted)?;
                    return store
                        .save_favorite(&scope, &trusted)
                        .map_err(|_| CoreError::InvalidInput);
                }
            }
        }
        store
            .save_favorite(&scope, &card)
            .map_err(|_| CoreError::InvalidInput)
    }
    pub fn remove_favorite(
        &self,
        profile: String,
        revision: u64,
        kind: String,
        id: String,
    ) -> Result<(), CoreError> {
        let mut scope = local_scope(&profile);
        scope.revision = revision;
        self.store
            .lock()
            .map_err(|_| CoreError::Bridge)?
            .remove_favorite(&scope, &kind, &id)
            .map_err(|_| CoreError::InvalidInput)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    #[derive(Default)]
    struct Secrets(Mutex<std::collections::HashMap<String, String>>);
    impl SecretStore for Secrets {
        fn get(&self, key: String) -> Result<Option<String>, CoreError> {
            Ok(self.0.lock().unwrap().get(&key).cloned())
        }
        fn put(&self, key: String, value: String) -> Result<(), CoreError> {
            self.0.lock().unwrap().insert(key, value);
            Ok(())
        }
        fn remove(&self, key: String) -> Result<(), CoreError> {
            self.0.lock().unwrap().remove(&key);
            Ok(())
        }
    }
    #[test]
    fn local_library_bridge_round_trips_my_list_across_restart_and_prepared_home() {
        let dir =
            std::env::temp_dir().join(format!("viptv-local-library-{}", uuid::Uuid::new_v4()));
        let path = dir.join("library.sqlite").to_string_lossy().to_string();
        let secrets = Arc::new(Secrets::default());
        let engine = LocalEngine::new(path.clone(), secrets.clone()).unwrap();
        let mut card = json!({"identity":{"id":"opaque-film","kind":"movie","ids":[{"namespace":"imdb","value":"tt1234567"}],"episode":null,"edition":null},"provider_id":"stremio","title":"My film","subtitle":null,"description":null,"poster":null,"landscape":"https://images.example/frame.jpg","logo":null,"progress":null,"badges":[],"parent_identity":null});
        engine
            .save_favorite("default".into(), 0, card.to_string())
            .unwrap();
        let page: Value =
            serde_json::from_str(&engine.library("default".into(), 0, 40).unwrap()).unwrap();
        assert!(page["cards"][0]["added_at_millis"].as_u64().is_some());
        card["added_at_millis"] = page["cards"][0]["added_at_millis"].clone();
        assert_eq!(page["cards"][0], card);
        let home: Value = serde_json::from_str(&engine.home("default".into()).unwrap()).unwrap();
        assert_eq!(home["rows"][0]["id"], "favorites");
        assert_eq!(home["revision"], 1);
        let plan: Value =
            serde_json::from_str(&engine.plan_home("default".into()).unwrap()).unwrap();
        let prepared: Value = serde_json::from_str(
            &engine
                .prepared_home(plan["id"].as_str().unwrap().into())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(prepared["rows"][0]["cards"][0], card);
        let projected: Value = serde_json::from_str(
            &engine
                .project_home(plan["id"].as_str().unwrap().into(), "[]".into())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(projected["rows"][0]["cards"][0], card);
        assert!(
            engine
                .remove_favorite("default".into(), 9, "movie".into(), "opaque-film".into())
                .is_err()
        );
        drop(engine);
        let engine = LocalEngine::new(path, secrets).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&engine.library("default".into(), 0, 40).unwrap())
                .unwrap()["cards"][0],
            card
        );
        engine
            .remove_favorite("default".into(), 0, "movie".into(), "opaque-film".into())
            .unwrap();
        assert!(
            serde_json::from_str::<Value>(&engine.library("default".into(), 0, 40).unwrap())
                .unwrap()["cards"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        drop(engine);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
