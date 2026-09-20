use super::*;

pub(crate) async fn start_playback(
    State(a): State<App>,
    axum::Json(mut v): axum::Json<PlaybackRequest>,
) -> ApiResult {
    if v.audio_language.as_ref().is_some_and(|s| {
        s.is_empty() || s.len() > 16 || !s.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
    }) {
        return Err("Invalid audio language".into());
    }
    let prefs_app = a.clone();
    v.preferences = blocking(move || {
        let db = prefs_app.db.lock().unwrap();
        prefs_app.request_lease().validate(&db)?;
        let auth::Principal::Account { profile_id, .. } = prefs_app.identity();
        profile_id
            .map(|id| preferences::load(&db, id))
            .transpose()
            .map(Option::unwrap_or_default)
    })
    .await?;
    v.capabilities = v.preferences.cap(v.capabilities);
    if v.managed_only {
        if let Some(caps) = &mut v.capabilities {
            caps.direct_play = false;
        }
    }
    let effective_preferences = json!(v.preferences);
    let key = v
        .startup_id
        .as_deref()
        .map(|id| startup_key(&a, id))
        .transpose()?;
    let cancelled = if let Some(key) = &key {
        let mut requests = a.startup_requests.lock().unwrap();
        prune_startups(&mut requests);
        if requests.contains_key(key) {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "Playback startup cancelled or already used".into(),
            ));
        }
        if requests.len() >= 4096 {
            return Err(ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many playback requests".into(),
            ));
        }
        let request = StartupRequest::new();
        let cancelled = request.cancelled.clone();
        requests.insert(key.clone(), request);
        Some(cancelled)
    } else {
        None
    };
    let budget = if v.channel_id.is_some() {
        Duration::from_secs(45)
    } else {
        Duration::from_secs(70)
    };
    let preparation = shared::start(a.clone(), v);
    let result=tokio::time::timeout(budget,async {
        tokio::select! {
            result=preparation=>result,
            _=startup_cancelled(cancelled)=>Err(ApiError(StatusCode::CONFLICT,"Playback startup cancelled".into())),
            error=revoked(&a)=>Err(error),
        }
    }).await.unwrap_or_else(|_|Err(ApiError(StatusCode::GATEWAY_TIMEOUT,"Playback startup timed out. Try again later.".into())));
    if let (Some(key), Ok(response)) = (key, &result) {
        let cancelled = {
            let mut requests = a.startup_requests.lock().unwrap();
            if let Some(request) = requests.get_mut(&key) {
                request.session = response.0["id"].as_str().map(str::to_owned);
                request.cancelled.load(std::sync::atomic::Ordering::Acquire)
            } else {
                true
            }
        };
        if cancelled {
            if let Some(id) = response.0["id"].as_str() {
                stop_owned(&a, id).await;
                a.resource_owners
                    .lock()
                    .unwrap()
                    .remove(&format!("playback:{id}"));
            }
            return Err(ApiError(
                StatusCode::CONFLICT,
                "Playback startup cancelled".into(),
            ));
        }
    }
    result.map(|mut response| {
        response.0["preferences"] = effective_preferences;
        response
    })
}
pub(super) async fn start_playback_inner(a: App, v: PlaybackRequest) -> ApiResult {
    a.prune_playback_owners().await;
    a.prune();
    let source_lease = v
        .stream_id
        .as_ref()
        .and_then(|id| a.resource_lease("stream", id));
    let worker = a.clone();
    let channel = v.channel_id.clone();
    let family = blocking(move || {
        let db = worker.db.lock().unwrap();
        worker.require_media(&db)?;
        channel
            .as_deref()
            .map(|id| lineup::family_id(&db, id).map_err(ApiError::from))
            .transpose()
            .map(Option::flatten)
    })
    .await?;
    if v.stream_id.is_none() {
        if let Some(family) = family {
            return start_family(a, &v, family).await;
        }
    }
    let selection = track_selection(&v);
    let (url, headers, live, provider_id, kind) = match (v.stream_id, v.channel_id) {
        (Some(id), None) => {
            a.check_resource(&a.identity(), "stream", &id)?;
            let streams = a.streams.lock().unwrap();
            let s = streams.get(&id).ok_or("Stream expired; discover again")?;
            (
                s.url.clone(),
                s.headers.clone(),
                s.live,
                s.provider_id,
                s.kind.clone(),
            )
        }
        (None, Some(id)) => {
            let (url, provider_id) = a.providers.blocking(move |p| p.channel_source(&id)).await?;
            (
                url,
                provider::egress::headers(&a.db.lock().unwrap(), provider_id)?,
                true,
                Some(provider_id),
                "live".to_owned(),
            )
        }
        _ => return Err("Provide exactly one stream_id or channel_id".into()),
    };
    let permit = if let Some(provider_id) = provider_id {
        Some(
            a.providers
                .acquire_playback_for_kind(provider_id, &kind)
                .await
                .map_err(|e| {
                    if e == "Provider connection limit reached" {
                        ApiError(StatusCode::TOO_MANY_REQUESTS, e)
                    } else {
                        ApiError::from(e)
                    }
                })?,
        )
    } else {
        None
    };
    let r = a
        .playback
        .start_with_selection(
            url,
            headers,
            v.position,
            v.capabilities,
            v.force_transcode,
            live,
            permit,
            selection,
        )
        .await?;
    publish(a, r, source_lease, false).await
}

