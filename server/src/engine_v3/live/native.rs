use super::*;
use std::sync::Arc;
#[cfg(test)]
#[path="m3u/tests.rs"]
mod tests;
fn id(connection: &IntegrationConnection) -> Result<i64, ApiError> {
    connection
        .id
        .strip_prefix("m3u:")
        .and_then(|id| id.parse().ok())
        .ok_or_else(auth::forbidden)
}
fn credential(
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    connection: &IntegrationConnection,
) -> Result<M3uConfiguration, ApiError> {
    let id = id(connection)?;
    let db = app.db.lock().unwrap();
    lease.validate(&db)?;
    if !configuration::available_m3u(&db, lease, id) {
        return Err(auth::forbidden());
    }
    let envelope:String=db.query_row("SELECT secret FROM engine_m3u_connections WHERE id=?1 AND account_id=?2 AND profile_id=?3",params![id,lease.principal.account_id(),profile],|r|r.get(0)).map_err(db_error)?;
    let vault = app
        .secret_vault
        .as_ref()
        .ok_or("Credential vault unavailable")?;
    let secret = vault
        .open(
            lease.principal.account_id().unwrap(),
            "m3u",
            &format!("{profile}:{id}"),
            &envelope,
        )
        .map_err(|_| ApiError::from("Live credentials unavailable"))?;
    serde_json::from_slice(secret.expose())
        .map_err(|_| ApiError::from("Live credentials unavailable"))
}
pub(super) async fn request(
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    revision: u64,
    connection: &IntegrationConnection,
    url: &str,
    limit: usize,
) -> Result<(String, String), ApiError> {
    let owner = app.clone();
    let lease = lease.clone();
    let connection = connection.clone();
    let authorize: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
        if super::super::configuration::validate_revision(&owner, profile, revision).is_err() {
            return false;
        }
        let db = owner.db.lock().unwrap();
        lease.validate(&db).is_ok() && configuration::available_private(&db, &lease, &connection)
    });
    let uri = url::Url::parse(url).map_err(|_| ApiError::from("Invalid live endpoint"))?;
    let mut input = viptv_engine::egress::HttpRequest::get(uri);
    input.timeout = Some(Duration::from_secs(8));
    input.max_redirects = Some(4);
    let response = tokio::time::timeout(
        Duration::from_secs(10),
        viptv_engine::egress::ScopedHttp::new(authorize).fetch(input, limit, false),
    )
    .await
    .map_err(|_| ApiError::from("Live data temporarily unavailable"))?
    .map_err(|_| ApiError::from("Live data temporarily unavailable"))?;
    if !(200..300).contains(&response.status) {
        return Err("Live data temporarily unavailable".into());
    }
    Ok((
        response.url.to_string(),
        viptv_engine::live::decode_xmltv_body(&response.body)
            .map_err(|_| ApiError::from("Live data temporarily unavailable"))?,
    ))
}
async fn directory(
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    revision: u64,
    connection: &IntegrationConnection,
) -> Result<Cache, ApiError> {
    if connection.id.starts_with("xtream:") { return super::xtream::directory(app,lease,profile,revision,connection).await; }
    let gate = app.live_fetch_gates.connection(&connection.id)?;
    let _gate = tokio::time::timeout(Duration::from_secs(10), gate.lock())
        .await
        .map_err(|_| ApiError::from("Live data temporarily unavailable"))?;
    if let Some(cache) = app
        .live_cache
        .lock()
        .await
        .get(&connection.id)
        .filter(|cache| cache.revision == revision as i64 && Instant::now() < cache.expires)
    {
        return Ok(cache.clone());
    }
    let credentials = credential(app, lease, profile, connection)?;
    let (base, body) = request(
        app,
        lease,
        profile,
        revision,
        connection,
        &credentials.playlist_url,
        3 * 1024 * 1024,
    )
    .await?;
    let channels = parse_m3u(
        &body,
        &url::Url::parse(&base).map_err(|_| ApiError::from("Invalid playlist"))?,
        &connection.id,
    )
    .map_err(|_| ApiError::from("Playlist data unsupported"))?;
    let mut groups = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for channel in &channels {
        for group in &channel.groups {
            if seen.insert(group.id.clone()) {
                groups.push(group.clone());
            }
        }
    }
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
pub(crate) async fn page(
    app: App,
    lease: ResourceLease,
    profile: i64,
    revision: u64,
    connection: IntegrationConnection,
    connections: Vec<IntegrationConnection>,
    query: LiveQuery,
) -> ApiResult {
    let cache = directory(&app, &lease, profile, revision, &connection).await?;
    let filtered = cache
        .channels
        .iter()
        .filter(|c| {
            query
                .group_id
                .as_ref()
                .is_none_or(|g| c.channel.in_group(g))
                && query.search.as_ref().is_none_or(|s| {
                    c.channel
                        .card
                        .title
                        .to_lowercase()
                        .contains(&s.to_lowercase())
                })
        })
        .collect::<Vec<_>>();
    let limit = query.limit.unwrap_or(100) as usize;
    let start = (query.offset as usize).min(filtered.len());
    let end = start.saturating_add(limit).min(filtered.len());
    super::super::configuration::validate_revision(&app, profile, revision)?;
    // This monotonic cache-instance timestamp is stable across clones/pages and
    // changes on every directory replacement; guide refreshes leave it intact.
    let catalog_generation = format!("{}:{:?}", connection.id, cache.expires);
    snapshot::project(&app, profile, revision, &query, &catalog_generation, filtered.len() as u64, LivePage {
        channels: filtered
            .iter()
            .skip(start)
            .take(limit)
            .map(|c| c.channel.clone())
            .collect(),
        groups: cache.groups,
        connections,
        next_cursor: (end < filtered.len()).then(|| end.to_string()),
        complete: end >= filtered.len(),
        availability: Availability::Available,
        reason: None
    })
}
pub(crate) async fn guide(
    app: App,
    lease: ResourceLease,
    profile: i64,
    revision: u64,
    connection: IntegrationConnection,
    channel: String,
) -> ApiResult {
    if connection.id.starts_with("xtream:") { return super::xtream::guide(app,lease,profile,revision,connection,channel).await; }
    let cache = directory(&app, &lease, profile, revision, &connection).await?;
    let selected = cache
        .channels
        .iter()
        .find(|c| c.channel.id == channel)
        .ok_or_else(auth::forbidden)?;
    let credentials = credential(&app, &lease, profile, &connection)?;
    if let Some((expiry, guide)) = cache
        .guides
        .get(&channel)
        .filter(|(expiry, _)| Instant::now() < *expiry)
    {
        let _ = expiry;
        return Ok(axum::Json(json!(guide)));
    }
    let Some((url, epg)) = credentials
        .epg_url
        .filter(|s| !s.is_empty())
        .zip(selected.epg_id.as_ref())
    else {
        return Ok(axum::Json(json!(LiveGuide {
            channel_id: channel,
            programs: vec![],
            availability: Availability::Unsupported,
            reason: Some("No XMLTV guide configured for this channel".into())
        })));
    };
    let gate = app.live_fetch_gates.connection(&connection.id)?;
    let _gate = tokio::time::timeout(Duration::from_secs(10), gate.lock())
        .await
        .map_err(|_| ApiError::from("Guide temporarily unavailable"))?;
    if let Some((expiry, guide)) = app
        .live_cache
        .lock()
        .await
        .get(&connection.id)
        .and_then(|c| c.guides.get(&channel))
        .filter(|(expiry, _)| Instant::now() < *expiry)
    {
        let _ = expiry;
        return Ok(axum::Json(json!(guide)));
    }
    let cached = app
        .live_cache
        .lock()
        .await
        .get(&connection.id)
        .and_then(|c| c.xmltv.as_ref())
        .filter(|(expiry, _)| Instant::now() < *expiry)
        .map(|(_, body)| body.clone());
    let fetched = cached.is_none();
    let response = if let Some(body) = cached {
        Ok(body)
    } else {
        request(
            &app,
            &lease,
            profile,
            revision,
            &connection,
            &url,
            16 * 1024 * 1024,
        )
        .await
        .map(|(_, body)| Arc::<str>::from(body))
    };
    if let Some(body) = response.as_ref().ok().filter(|_| fetched) {
        let mut caches = app.live_cache.lock().await;
        let mut total = caches
            .values()
            .filter_map(|c| c.xmltv.as_ref())
            .map(|(_, body)| body.len())
            .sum::<usize>();
        if total.saturating_add(body.len()) > 32 * 1024 * 1024 {
            for (other, cache) in caches.iter_mut() {
                if other != &connection.id {
                    if let Some((_, old)) = cache.xmltv.take() {
                        total = total.saturating_sub(old.len());
                    }
                    if total.saturating_add(body.len()) <= 32 * 1024 * 1024 {
                        break;
                    }
                }
            }
        }
        if let Some(cache) = caches.get_mut(&connection.id) {
            cache.xmltv = Some((Instant::now() + Duration::from_secs(60), body.clone()));
        }
    }
    let mut guide = match response.and_then(|body| {
        parse_xmltv(&body, epg, &channel).map_err(|_| ApiError::from("Guide data unsupported"))
    }) {
        Ok(programs) => LiveGuide {
            channel_id: channel.clone(),
            programs,
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
    let now = util::now_millis() as u64;
    guide.programs.retain(|p| {
        p.end_millis > now.saturating_sub(24 * 60 * 60 * 1000)
            && p.start_millis < now + 7 * 24 * 60 * 60 * 1000
    });
    guide.programs.truncate(100);
    super::super::configuration::validate_revision(&app, profile, revision)?;
    if let Some(cache) = app.live_cache.lock().await.get_mut(&connection.id) {
        cache.guides.insert(
            channel,
            (Instant::now() + Duration::from_secs(60), guide.clone()),
        );
    }
    Ok(axum::Json(json!(guide)))
}
pub(crate) async fn sources(
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
        .find(|c| c.channel.id == channel)
        .ok_or_else(auth::forbidden)?;
    let proof = crate::sources::source_configuration(&app.db.lock().unwrap(), &connection.id)?
        .ok_or_else(auth::forbidden)?;
    let raw=selected.sources.iter().map(|source|json!({"url":source.url,"name":selected.channel.card.title,"behaviorHints":{"proxyHeaders":{"request":source.headers}}})).collect();
    let (cards, error) = app.register_with_configuration(&connection.id, raw, "live", Some(proof));
    if error.is_some() {
        return Err("Live source unavailable".into());
    }
    super::super::configuration::validate_revision(&app, profile, revision)?;
    let mut entries = app.streams.lock().unwrap();
    for card in &cards {
        if let Some(entry) = card["id"].as_str().and_then(|id| entries.get_mut(id)) {
            entry.media_id = Some(channel.clone());
            entry.profile_revision = Some((profile, revision));
            entry.created = Instant::now()
                .checked_sub(Duration::from_secs(1500))
                .unwrap_or_else(Instant::now);
            entry.fixed_lifetime = true;
        }
    }
    drop(entries);
    super::remember_sources(&app, profile, revision, &channel, &connection.id, &cards);
    let action = super::viewing_action(&app, &lease, profile, &selected.channel.card)?;
    Ok(axum::Json(json!({"streams":cards,"live":true,"action":action})))
}
