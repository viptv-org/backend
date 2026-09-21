use super::*;
use sha2::{Digest, Sha256};

fn validate_source_scope(a: &App, v: &PlaybackRequest, db: &Connection) -> Result<(), ApiError> {
    let provider_kind = if let Some(id) = &v.stream_id {
        let streams = a.streams.lock().unwrap();
        let source = streams.get(id).ok_or("Stream expired; discover again")?;
        source.provider_id.map(|p| (p, source.kind.clone()))
    } else if let Some(id) = &v.channel_id {
        if lineup::family_id(db, id)?.is_some() {
            None
        } else {
            db.query_row(
                "SELECT provider_id FROM provider_live WHERE id=?1",
                [id],
                |r| r.get::<_, i64>(0),
            )
            .ok()
            .map(|p| (p, "live".into()))
        }
    } else {
        None
    };
    if let Some((provider, kind)) = provider_kind {
        let allowed: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM providers WHERE id=?1 AND enabled=1 AND CASE ?2 WHEN 'live' THEN enable_live WHEN 'movie' THEN enable_movies WHEN 'series' THEN enable_series ELSE 0 END=1)",params![provider,kind],|r|r.get(0)).map_err(db_error)?;
        if !allowed {
            return Err("Provider not found or disabled".into());
        }
    }
    Ok(())
}
fn source_revision(
    a: &App,
    v: &PlaybackRequest,
    db: &Connection,
) -> Result<Option<String>, ApiError> {
    let streams = a.streams.lock().unwrap();
    v.stream_id
        .as_ref()
        .and_then(|id| streams.get(id))
        .and_then(|s| s.provider_id)
        .map(|id| {
            db.query_row(
                "SELECT json_array(url,username,password) FROM providers WHERE id=?1",
                [id],
                |r| r.get::<_, String>(0),
            )
            .map(|value| format!("{:x}", Sha256::digest(value)))
            .map_err(db_error)
        })
        .transpose()
}
pub(super) async fn identity(
    a: &App,
    v: &PlaybackRequest,
) -> Result<(String, Option<String>), ApiError> {
    validate_request(a).await?;
    {
        let db = a.db.lock().unwrap();
        validate_source_scope(a, v, &db)?;
    }
    if !v.position.is_finite() || v.position < 0.0 || v.position > 604800.0 {
        return Err("Invalid playback position".into());
    }
    let source = match (&v.channel_id, &v.stream_id) {
        (Some(channel), None) => {
            let family = {
                let db = a.db.lock().unwrap();
                lineup::family_id(&db, channel)?
                    .map(|family| json!(["family", family, lineup::recovery_policy(&db).ok()]))
            };
            if let Some(value) = family {
                value
            } else {
                let id = channel.clone();
                let (url, provider) = a.providers.blocking(move |p| p.channel_source(&id)).await?;
                json!(["provider-live", provider, url])
            }
        }
        (None, Some(id)) => {
            a.check_resource(&a.identity(), "stream", id)?;
            let streams = a.streams.lock().unwrap();
            let source = streams.get(id).ok_or("Stream expired; discover again")?;
            let mut headers = source.headers.iter().collect::<Vec<_>>();
            headers.sort();
            let auth::Principal::Account { account_id, .. } = a.identity();
            // Provider catalogs are server-owned; addon sources remain confined
            // to the VIPTV account that discovered them, including its headers.
            json!([
                "source",
                source.url,
                headers,
                source.live,
                source.kind,
                source.provider_id,
                source.provider_id.is_none().then_some(account_id)
            ])
        }
        _ => return Err("Provide exactly one stream_id or channel_id".into()),
    };
    let provider_revision = source_revision(a, v, &a.db.lock().unwrap())?;
    let material = json!([
        source,
        provider_revision,
        v.position,
        v.capabilities.clone().unwrap_or_default(),
        v.force_transcode,
        v.audio_track_index,
        v.audio_language,
        v.subtitle_track_index,
        v.subtitles_off,
        v.preferences.audio_language,
        v.preferences.subtitle_language,
        v.preferences.subtitles_enabled
    ]);
    Ok((
        format!("{:x}", Sha256::digest(material.to_string())),
        provider_revision,
    ))
}

