//! Shared-engine Xtream requests/projections with server-owned scope and egress.
use super::*;

pub(super) async fn directory(
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    revision: u64,
    connection: &IntegrationConnection,
) -> Result<Cache, ApiError> {
    let gate = app.live_fetch_gates.connection(&connection.id)?;
    let _gate = tokio::time::timeout(Duration::from_secs(10), gate.lock())
        .await
        .map_err(|_| ApiError::from("Live data temporarily unavailable"))?;
    let credentials = xtream_configuration::credential(app, lease, profile, connection)?;
    let cached = app
        .live_cache
        .lock()
        .await
        .get(&connection.id)
        .filter(|cache| cache.revision == revision as i64 && Instant::now() < cache.expires)
        .cloned();
    if let Some(cache) = cached {
        xtream_configuration::validate(app, lease, profile, revision, connection)?;
        return Ok(cache);
    }
    let streams = xtream_request(&credentials, "get_live_streams", None)
        .map_err(|_| ApiError::from("Invalid live endpoint"))?;
    let groups = xtream_request(&credentials, "get_live_categories", None)
        .map_err(|_| ApiError::from("Invalid live endpoint"))?;
    let ((_, streams), (_, groups)) = tokio::try_join!(
        native::request(
            app,
            lease,
            profile,
            revision,
            connection,
            streams.as_str(),
            3 * 1024 * 1024
        ),
        native::request(
            app,
            lease,
            profile,
            revision,
            connection,
            groups.as_str(),
            3 * 1024 * 1024
        )
    )?;
    let mut channels = parse_xtream(&streams, &connection.id, &credentials)
        .map_err(|_| ApiError::from("Live data unsupported"))?;
    let groups = xtream_groups(&groups).map_err(|_| ApiError::from("Live groups unsupported"))?;
    for channel in &mut channels {
        if let Some(group) = groups
            .iter()
            .find(|group| channel.channel.group_id.as_ref() == Some(&group.id))
        {
            channel.channel.group_name = Some(group.label.clone());
        }
    }
    xtream_configuration::validate(app, lease, profile, revision, connection)?;
    let cache = Cache {
        revision: revision as i64,
        expires: Instant::now() + Duration::from_secs(300),
        channels,
        groups,
        guides: Default::default(),
        xmltv: None,
    };
    let mut caches = app.live_cache.lock().await;
    caches.retain(|_, cache| Instant::now() < cache.expires);
    if caches.len() >= 256 {
        return Err("Live refresh capacity reached".into());
    }
    caches.insert(connection.id.clone(), cache.clone());
    Ok(cache)
}
pub(super) async fn guide(
    app: App,
    lease: ResourceLease,
    profile: i64,
    revision: u64,
    connection: IntegrationConnection,
    channel: String,
) -> ApiResult {
    let cache = directory(&app, &lease, profile, revision, &connection).await?;
    let selected = cache
        .channels
        .iter()
        .find(|entry| entry.channel.id == channel)
        .ok_or_else(auth::forbidden)?;
    if let Some((_, guide)) = cache
        .guides
        .get(&channel)
        .filter(|(expiry, _)| Instant::now() < *expiry)
    {
        return Ok(axum::Json(json!(guide)));
    }
    let gate = app.live_fetch_gates.connection(&connection.id)?;
    let _gate = tokio::time::timeout(Duration::from_secs(10), gate.lock())
        .await
        .map_err(|_| ApiError::from("Guide temporarily unavailable"))?;
    let credentials = xtream_configuration::credential(&app, &lease, profile, &connection)?;
    let cached = app
        .live_cache
        .lock()
        .await
        .get(&connection.id)
        .and_then(|cache| cache.guides.get(&channel))
        .filter(|(expiry, _)| Instant::now() < *expiry)
        .cloned();
    if let Some((_, guide)) = cached {
        xtream_configuration::validate(&app, &lease, profile, revision, &connection)?;
        return Ok(axum::Json(json!(guide)));
    }
    let uri = xtream_request(&credentials, "get_short_epg", selected.stream_id.as_deref())
        .map_err(|_| ApiError::from("Guide unavailable"))?;
    let result = native::request(
        &app,
        &lease,
        profile,
        revision,
        &connection,
        uri.as_str(),
        3 * 1024 * 1024,
    )
    .await
    .and_then(|(_, body)| {
        serde_json::from_str::<Value>(&body).map_err(|_| ApiError::from("Guide unavailable"))
    });
    let mut guide = match result {
        Ok(value) => LiveGuide {
            channel_id: channel.clone(),
            programs: parse_xtream_guide(&value, &channel),
            availability: Availability::Available,
            reason: None,
        },
        Err(_) => LiveGuide {
            channel_id: channel.clone(),
            programs: vec![],
            availability: Availability::TemporarilyUnavailable,
            reason: Some("Guide temporarily unavailable".into()),
        },
    };
    let now = util::now_millis().max(0) as u64;
    guide.programs.retain(|program| {
        program.end_millis > now.saturating_sub(24 * 60 * 60 * 1000)
            && program.start_millis < now.saturating_add(7 * 24 * 60 * 60 * 1000)
    });
    guide.programs.truncate(100);
    xtream_configuration::validate(&app, &lease, profile, revision, &connection)?;
    if let Some(cache) = app.live_cache.lock().await.get_mut(&connection.id) {
        cache.guides.insert(
            channel,
            (Instant::now() + Duration::from_secs(60), guide.clone()),
        );
    }
    Ok(axum::Json(json!(guide)))
}
