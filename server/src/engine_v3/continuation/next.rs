use super::*;
fn unavailable() -> NextEpisodeResult {
    NextEpisodeResult {
        status: ContinuationStatus::Unavailable,
        item: None,
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NextRequest {
    identity: MediaIdentity,
    parent_identity: Option<MediaIdentity>,
    metadata_provider_id: Option<String>,
    metadata_addon_id: Option<i64>,
    metadata_origin: Option<viptv_engine::origin::MetadataOrigin>,
    card: Option<Card>,
}
pub(crate) async fn resolve(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    axum::Json(request): axum::Json<NextRequest>,
) -> ApiResult {
    if request.metadata_origin.is_none()
        && request
            .card
            .as_ref()
            .is_some_and(|card| card.metadata_origin.is_some())
    {
        return Err("Metadata origin is required".into());
    }
    let input = NextEpisodeRequest {
        identity: request.identity,
        parent_identity: request.parent_identity,
    };
    validate(&input.identity, input.parent_identity.as_ref())?;
    if input.identity.kind != "series"
        || input.identity.episode.is_none()
        || input.parent_identity.is_none()
    {
        return Err("A typed episode and parent are required".into());
    }
    let app = app.with_lease(lease.clone());
    let (config, _) = configuration::read(&app, &lease, profile)?;
    let (provider, addon) = super::super::origins::selectors(
        &app,
        request.metadata_provider_id.as_deref(),
        request.metadata_addon_id,
        request.metadata_origin.as_ref(),
    )?;
    let origin = super::super::search::request_origin(&app, provider.as_deref(), addon)?;
    let metadata_proof = if let Some(wire) = &request.metadata_origin {
        let expected = request
            .card
            .as_ref()
            .ok_or("A scoped episode selection is required")?;
        if expected.identity != input.identity
            || expected.parent_identity != input.parent_identity
            || expected.metadata_origin.as_ref() != Some(wire)
        {
            return Err("Conflicting episode expectation".into());
        }
        Some(super::super::origins::MetadataProof::verified(&app, expected).await?)
    } else {
        None
    };
    let simkl = origin
        .as_ref()
        .map(|(provider, _, _)| provider.as_str())
        .unwrap_or(&config.browsing_plugin)
        == "simkl";
    let provider = if simkl { "simkl" } else { "stremio" };
    if metadata_proof.is_none() {
        kids::consumers::require(
            &app,
            &input.identity.kind,
            &input.identity.id,
            provider,
            true,
        )?;
        if let Some(parent) = &input.parent_identity {
            kids::consumers::require(&app, &parent.kind, &parent.id, provider, true)?;
        }
    }
    let addon_revision = app.addons.revision()?;
    {
        let db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        lease.validate(&db)?;
        app.require_profile(&db, profile)?;
        if let Some(proof) = &metadata_proof {
            proof.validate(&db, &lease)?;
        } else {
            kids::require_item(
                &db,
                &lease.principal,
                &input.identity.kind,
                &input.identity.id,
            )?;
        }
    }
    let document = if origin.is_none() {
        super::metadata::series_document(
            &app,
            &lease,
            profile,
            &input,
            simkl,
            false,
            Duration::from_secs(8),
        )
        .await
    } else {
        match tokio::time::timeout(
            Duration::from_secs(8),
            super::metadata::document(
                &app,
                &lease,
                profile,
                &input,
                simkl,
                origin.as_ref(),
                request.metadata_origin.as_ref(),
                false,
            ),
        )
        .await
        {
            Ok(document) => document,
            Err(_) => Err(ApiError::from("Metadata unavailable")),
        }
    };
    let (parent, episodes) = match document {
        Ok(document) => document,
        Err(error)
            if matches!(
                error.0,
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::CONFLICT
            ) =>
        {
            return Err(error)
        }
        _ => {
            configuration::validate_revision(&app, profile, config.scope.revision)?;
            let db = app
                .db
                .lock()
                .map_err(|_| ApiError::from("Database unavailable"))?;
            lease.validate(&db)?;
            return Ok(axum::Json(serde_json::to_value(unavailable()).unwrap()));
        }
    };
    let output = viptv_engine::continuation::next_episode(
        &input.identity,
        &episodes,
        util::now_millis().max(0) as u64,
    );
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    lease.validate(&db)?;
    revision_db(&db, profile, config.scope.revision)?;
    app.require_profile(&db, profile)?;
    if app.addons.revision_db(&db)? != addon_revision {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Metadata integrations changed. Try again.".into(),
        ));
    }
    let current = episodes.iter().find(|entry| {
        entry.card.identity.id == input.identity.id
            && entry.card.identity.kind == input.identity.kind
    });
    if current.is_some_and(|entry| {
        request.card.as_ref().is_some_and(|expected| {
            !viptv_engine::origin::metadata_assertions_match(expected, &entry.card)
        })
    }) {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Episode identity changed. Select this title again.".into(),
        ));
    }
    if current.is_none_or(|entry| entry.card.identity.episode != input.identity.episode)
        || input
            .parent_identity
            .as_ref()
            .is_some_and(|hint| hint.id != parent.identity.id || hint.kind != parent.identity.kind)
        || output.item.as_ref().is_some_and(|card| {
            if metadata_proof.is_some() {
                kids::scoped::require_card_db(&db, &lease.principal, card).is_err()
            } else {
                kids::require_item(
                    &db,
                    &lease.principal,
                    &card.identity.kind,
                    &card.identity.id,
                )
                .is_err()
            }
        })
    {
        return Ok(axum::Json(serde_json::to_value(unavailable()).unwrap()));
    }
    if let Some(proof) = &metadata_proof {
        proof.validate(&db, &lease)?;
        super::super::scoped_library::cache_next_db(&db, &app, profile, &proof.card, &output)?;
        return Ok(axum::Json(
            serde_json::to_value(output).map_err(|_| ApiError::from("Next episode unavailable"))?,
        ));
    }
    let mut legacy = if let Some(card) = output.item.as_ref() {
        let coordinates = card.identity.episode.as_ref();
        json!({"status":"next","item":{"id":card.identity.id,"type":card.identity.kind,"name":parent.title,"poster":parent.poster,"position":0,"duration":0,"series_id":parent.identity.id,"season":coordinates.and_then(|e|e.season),"episode":coordinates.map(|e|e.episode),"watched":false,"queue_status":"next","engine_identity":card.identity,"engine_parent_identity":parent.identity,"engine_provider_id":card.provider_id,"engine_presentation":card,"background":parent.landscape}})
    } else {
        json!({"status":match output.status{ContinuationStatus::CaughtUp=>"caught_up",ContinuationStatus::Upcoming=>"upcoming",_=>"unavailable"}})
    };
    legacy["metadata_origin"] = json!(parent.provider_id);
    legacy["metadata_account_id"] = json!(lease.principal.account_id());
    legacy["metadata_addon_revision"] = json!(addon_revision);
    legacy["metadata_configuration_revision"] = json!(config.scope.revision);
    let previous:Option<String>=db.query_row("SELECT result FROM continuation_cache WHERE profile_id=?1 AND series_id=?2 AND from_id=?3",params![profile,parent.identity.id,input.identity.id],|r|r.get(0)).optional().map_err(db_error)?;
    db.execute("INSERT INTO continuation_cache(profile_id,series_id,from_id,result,updated_at) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(profile_id,series_id,from_id) DO UPDATE SET result=excluded.result,updated_at=excluded.updated_at",params![profile,parent.identity.id,input.identity.id,legacy.to_string(),util::now()]).map_err(db_error)?;
    if previous.as_deref() != Some(&legacy.to_string()) {
        db.execute("INSERT INTO engine_library_revisions VALUES(?1,1) ON CONFLICT(profile_id) DO UPDATE SET revision=revision+1",[profile]).map_err(db_error)?;
    }
    db.execute("DELETE FROM continuation_cache WHERE profile_id=?1 AND rowid NOT IN(SELECT rowid FROM continuation_cache WHERE profile_id=?1 ORDER BY updated_at DESC,rowid DESC LIMIT 200)",[profile]).map_err(db_error)?;
    let _ = super::releases::store_db(
        &db,
        &lease,
        profile,
        &parent.identity.id,
        &input.identity,
        &episodes,
        config.scope.revision,
        &addon_revision,
        parent.landscape.as_deref(),
        parent.metadata_origin.as_ref(),
    );
    drop(db);
    if let Some(card) = output.item.as_ref() {
        super::super::source_integrations::cache_card_at(
            &app,
            &lease,
            config.scope.revision,
            &addon_revision,
            card.clone(),
        );
    }
    super::super::source_integrations::cache_card_at(
        &app,
        &lease,
        config.scope.revision,
        &addon_revision,
        parent,
    );
    Ok(axum::Json(
        serde_json::to_value(output).map_err(|_| ApiError::from("Next episode unavailable"))?,
    ))
}
