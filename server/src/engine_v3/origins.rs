//! Stable metadata lookup provenance. Revisions fence access; incarnations identify installs.
use super::*;
use viptv_engine::{identity::MediaIdentity, origin::MetadataOrigin};

pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS engine_addon_incarnations(addon_id INTEGER PRIMARY KEY REFERENCES addons(id) ON DELETE CASCADE,incarnation TEXT NOT NULL UNIQUE);
        DELETE FROM engine_addon_incarnations WHERE NOT EXISTS(SELECT 1 FROM addons WHERE addons.id=engine_addon_incarnations.addon_id);
        INSERT OR IGNORE INTO engine_addon_incarnations(addon_id,incarnation) SELECT id,lower(hex(randomblob(16))) FROM addons;
        CREATE TRIGGER IF NOT EXISTS engine_addon_incarnation_insert AFTER INSERT ON addons BEGIN INSERT INTO engine_addon_incarnations(addon_id,incarnation)VALUES(NEW.id,lower(hex(randomblob(16)))); END;
        CREATE TRIGGER IF NOT EXISTS engine_addon_incarnation_delete AFTER DELETE ON addons BEGIN DELETE FROM engine_addon_incarnations WHERE addon_id=OLD.id;END;")
}

pub(crate) fn for_provider_db(
    db: &Connection,
    account: i64,
    provider: &str,
) -> Result<Option<MetadataOrigin>, ApiError> {
    if provider == "simkl" {
        return Ok(Some(MetadataOrigin {
            provider_id: "simkl".into(),
            connection_id: None,
            incarnation: None,
        }));
    }
    let Some(id) = provider
        .strip_prefix("addon:")
        .and_then(|id| id.parse::<i64>().ok())
        .filter(|id| *id > 0)
    else {
        return Ok(None);
    };
    if provider != format!("addon:{id}") {
        return Ok(None);
    }
    let token:Option<String>=db.query_row("SELECT i.incarnation FROM engine_addon_incarnations i JOIN addons a ON a.id=i.addon_id WHERE a.id=?1 AND a.account_id=?2 AND a.enabled=1",params![id,account],|row|row.get(0)).optional().map_err(db_error)?;
    Ok(token.map(|incarnation| MetadataOrigin {
        provider_id: "stremio".into(),
        connection_id: Some(provider.to_owned()),
        incarnation: Some(incarnation),
    }))
}

/// The profile's metadata integrations, read once so every Continue Watching
/// row can be attributed without asking the provider. Add-on manifests may be
/// sealed in the vault, so they come from the add-on store rather than the
/// database the queue holds.
pub(crate) struct Serving {
    account: i64,
    profile: i64,
    simkl_browsing: bool,
    linked: bool,
    addons: Vec<(i64, Value)>,
}
impl Serving {
    pub(crate) fn read(
        app: &App,
        lease: &ResourceLease,
        profile: i64,
    ) -> Result<Self, ApiError> {
        let account = lease.principal.account_id().ok_or_else(auth::forbidden)?;
        let linked = crate::simkl::linked(app, profile)?;
        let (configuration, _) = super::configuration::read(app, lease, profile)?;
        // The queue serves an imported title from the add-ons even when the
        // profile browses Simkl (backend#60), so read that scope's add-ons.
        let addons = match super::search::metadata_scope(
            app.clone(),
            lease,
            profile,
            "stremio",
            None,
        ) {
            Ok((app, _, _)) => app
                .addons
                .entries()
                .unwrap_or_default()
                .into_iter()
                .map(|(id, _, manifest)| (id, manifest))
                .collect(),
            Err(_) => Vec::new(),
        };
        Ok(Self {
            account,
            profile,
            simkl_browsing: configuration.browsing_plugin == "simkl",
            linked,
            addons,
        })
    }