async fn start_family(a: App, v: &PlaybackRequest, channel: String) -> ApiResult {
    let worker = a.clone();
    let policy = blocking(move || {
        let db = worker.db.lock().unwrap();
        worker.request_lease().validate(&db)?;
        lineup::recovery_policy(&db).map_err(db_error)
    })
    .await?;
    let mut inputs = FamilyInputs {
        current: None,
        policy,
        attempted: std::collections::HashSet::new(),
    };
    let response = tokio::time::timeout(
        Duration::from_secs(policy.deadline_seconds),
        prepare_family(a.clone(), v, channel.clone(), &mut inputs),
    )
    .await
    .map_err(|_| {
        ApiError(
            StatusCode::GATEWAY_TIMEOUT,
            "Playback startup timed out. Try again later.".into(),
        )
    })??;
    let mut result = publish(a.clone(), response, None, true).await?;
    result.0["candidate_id"] = json!(inputs.current.as_ref().map(|key| key.candidate.clone()));
    result.0["_source_key"] = json!(inputs.current.as_ref().map(|key| key.source.clone()));
    live::register(a, v.clone(), channel, inputs, &mut result.0);
    Ok(result)
}
pub(super) async fn prepare_family(
    a: App,
    v: &PlaybackRequest,
    channel: String,
    inputs: &mut FamilyInputs,
) -> Result<playback::PlaybackResponse, ApiError> {
    if v.position != 0.0 {
        return Err("Live playback does not support offset seeking".into());
    }
    let caps = v.capabilities.clone().unwrap_or_default();
    if !caps.h264 || !caps.aac || caps.max_width < 2 || caps.max_height < 2 {
        return Err("H264/AAC playback support and valid dimensions are required".into());
    }
    let device_settings = json!({"capabilities":caps,"force_transcode":v.force_transcode,"audio":v.audio_track_index,"audio_language":v.audio_language,"subtitle":v.subtitle_track_index,"subtitles_off":v.subtitles_off,"preferences":v.preferences}).to_string();
    let mut excluded = inputs.attempted.clone();
    let deadline = Instant::now() + Duration::from_secs(inputs.policy.deadline_seconds);
    let mut attempts = Vec::new();
    let mut failures = 0;
    let mut busy = 0;
    loop {
        if Instant::now() >= deadline || failures >= 5 {
            break;
        }
        validate_request(&a).await?;
        let requested = channel.clone();
        let omitted = excluded.clone();
        let device = device_settings.clone();
        let selection = a
            .providers
            .blocking(move |p| p.reserve_family(&requested, &omitted, &device))
            .await?;
        for skipped in selection.skipped {
            if skipped["reason"] == "connections_busy" {
                busy += 1;
            }
            if !attempts.iter().any(|previous: &Value| {
                previous["candidate_id"] == skipped["candidate_id"]
                    && previous["reason"] == skipped["reason"]
            }) {
                attempts.push(skipped);
            }
        }
        let Some(selected) = selection.reservation else {
            break;
        };
        let provider::selection::Reservation {
            candidate,
            url,
            permit,
            explanation,
            observation,
        } = selected;
        excluded.insert(candidate.clone());
        let started = Instant::now();
        validate_request(&a).await?;
        inputs.attempted.insert(candidate.clone());
        let headers = provider::egress::candidate_headers(&a.db.lock().unwrap(), &candidate)?;
        let preparation = a.playback.start_with_selection(
            url,
            headers,
            0.0,
            v.capabilities.clone(),
            v.force_transcode,
            true,
            Some(permit),
            track_selection(v),
        );
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_secs(inputs.policy.attempt_seconds));
        let result = tokio::select! {
            result=tokio::time::timeout(remaining,preparation)=>result,
            error=revoked(&a)=>return Err(error),
        };
        match result {
            Ok(Ok(response)) => {
                let _ = a.providers.record_family_input(
                    &observation,
                    true,
                    started.elapsed().as_millis() as u64,
                );
                inputs.current = Some(observation);
                attempts.push(explanation);
                record_family_startup(&a, &channel, &attempts).await;
                return Ok(response);
            }
            Ok(Err(error)) => {
                let reason = if error == "Playback capacity reached" {
                    busy += 1;
                    "connections_busy"
                } else {
                    failures += 1;
                    let _ = a.providers.record_family_input(
                        &observation,
                        false,
                        started.elapsed().as_millis() as u64,
                    );
                    "startup_failed"
                };
                attempts.push(json!({"candidate_id":candidate,"reason":reason}));
            }
            Err(_) => {
                failures += 1;
                let _ = a.providers.record_family_input(
                    &observation,
                    false,
                    started.elapsed().as_millis() as u64,
                );
                attempts.push(json!({"candidate_id":candidate,"reason":"startup_timed_out"}));
            }
        }
        // Never begin replacement while cancelled process cleanup is unconfirmed.
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_secs(2));
        if tokio::time::timeout(remaining, a.playback.settle_cancelled_inputs())
            .await
            .is_err()
        {
            break;
        }
    }
    validate_request(&a).await?;
    record_family_startup(&a, &channel, &attempts).await;
    if busy > 0 && failures == 0 {
        Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "All available connections are busy. Try this channel again shortly.".into(),
        ))
    } else {
        Err(ApiError(
            StatusCode::BAD_GATEWAY,
            "No working stream is available for this channel. Try again later.".into(),
        ))
    }
}
