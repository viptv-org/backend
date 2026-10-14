use super::*;
fn unavailable() -> NextEpisodeResult {
    NextEpisodeResult {
        status: ContinuationStatus::Unavailable,
        item: None,
    }
}
pub(crate) async fn resolve(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    axum::Json(input): axum::Json<NextEpisodeRequest>,
) -> ApiResult {
    validate(&input.identity, input.parent_identity.as_ref())?;
    if input.identity.kind != "series"
        || input.identity.episode.is_none()
        || input.parent_identity.is_none()
    {
        return Err("A typed episode and parent are required".into());
    }
    let app = app.with_lease(lease.clone());
    let (config, _) = configuration::read(&app, &lease, profile)?;
    {
        let db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        lease.validate(&db)?;
        app.require_profile(&db, profile)?;
        kids::require_item(
            &db,
            &lease.principal,
            &input.identity.kind,
            &input.identity.id,
        )?;
    }
    let result = tokio::time::timeout(
        Duration::from_secs(8),
        super::metadata::document(
            &app,
            &lease,
            profile,
            &input,
            config.browsing_plugin == "simkl",
        ),
    )
    .await;
    let (parent, episodes) = match result {
        Ok(Ok(document)) => document,
        Ok(Err(error))
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
    let current = episodes.iter().find(|entry| {
        entry.card.identity.id == input.identity.id
            && entry.card.identity.kind == input.identity.kind
    });
    if current.is_none_or(|entry| entry.card.identity.episode != input.identity.episode)
        || input
            .parent_identity
            .as_ref()
            .is_some_and(|hint| hint.id != parent.identity.id || hint.kind != parent.identity.kind)
        || output.item.as_ref().is_some_and(|card| {
            kids::require_item(
                &db,
                &lease.principal,
                &card.identity.kind,
                &card.identity.id,
            )
            .is_err()
        })
    {
        return Ok(axum::Json(serde_json::to_value(unavailable()).unwrap()));
    }
    // Keep legacy Home/queue projection coherent without manufacturing a progress record.
    let legacy = if let Some(card) = output.item.as_ref() {
        let coordinates = card.identity.episode.as_ref();
        json!({"status":"next","item":{"id":card.identity.id,"type":card.identity.kind,"name":parent.title,"poster":parent.poster,"position":0,"duration":0,"series_id":parent.identity.id,"season":coordinates.and_then(|e|e.season),"episode":coordinates.map(|e|e.episode),"watched":false,"queue_status":"next","engine_identity":card.identity,"engine_parent_identity":parent.identity,"engine_provider_id":card.provider_id,"engine_presentation":card,"background":parent.landscape}})
    } else {
        json!({"status":match output.status{ContinuationStatus::CaughtUp=>"caught_up",ContinuationStatus::Upcoming=>"upcoming",_=>"unavailable"}})
    };
    let previous:Option<String>=db.query_row("SELECT result FROM continuation_cache WHERE profile_id=?1 AND series_id=?2 AND from_id=?3",params![profile,parent.identity.id,input.identity.id],|r|r.get(0)).optional().map_err(db_error)?;
    db.execute("INSERT INTO continuation_cache(profile_id,series_id,from_id,result,updated_at) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(profile_id,series_id,from_id) DO UPDATE SET result=excluded.result,updated_at=excluded.updated_at",params![profile,parent.identity.id,input.identity.id,legacy.to_string(),util::now()]).map_err(db_error)?;
    if previous.as_deref() != Some(&legacy.to_string()) {
        db.execute("INSERT INTO engine_library_revisions VALUES(?1,1) ON CONFLICT(profile_id) DO UPDATE SET revision=revision+1",[profile]).map_err(db_error)?;
    }
    db.execute("DELETE FROM continuation_cache WHERE profile_id=?1 AND rowid NOT IN(SELECT rowid FROM continuation_cache WHERE profile_id=?1 ORDER BY updated_at DESC,rowid DESC LIMIT 200)",[profile]).map_err(db_error)?;
    drop(db);
    if let Some(card) = output.item.as_ref() {
        super::super::source_integrations::cache_card(
            &app,
            &lease,
            config.scope.revision,
            card.clone(),
        );
    }
    super::super::source_integrations::cache_card(&app, &lease, config.scope.revision, parent);
    Ok(axum::Json(
        serde_json::to_value(output).map_err(|_| ApiError::from("Next episode unavailable"))?,
    ))
}