    /// The metadata scope that serves `identity`: a `simkl:` title, and one the
    /// profile's own Simkl link identifies, stay on Simkl; a shared title comes
    /// from the first assigned add-on that publishes it (the fallback of
    /// backend#60). A Stremio import writes no provider provenance, so the queue
    /// attributes its rows from here (backend#72).
    pub(crate) fn origin(
        &self,
        db: &Connection,
        identity: &MediaIdentity,
    ) -> Result<Option<MetadataOrigin>, ApiError> {
        let (kind, id) = (identity.kind.as_str(), identity.id.as_str());
        // Only a Simkl link can carry a mapping, and only Simkl browsing reads it.
        if id.starts_with("simkl:")
            || self.simkl_browsing && self.linked && self.identifies(db, identity)?
        {
            return Ok(Some(MetadataOrigin {
                provider_id: "simkl".into(),
                connection_id: None,
                incarnation: None,
            }));
        }
        // An add-on id that only one connector knows cannot be attributed from
        // its shape: two add-ons may publish different titles under it.
        if !viptv_engine::origin::shared_identity(identity) {
            return Ok(None);
        }
        for (addon, manifest) in &self.addons {
            if crate::addon::supports(manifest, "meta", kind, id) {
                return for_provider_db(db, self.account, &format!("addon:{addon}"));
            }
        }
        Ok(None)
    }

    fn identifies(&self, db: &Connection, identity: &MediaIdentity) -> Result<bool, ApiError> {
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM simkl_import_entries WHERE profile_id=?1 AND local_kind=?2 AND local_id=?3)",
            params![self.profile, identity.kind, identity.id],
            |r| r.get(0),
        )
        .map_err(db_error)
    }
}

/// Capability selection remains with each producer; this proves current ownership and install.
pub(crate) fn validate_db(
    db: &Connection,
    account: i64,
    origin: &MetadataOrigin,
) -> Result<(), ApiError> {
    origin
        .validate()
        .map_err(|_| ApiError::from("Invalid metadata origin"))?;
    let provider = if origin.provider_id == "simkl" {
        "simkl"
    } else {
        origin
            .connection_id
            .as_deref()
            .ok_or_else(auth::forbidden)?
    };
    if for_provider_db(db, account, provider)?.as_ref() != Some(origin) {
        return Err(auth::forbidden());
    }
    Ok(())
}
pub(crate) fn stamp(
    app: &App,
    cards: &mut [viptv_engine::presentation::Card],
    provider: &str,
) -> Result<(), ApiError> {
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.request_lease().validate(&db)?;
    let account = app.identity().account_id().ok_or_else(auth::forbidden)?;
    let origin = for_provider_db(&db, account, provider)?;
    if provider.starts_with("addon:") && origin.is_none() {
        return Err(auth::forbidden());
    }
    for card in cards {
        card.metadata_origin = origin.clone();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn incarnation_survives_configuration_but_never_id_reuse() {
        let app = crate::auth_integration_tests::fixture();
        let db = app.db.lock().unwrap();
        db.execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id)VALUES(301,'Owned','https://fixture.invalid/manifest.json','{}',1)",[]).unwrap();
        let old = for_provider_db(&db, 1, "addon:301").unwrap().unwrap();
        db.execute(
            "UPDATE addons SET name='Changed',manifest='{}' WHERE id=301",
            [],
        )
        .unwrap();
        assert_eq!(
            for_provider_db(&db, 1, "addon:301").unwrap(),
            Some(old.clone())
        );
        assert!(for_provider_db(&db, 2, "addon:301").unwrap().is_none());
        db.execute("UPDATE addons SET enabled=0 WHERE id=301", [])
            .unwrap();
        assert!(validate_db(&db, 1, &old).is_err());
        db.execute("UPDATE addons SET enabled=1 WHERE id=301", [])
            .unwrap();
        assert!(validate_db(&db, 1, &old).is_ok());
        db.execute("DELETE FROM addons WHERE id=301", []).unwrap();
        db.execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id)VALUES(301,'Reinstalled','https://fixture.invalid/manifest.json','{}',1)",[]).unwrap();
        assert_ne!(
            for_provider_db(&db, 1, "addon:301").unwrap(),
            Some(old.clone())
        );
        assert!(validate_db(&db, 1, &old).is_err());
    }
}