pub(in crate::session) async fn start(a: App, v: PlaybackRequest) -> ApiResult {
    // A direct-URL client fetches the source itself, so no proxy transport
    // would ever exist behind a shared wrapper: it never enters the shared
    // layer and the manager hands back the original URL untouched.
    if v.capabilities
        .as_ref()
        .is_some_and(|caps| caps.direct_urls == Some(true))
    {
        return start_playback_inner(a, v).await;
    }
    let (key, revision) = identity(&a, &v).await?;
    let registry = a.shared_playback.clone();
    let prior = registry.groups.lock().unwrap().get(&key).cloned();
    if let Some(group) = prior {
        let response = group.state.lock().unwrap().response.clone();
        let reusable = if let Some(response) = response {
            if response["live"] == true {
                let db = a.db.lock().unwrap();
                response["worker_state"] != "failed"
                    && response["candidate_id"]
                        .as_str()
                        .map(|candidate| {
                            let channel = response["channel_id"].as_str().unwrap_or("");
                            lineup::eligible(&db, channel, candidate).unwrap_or(false)
                                && provider::selection::current_source(&db, candidate)
                                    .ok()
                                    .as_deref()
                                    == response["_source_key"].as_str()
                        })
                        .unwrap_or(true)
            } else if let Some(id) = response["id"].as_str() {
                a.playback.timeline_origin_available(id).await
            } else {
                false
            }
        } else {
            true
        };
        if !reusable {
            let mut groups = registry.groups.lock().unwrap();
            if groups.get(&key).is_some_and(|g| Arc::ptr_eq(g, &group)) {
                groups.remove(&key);
            }
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    let capability = uuid::Uuid::new_v4().simple().to_string();
    let (group, created) = {
        let db = a.db.lock().unwrap();
        validate_source_scope(&a, &v, &db)?;
        if source_revision(&a, &v, &db)? != revision {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "Source changed; retry playback".into(),
            ));
        }
        let mut groups = registry.groups.lock().unwrap();
        let mut viewers = registry.viewers.lock().unwrap();
        if viewers.len() >= 4096 {
            return Err(ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "Viewer capacity reached".into(),
            ));
        }
        let existing = groups
            .get(&key)
            .filter(|g| {
                let state = g.state.lock().unwrap();
                !*g.finished.borrow()
                    && state.response.as_ref().is_none_or(|response| {
                        response["candidate_id"].as_str().is_none_or(|candidate| {
                            lineup::eligible(
                                &db,
                                response["channel_id"].as_str().unwrap_or(""),
                                candidate,
                            )
                            .unwrap_or(false)
                                && provider::selection::current_source(&db, candidate)
                                    .ok()
                                    .as_deref()
                                    == response["_source_key"].as_str()
                        })
                    })
            })
            .cloned();
        let created = existing.is_none();
        let group = existing.unwrap_or_else(|| {
            let (finish, finished) = watch::channel(false);
            Arc::new(Group {
                audience: uuid::Uuid::new_v4().to_string(),
                state: Mutex::new(GroupState {
                    viewers: HashMap::new(),
                    response: None,
                    error: None,
                }),
                notify: tokio::sync::Notify::new(),
                finish,
                finished,
            })
        });
        group.state.lock().unwrap().viewers.insert(
            id.clone(),
            Viewer {
                lease: a.request_lease(),
                capability,
                touched: Instant::now(),
                options: v.clone(),
            },
        );
        viewers.insert(id.clone(), group.clone());
        groups.insert(key.clone(), group.clone());
        (group, created)
    };
    let mut pending = Pending {
        registry: registry.clone(),
        id: id.clone(),
        armed: true,
    };
    if created {
        let app = a.clone();
        let work = group.clone();
        tokio::spawn(async move {
            run(app, work, key, v).await;
        });
    }
    loop {
        let notified = group.notify.notified();
        let (response, error) = {
            let state = group.state.lock().unwrap();
            (state.response.clone(), state.error.clone())
        };
        if let Some((code, message)) = error {
            validate_request(&a).await?;
            return Err(ApiError(code, message));
        }
        if let Some(response) = response {
            validate_request(&a).await?;
            let result = render(&group, &id, response)?;
            a.own_resource("playback", &id);
            pending.armed = false;
            return Ok(axum::Json(result));
        }
        tokio::select! {_=notified=>{},_=tokio::time::sleep(Duration::from_millis(50))=>{}}
    }
}
