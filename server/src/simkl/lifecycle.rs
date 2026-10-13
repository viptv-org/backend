use super::*;
use viptv_engine::identity::MediaIdentity;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Playback {
    attempt_id: String,
    sequence: u64,
    identity: MediaIdentity,
    parent_identity: Option<MediaIdentity>,
    event: String,
    position_millis: u64,
    duration_millis: u64,
    #[serde(default)]
    ended: bool,
}

pub(super) fn identity_value(
    identity: &MediaIdentity,
    parent: Option<&MediaIdentity>,
) -> Result<Value, ApiError> {
    if identity.id.is_empty()
        || identity.id.len() > 512
        || identity.id.chars().any(char::is_control)
        || !matches!(identity.kind.as_str(), "movie" | "series" | "live")
    {
        return Err("Invalid media identity".into());
    }
    if identity.id.starts_with("simkl:tv:")
        && identity.id.split(':').count() == 5
        && identity.episode.is_none()
    {
        return Err("A typed Simkl episode identity is required".into());
    }
    let mut value = json!({"id":identity.id,"type":identity.kind});
    if let Some(episode) = &identity.episode {
        if identity.kind != "series"
            || episode.order != "aired"
            || episode.season.is_none()
            || episode.episode == 0
        {
            return Err("An exact aired episode is required".into());
        }
        let parent = parent
            .filter(|p| p.kind == "series" && p.episode.is_none())
            .ok_or("A parent title identity is required")?;
        value["series_id"] = json!(parent.id);
        value["season"] = json!(episode.season);
        value["episode"] = json!(episode.episode);
    } else if parent.is_some() {
        return Err("Unexpected parent title identity".into());
    }
    Ok(value)
}

/// Supported lifecycle calls; incomplete stops become pauses to retain VIPTV's 95% policy.
pub(crate) async fn playback(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    axum::Json(q): axum::Json<Playback>,
) -> ApiResult {
    if !matches!(q.event.as_str(), "start" | "pause" | "stop")
        || q.position_millis > 1_000_000_000_000
        || q.duration_millis > 1_000_000_000_000
        || (q.ended && q.event != "stop")
    {
        return Err("Invalid Simkl playback event".into());
    }
    let app = app.with_lease(lease.clone());
    {
        let db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        app.require_profile(&db, profile)?;
    }
    let credential = match store::credential(&app, profile, Capability::Tracking) {
        Ok(credential) => credential,
        Err(error) => {
            let reason = match error.1.as_str() {
                "Simkl is not configured on this server" => "tracking_unconfigured",
                "Link Simkl for this profile" => "tracking_unlinked",
                "Simkl capability is disabled for this profile" => "tracking_disabled",
                _ => return Err(error),
            };
            return Ok(axum::Json(
                json!({"state":"ignored","provider_id":"simkl","reason":reason}),
            ));
        }
    };
    if q.duration_millis == 0 && (q.event != "start" || q.position_millis != 0 || q.ended) {
        return Ok(axum::Json(
            json!({"state":"unavailable","provider_id":"simkl","reason":"runtime_unavailable"}),
        ));
    }
    let value = identity_value(&q.identity, q.parent_identity.as_ref())?;
    let target = match tracking::Watched::canonical(&value) {
        Ok(target) => target,
        Err(_) => {
            return Ok(axum::Json(
                json!({"state":"unavailable","provider_id":"simkl","reason":"metadata_mapping_unavailable"}),
            ))
        }
    };
    let complete = q.ended
        || (q.duration_millis > 0
            && q.position_millis.saturating_mul(100) >= q.duration_millis.saturating_mul(95));
    let action = if q.event == "stop" && !complete {
        "pause"
    } else {
        q.event.as_str()
    };
    let key = format!("lifecycle:{profile}:{}:{action}", q.identity.id);
    let Some(_reservation) = automatic::reserve(app.simkl.clone(), key) else {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "Simkl tracking is busy".into(),
        ));
    };
    let fence = {
        let db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        app.require_profile(&db, profile)?;
        app.simkl.tracking.events.admit(
            &db,
            &lease,
            profile,
            &q.attempt_id,
            &q.identity.id,
            q.sequence,
            &q.event,
        )?
    };
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        let _permit = app
            .simkl
            .tracking
            .workers
            .acquire()
            .await
            .map_err(|_| unavailable())?;
        let gate = app.simkl.tracking.profile_gate(profile)?;
        let _serial = gate.lock().await;
        tracking::verify(&app, profile, &target, &credential).await?;
        if action=="stop" && completion::confirmed(&app,profile,&q.identity.id,credential.revision.ok_or_else(unavailable)?)? {
            app.simkl.tracking.events.validate(&fence)?;
            store::validate(&app,profile,&credential)?;
            return Ok(axum::Json(json!({"state":"recorded","provider_id":"simkl","action":null,"completed":true,"deduplicated":true})));
        }
        let progress = if q.ended {
            100.0
        } else if q.duration_millis == 0 {
            0.0
        } else {
            (q.position_millis as f64 / q.duration_millis as f64 * 100.0).min(100.0)
        };
        let mut body = json!({"progress":progress});
        if target.category == "movie" {
            body["movie"] = json!({"ids":{"simkl":target.id}});
        } else {
            body["show"] = json!({"ids":{"simkl":target.id}});
            body["episode"] = json!({"season":target.season,"number":target.episode});
        }
        let token = tokens::access(&app,profile,&credential).await?.ok_or_else(unavailable)?;
        let result = app
            .simkl
            .request_checked(
                &format!("/scrobble/{action}"),
                Some(token.as_str()),
                Some(body),
                || {
                    app.simkl.tracking.events.validate(&fence)?;
                    store::validate(&app, profile, &credential)
                },
            )
            .await?;
        app.simkl.tracking.events.validate(&fence)?;
        store::validate(&app, profile, &credential)?;
        let media_key=if target.category=="movie" {"movie"} else {"show"};
        let correct_identity=viptv_engine::simkl::id(&result[media_key])==Some(target.id)
            && (target.category=="movie" || (result["episode"]["season"].as_u64()==target.season.map(u64::from)
                && result["episode"]["number"].as_u64()==target.episode.map(u64::from)));
        if !correct_identity {return Err(ApiError(StatusCode::BAD_GATEWAY,"Simkl returned a different playback item".into()));}
        let expected = if action == "stop" { "scrobble" } else { action };
        if result["action"].as_str() != Some(expected) {
            return Err(ApiError(
                StatusCode::BAD_GATEWAY,
                "Simkl did not confirm the playback event".into(),
            ));
        }
        if action=="stop" {
            completion::confirm(&app,profile,&q.identity.id,credential.revision.ok_or_else(unavailable)?)?;
        }
        Ok(axum::Json(
            json!({"state":"recorded","provider_id":"simkl","action":action,"completed":action=="stop"}),
        ))
    })
    .await
    .map_err(|_| {
        ApiError(
            StatusCode::GATEWAY_TIMEOUT,
            "Simkl tracking timed out".into(),
        )
    })
    .and_then(|result| result);
    match result {
        Err(error) if error.1 == "Simkl authorization is required" => Ok(axum::Json(
            json!({"state":"unavailable","provider_id":"simkl","reason":"tracking_authorization_required"}),
        )),
        result => result,
    }
}