pub(crate) fn parse_origin(raw: Option<&str>) -> Result<Option<MetadataOrigin>, ApiError> {
    raw.map(|raw| {
        if raw.len() > 2048 {
            return Err(ApiError::from("Invalid metadata origin"));
        }
        let value: MetadataOrigin =
            serde_json::from_str(raw).map_err(|_| ApiError::from("Invalid metadata origin"))?;
        value
            .validate()
            .map_err(|_| ApiError::from("Invalid metadata origin"))?;
        Ok(value)
    })
    .transpose()
}
pub(crate) fn parse_card(
    raw: Option<&str>,
) -> Result<Option<viptv_engine::presentation::Card>, ApiError> {
    raw.map(|raw| {
        if raw.len() > 65536 {
            return Err(ApiError::from("Invalid metadata expectation"));
        }
        let value: viptv_engine::presentation::Card = serde_json::from_str(raw)
            .map_err(|_| ApiError::from("Invalid metadata expectation"))?;
        value
            .validate_library()
            .map_err(|_| ApiError::from("Invalid metadata expectation"))?;
        Ok(value)
    })
    .transpose()
}
/// Match old scalar routing with the exact modern install token, never silently reinterpret it.
pub(crate) fn selectors(
    app: &App,
    provider: Option<&str>,
    addon: Option<i64>,
    origin: Option<&MetadataOrigin>,
) -> Result<(Option<String>, Option<i64>), ApiError> {
    let Some(origin) = origin else {
        return Ok((provider.map(str::to_owned), addon));
    };
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.request_lease().validate(&db)?;
    validate_db(
        &db,
        app.identity().account_id().ok_or_else(auth::forbidden)?,
        origin,
    )?;
    let id = origin
        .connection_id
        .as_deref()
        .and_then(|id| id.strip_prefix("addon:"))
        .and_then(|id| id.parse::<i64>().ok());
    if provider.is_some_and(|provider| provider != origin.provider_id)
        || addon.is_some_and(|addon| Some(addon) != id)
    {
        return Err("Conflicting metadata origin".into());
    }
    Ok((Some(origin.provider_id.clone()), id))
}
pub(crate) fn check_expected(
    expected: Option<&viptv_engine::presentation::Card>,
    actual: &viptv_engine::presentation::Card,
) -> Result<(), ApiError> {
    if expected
        .is_some_and(|expected| !viptv_engine::origin::metadata_assertions_match(expected, actual))
    {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Metadata identity changed. Select this title again.".into(),
        ));
    }
    Ok(())
}
/// The exact metadata provider is host selected; client external IDs are consistency checks only.
pub(crate) async fn verified_card(
    app: &App,
    expected: &viptv_engine::presentation::Card,
) -> Result<viptv_engine::presentation::Card, ApiError> {
    expected
        .validate_library()
        .map_err(|_| ApiError::from("Invalid metadata expectation"))?;
    let origin = expected
        .metadata_origin
        .as_ref()
        .ok_or("Metadata origin is required")?;
    let (provider, addon) = selectors(app, None, None, Some(origin))?;
    let parent = expected
        .parent_identity
        .as_ref()
        .unwrap_or(&expected.identity);
    for offset in (0..=2048).step_by(128) {
        let page = super::catalog::DetailPage {
            offset,
            limit: Some(128),
            provider_id: provider.clone(),
            addon_id: addon,
            metadata_origin: Some(
                serde_json::to_string(origin)
                    .map_err(|_| ApiError::from("Invalid metadata origin"))?,
            ),
            card: if expected.identity.episode.is_none() {
                Some(
                    serde_json::to_string(expected)
                        .map_err(|_| ApiError::from("Invalid metadata expectation"))?,
                )
            } else {
                None
            },
        };
        let axum::Json(value) = tokio::time::timeout(
            Duration::from_secs(15),
            super::catalog::detail(
                State(app.clone()),
                Extension(app.request_lease()),
                Path((parent.kind.clone(), parent.id.clone())),
                Query(page),
            ),
        )
        .await
        .map_err(|_| ApiError::from("Metadata timed out"))??;
        let detail: viptv_engine::presentation::DetailSnapshot =
            serde_json::from_value(value).map_err(|_| ApiError::from("Metadata unavailable"))?;
        let candidate = if expected.identity.episode.is_none() {
            Some(detail.item)
        } else {
            detail
                .episodes
                .into_iter()
                .find(|card| card.identity.id == expected.identity.id)
        };
        if let Some(card) = candidate {
            check_expected(Some(expected), &card)?;
            return Ok(card);
        }
        if detail.next_cursor.is_none() {
            break;
        }
    }
    Err(ApiError(
        StatusCode::CONFLICT,
        "Episode metadata changed. Select this title again.".into(),
    ))
}

pub(crate) enum MetadataEvidence {
    Verified(viptv_engine::presentation::Card, MetadataProof),
    Unverified(viptv_engine::presentation::Card),
}
/// Catalog provenance permits a private lookup record, not a shared external-ID alias.
pub(crate) async fn persistence_card(
    app: &App,
    expected: &viptv_engine::presentation::Card,
) -> Result<MetadataEvidence, ApiError> {
    let lease = app.request_lease();
    let auth::Principal::Account {
        profile_id: Some(profile),
        ..
    } = lease.principal
    else {
        return Err(auth::forbidden());
    };
    let origin = expected
        .metadata_origin
        .as_ref()
        .ok_or("Metadata origin is required")?;
    let (provider, addon) = selectors(app, None, None, Some(origin))?;
    let (config, connections) = super::configuration::read(app, &lease, profile)?;
    let connection = if origin.provider_id == "simkl" {
        format!("simkl:{profile}")
    } else {
        origin.connection_id.clone().ok_or_else(auth::forbidden)?
    };
    if !connections
        .iter()
        .any(|entry| entry.id == connection && entry.enabled)
        || !config
            .assignments
            .iter()
            .any(|entry| entry.connection_id == connection && !entry.enabled.is_empty())
    {
        return Err(auth::forbidden());
    }
    if super::search::metadata_scope(
        app.clone(),
        &lease,
        profile,
        provider.as_deref().ok_or_else(auth::forbidden)?,
        addon,
    )
    .is_err()
    {
        super::configuration::validate_revision(app, profile, config.scope.revision)?;
        selectors(app, None, None, Some(origin))?;
        return Ok(MetadataEvidence::Unverified(expected.clone()));
    }
    let captured = MetadataProof::capture(app, expected.clone())?;
    let result = verified_card(app, expected).await;
    super::configuration::validate_revision(app, profile, config.scope.revision)?;
    selectors(app, None, None, Some(origin))?;
    match result {
        Ok(card) => {
            let proof = MetadataProof {
                card: card.clone(),
                configuration: captured.configuration,
                profile: captured.profile,
                profile_revision: captured.profile_revision,
                delegates: captured.delegates,
            };
            proof.validate(
                &*app
                    .db
                    .lock()
                    .map_err(|_| ApiError::from("Database unavailable"))?,
                &lease,
            )?;
            Ok(MetadataEvidence::Verified(card, proof))
        }
        Err(error)
            if matches!(
                error.0,
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::CONFLICT
            ) =>
        {
            Err(error)
        }
        Err(_) => Ok(MetadataEvidence::Unverified(expected.clone())),
    }
}

#[derive(Clone)]
pub(crate) struct MetadataProof {
    pub card: viptv_engine::presentation::Card,
    pub configuration: Option<[u8; 32]>,
    profile: i64,
    profile_revision: u64,
    delegates: Vec<(String, [u8; 32])>,
}
impl MetadataProof {
    pub(crate) async fn verified(
        app: &App,
        expected: &viptv_engine::presentation::Card,
    ) -> Result<Self, ApiError> {
        let captured = Self::capture(app, expected.clone())?;
        let card = verified_card(app, expected).await?;
        let proof = Self {
            card,
            configuration: captured.configuration,
            profile: captured.profile,
            profile_revision: captured.profile_revision,
            delegates: captured.delegates,
        };
        proof.validate(
            &*app
                .db
                .lock()
                .map_err(|_| ApiError::from("Database unavailable"))?,
            &app.request_lease(),
        )?;
        Ok(proof)
    }
    pub(crate) fn capture(
        app: &App,
        card: viptv_engine::presentation::Card,
    ) -> Result<Self, ApiError> {
        let lease = app.request_lease();
        let auth::Principal::Account {
            profile_id: Some(profile),
            ..
        } = lease.principal
        else {
            return Err(auth::forbidden());
        };
        let origin = card
            .metadata_origin
            .as_ref()
            .ok_or("Metadata origin is required")?;
        let (provider, addon) = selectors(app, None, None, Some(origin))?;
        let (metadata, _, selected_revision) = super::search::metadata_scope(
            app.clone(),
            &lease,
            profile,
            provider.as_deref().ok_or_else(auth::forbidden)?,
            addon,
        )?;
        let lookup = card.parent_identity.as_ref().unwrap_or(&card.identity);
        let mut ids = if origin.provider_id == "stremio" {
            metadata
                .addons
                .entries()?
                .into_iter()
                .filter(|(_, _, manifest)| {
                    crate::addon::supports(manifest, "meta", &lookup.kind, &lookup.id)
                })
                .map(|(id, _, _)| id)
                .take(32)
                .collect::<Vec<_>>()
        } else {
            vec![]
        };
        ids.sort_unstable();
        ids.dedup();
        let db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        let origin = card
            .metadata_origin
            .as_ref()
            .ok_or("Metadata origin is required")?;
        validate_db(
            &db,
            app.identity().account_id().ok_or_else(auth::forbidden)?,
            origin,
        )?;
        let configuration = origin
            .connection_id
            .as_deref()
            .map(|id| crate::sources::source_configuration(&db, id))
            .transpose()?
            .flatten();
        let lease = app.request_lease();
        let auth::Principal::Account {
            profile_id: Some(profile),
            ..
        } = lease.principal
        else {
            return Err(auth::forbidden());
        };
        validate_assignment_db(&db, &lease, origin)?;
        let profile_revision: u64 = db
            .query_row(
                "SELECT revision FROM engine_profile_configuration WHERE profile_id=?1",
                [profile],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?
            .unwrap_or(0);
        if profile_revision != selected_revision {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "Profile configuration changed".into(),
            ));
        }
        let delegates = ids
            .into_iter()
            .map(|id| {
                let source = format!("addon:{id}");
                let proof = crate::sources::source_configuration(&db, &source)?
                    .ok_or_else(auth::forbidden)?;
                Ok((source, proof))
            })
            .collect::<Result<Vec<_>, ApiError>>()?;
        Ok(Self {
            card,
            configuration,
            profile,
            profile_revision,
            delegates,
        })
    }
    pub(crate) fn validate(&self, db: &Connection, lease: &ResourceLease) -> Result<(), ApiError> {
        lease.validate(db)?;
        let auth::Principal::Account {
            profile_id: Some(profile),
            ..
        } = lease.principal
        else {
            return Err(auth::forbidden());
        };
        let revision: u64 = db
            .query_row(
                "SELECT revision FROM engine_profile_configuration WHERE profile_id=?1",
                [profile],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?
            .unwrap_or(0);
        if profile != self.profile || revision != self.profile_revision {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "Profile configuration changed".into(),
            ));
        }
        let origin = self
            .card
            .metadata_origin
            .as_ref()
            .ok_or_else(auth::forbidden)?;
        validate_db(
            db,
            lease.principal.account_id().ok_or_else(auth::forbidden)?,
            origin,
        )?;
        if let Some(source) = origin.connection_id.as_deref() {
            if crate::sources::source_configuration(db, source)? != self.configuration {
                return Err(ApiError(
                    StatusCode::CONFLICT,
                    "Metadata integrations changed. Try again.".into(),
                ));
            }
        }
        for (source, expected) in &self.delegates {
            if crate::sources::source_configuration(db, source)? != Some(*expected) {
                return Err(ApiError(
                    StatusCode::CONFLICT,
                    "Metadata integrations changed. Try again.".into(),
                ));
            }
        }
        kids::scoped::require_card_db(db, &lease.principal, &self.card)
    }
}

pub(crate) fn check_expected_at(
    app: &App,
    expected: Option<&viptv_engine::presentation::Card>,
    actual: &viptv_engine::presentation::Card,
) -> Result<(), ApiError> {
    if let Err(error) = check_expected(expected, actual) {
        let db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        app.request_lease().validate(&db)?;
        if let Some(origin) = &actual.metadata_origin {
            validate_db(
                &db,
                app.identity().account_id().ok_or_else(auth::forbidden)?,
                origin,
            )?;
        }
        if let Ok(mut cache) = app.verified_cards.lock() {
            cache.retain(|_, card| {
                !(card.metadata_origin == actual.metadata_origin
                    && (card.identity.kind == actual.identity.kind
                        && card.identity.id == actual.identity.id
                        || card.parent_identity.as_ref().is_some_and(|parent| {
                            parent.kind == actual.identity.kind && parent.id == actual.identity.id
                        })))
            });
        }
        kids::scoped::invalidate_mismatch(
            &db,
            app.identity().account_id().ok_or_else(auth::forbidden)?,
            actual,
        )?;
        return Err(error);
    }
    Ok(())
}

pub(crate) fn validate_assignment_db(
    db: &Connection,
    lease: &ResourceLease,
    origin: &MetadataOrigin,
) -> Result<(), ApiError> {
    lease.validate(db)?;
    let auth::Principal::Account {
        profile_id: Some(profile),
        ..
    } = lease.principal
    else {
        return Err(auth::forbidden());
    };
    validate_db(
        db,
        lease.principal.account_id().ok_or_else(auth::forbidden)?,
        origin,
    )?;
    let connector = if origin.provider_id == "simkl" {
        format!("simkl:{profile}")
    } else {
        origin.connection_id.clone().ok_or_else(auth::forbidden)?
    };
    let saved: Option<String> = db
        .query_row(
            "SELECT value FROM engine_profile_configuration WHERE profile_id=?1",
            [profile],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?;
    if let Some(saved) = saved {
        let config: viptv_engine::profile::ProfileConfiguration = serde_json::from_str(&saved)
            .map_err(|_| ApiError::from("Profile configuration unavailable"))?;
        if !config
            .assignments
            .iter()
            .any(|a| a.connection_id == connector && !a.enabled.is_empty())
        {
            return Err(auth::forbidden());
        }
    }
    Ok(())
}

/// Construct identity-affecting provider matching only from the verified host card.
pub(crate) fn source_value(card: &viptv_engine::presentation::Card) -> Value {
    let mut value = json!({"type":card.identity.kind,"id":card.identity.id,"name":if card.identity.episode.is_some(){Value::Null}else{json!(card.title)},"poster":card.poster,"card":card,"metadata_origin":card.metadata_origin});
    if let Some(episode) = &card.identity.episode {
        value["season"] = json!(episode.season);
        value["episode"] = json!(episode.episode);
        value["series_id"] = json!(card.parent_identity.as_ref().map(|p| p.id.as_str()));
    }
    let identity = card.parent_identity.as_ref().unwrap_or(&card.identity);
    for (namespace, field) in [("imdb", "imdb_id"), ("tmdb", "tmdb_id")] {
        let values = identity
            .ids
            .iter()
            .filter(|id| id.namespace == namespace)
            .map(|id| id.value.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        if values.len() == 1 {
            if let Some(id) = values.first() {
                if let Ok(clean) = matching_context(&json!({field:id})) {
                    value[field] = clean[field].clone();
                }
            }
        }
    }
    if let Some(year) = card.metadata.as_ref().and_then(|m| m.year) {
        value["year"] = json!(year);
    }
    value
}
