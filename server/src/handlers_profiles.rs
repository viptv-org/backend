use super::*;

// Authenticated wrappers attach the validated request lease to handler state.
pub(crate) async fn poll_streams_authenticated(
    State(mut app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    path: Path<String>,
    query: Query<Cursor>,
) -> ApiResult {
    app.lease = Some(lease);
    poll_streams(State(app), path, query).await
}
pub(crate) async fn stream_events_authenticated(
    State(mut app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    path: Path<String>,
    query: Query<Cursor>,
) -> Result<impl IntoResponse, ApiError> {
    app.lease = Some(lease);
    stream_events(State(app), path, query).await
}
pub(crate) async fn profiles_authenticated(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
) -> ApiResult {
    profiles(State(app.with_lease(lease))).await
}
pub(crate) async fn create_profile_authenticated(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    value: axum::Json<Value>,
) -> ApiResult {
    create_profile(State(app.with_lease(lease)), value).await
}
pub(crate) async fn update_profile_authenticated(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    path: Path<i64>,
    value: axum::Json<Value>,
) -> ApiResult {
    update_profile(State(app.with_lease(lease)), path, value).await
}
pub(crate) async fn delete_profile_authenticated(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<i64>,
) -> ApiResult {
    let app = app.with_lease(lease);
    let worker = app.clone();
    blocking(move || {
        let mut db = worker.db.lock().unwrap();
        let tx = db.transaction().map_err(db_error)?;
        worker.request_lease().validate(&tx)?;
        let principal = worker.identity();
        kids::require_parent(&tx, &principal)?;
        let account = principal.account_id().ok_or("Account required")?;
        let owns: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM profile_owners WHERE account_id=?1 AND profile_id=?2)",
                params![account, id],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        if !owns {
            return Err(ApiError(
                StatusCode::FORBIDDEN,
                "Profile access denied".into(),
            ));
        }
        if auth::primary_profile(&tx, account)? == Some(id) {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "The primary profile cannot be deleted".into(),
            ));
        }
        tx.execute("DELETE FROM profiles WHERE id=?1", [id])
            .map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        Ok(())
    })
    .await?;
    // Captured leases retain the old profile identity and cannot serve media after deletion.
    // Release only affected clients; shared workers used by another profile survive.
    let resources: Vec<String> = app
        .resource_owners
        .lock()
        .unwrap()
        .iter()
        .filter_map(|(key, owner)| {
            let auth::Principal::Account { profile_id, .. } = owner.lease.principal;
            (profile_id == Some(id)).then(|| key.clone())
        })
        .collect();
    for resource in resources {
        if let Some(session) = resource.strip_prefix("playback:") {
            session::stop_owned(&app, session).await;
        }
        if let Some(job) = resource.strip_prefix("job:") {
            app.jobs.lock().unwrap().remove(job);
        }
        if let Some(stream) = resource.strip_prefix("stream:") {
            app.streams.lock().unwrap().remove(stream);
        }
        app.resource_owners.lock().unwrap().remove(&resource);
    }
    Ok(axum::Json(json!({"deleted":true})))
}
pub(crate) async fn favorites_authenticated(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    path: Path<i64>,
) -> ApiResult {
    favorites(State(app.with_lease(lease)), path).await
}
pub(crate) async fn save_favorite_authenticated(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    path: Path<i64>,
    value: axum::Json<Value>,
) -> ApiResult {
    save_favorite(State(app.with_lease(lease)), path, value).await
}
pub(crate) async fn delete_favorite_authenticated(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    path: Path<(i64, String, String)>,
) -> ApiResult {
    delete_favorite(State(app.with_lease(lease)), path).await
}
pub(crate) async fn progress_authenticated(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    path: Path<i64>,
) -> ApiResult {
    progress(State(app.with_lease(lease)), path).await
}
pub(crate) async fn save_progress_authenticated(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    path: Path<i64>,
    value: axum::Json<Value>,
) -> ApiResult {
    save_progress(State(app.with_lease(lease)), path, value).await
}
pub(crate) async fn start_streams_authenticated(
    State(mut app): State<App>,
    Extension(p): Extension<auth::Principal>,
    Extension(lease): Extension<ResourceLease>,
    value: axum::Json<Value>,
) -> ApiResult {
    app.principal = Some(p);
    app = app.with_lease(lease);
    start_streams(State(app), value).await
}
pub(crate) async fn start_playback_authenticated(
    State(mut app): State<App>,
    Extension(p): Extension<auth::Principal>,
    Extension(lease): Extension<ResourceLease>,
    value: axum::Json<PlaybackRequest>,
) -> ApiResult {
    app.principal = Some(p);
    app = app.with_lease(lease);
    start_playback(State(app), value).await
}
pub(crate) fn text<'a>(v: &'a Value, key: &str, max: usize) -> Result<&'a str, ApiError> {
    let s = v[key]
        .as_str()
        .ok_or_else(|| ApiError::from(format!("Missing {key}")))?;
    if s.trim().is_empty() || s.len() > max {
        return Err(format!("Invalid {key}").into());
    }
    Ok(s)
}
pub(crate) fn media_type(v: &Value) -> Result<&str, ApiError> {
    let k = text(v, "type", 16)?;
    if !["movie", "series", "live"].contains(&k) {
        return Err("Invalid media type".into());
    }
    Ok(k)
}
pub(crate) async fn profiles(State(a): State<App>) -> ApiResult {
    blocking(move || {
        let db = a.db.lock().unwrap();
        a.request_lease().validate(&db)?;
        let account_id = a.identity().account_id().ok_or_else(auth::unauthorized)?;
        Ok(axum::Json(json!(auth::list_profiles(&db, account_id)?)))
    })
    .await
}
pub(crate) async fn create_profile(
    State(a): State<App>,
    axum::Json(v): axum::Json<Value>,
) -> ApiResult {
    blocking(move || {
        let mut db = a.db.lock().unwrap();
        let tx = db.transaction().map_err(db_error)?;
        a.request_lease().validate(&tx)?;
        let account_id = a.identity().account_id().ok_or_else(auth::unauthorized)?;
        kids::require_parent(&tx, &a.identity())?;
        let profile = auth::create_profile(&tx, account_id, &v)?;
        tx.commit().map_err(db_error)?;
        Ok(axum::Json(profile))
    })
    .await
}
pub(crate) async fn update_profile(
    State(a): State<App>,
    Path(id): Path<i64>,
    axum::Json(v): axum::Json<Value>,
) -> ApiResult {
    blocking(move || {
        let mut db = a.db.lock().unwrap();
        let tx = db.transaction().map_err(db_error)?;
        a.request_lease().validate(&tx)?;
        let account_id = a.identity().account_id().ok_or_else(auth::unauthorized)?;
        kids::require_parent(&tx, &a.identity())?;
        let profile = auth::update_profile(&tx, account_id, id, &v)?;
        tx.commit().map_err(db_error)?;
        Ok(axum::Json(profile))
    })
    .await
}

pub(crate) async fn favorites(State(a): State<App>, Path(id): Path<i64>) -> ApiResult {
    blocking(move || {
    let db = a.db.lock().unwrap();
    a.require_profile(&db, id)?;
    let mut q=db.prepare("SELECT id,type,name,poster FROM favorites WHERE profile_id=?1 ORDER BY name LIMIT 5000").map_err(db_error)?;
    let r=q.query_map([id],|r|Ok(json!({"id":r.get::<_,String>(0)?,"type":r.get::<_,String>(1)?,"name":r.get::<_,String>(2)?,"poster":r.get::<_,Option<String>>(3)?}))).map_err(db_error)?.collect::<Result<Vec<_>,_>>().map_err(db_error)?;
    Ok(axum::Json(json!(lineup::references(&db,r).map_err(db_error)?)))
    }).await
}
pub(crate) async fn save_favorite(
    State(a): State<App>,
    Path(profile): Path<i64>,
    axum::Json(v): axum::Json<Value>,
) -> ApiResult {
    blocking(move || {
    let id = text(&v, "id", 512)?;
    let kind = media_type(&v)?;
    let name = text(&v, "name", 512)?;
    let db = a.db.lock().unwrap();
    a.require_profile(&db, profile)?;
    db.execute("INSERT INTO favorites(profile_id,id,type,name,poster) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(profile_id,type,id) DO UPDATE SET name=excluded.name,poster=excluded.poster",params![profile,id,kind,name,v["poster"].as_str()]).map_err(db_error)?;
    Ok(axum::Json(json!({"ok":true})))
    }).await
}
pub(crate) async fn delete_favorite(
    State(a): State<App>,
    Path((profile, kind, id)): Path<(i64, String, String)>,
) -> ApiResult {
    blocking(move || {
        let db = a.db.lock().unwrap();
        a.require_profile(&db, profile)?;
        db.execute(
            "DELETE FROM favorites WHERE profile_id=?1 AND type=?2 AND (id=?3 OR (?2='live' AND id IN (SELECT live_id FROM family_aliases WHERE channel_id=?3)))",
            params![profile, kind, if kind == "live" { lineup::canonical_id(&db,&id).map_err(db_error)? } else { id }],
        )
        .map_err(db_error)?;
        Ok(axum::Json(json!({"ok":true})))
    })
    .await
}

pub(crate) async fn progress(State(a): State<App>, Path(id): Path<i64>) -> ApiResult {
    blocking(move || {
    let db = a.db.lock().unwrap();
    a.require_profile(&db, id)?;
    let mut q=db.prepare("SELECT id,type,name,poster,position,duration,updated_at,context FROM progress WHERE profile_id=?1 ORDER BY updated_at DESC,rowid DESC LIMIT 500").map_err(db_error)?;
    let r=q.query_map([id],|r| {
        let mut item = json!({"id":r.get::<_,String>(0)?,"type":r.get::<_,String>(1)?,"name":r.get::<_,String>(2)?,"poster":r.get::<_,Option<String>>(3)?,"position":r.get::<_,f64>(4)?,"duration":r.get::<_,f64>(5)?,"updated_at":r.get::<_,i64>(6)?});
        if let Ok(raw) = serde_json::from_str::<Value>(&r.get::<_,String>(7)?) {
            item["watched"]=json!(library::watched(&item,&raw));
            if let Ok(context) = matching_context(&raw) { item.as_object_mut().unwrap().extend(context.as_object().unwrap().clone()); }
        }
        Ok(item)
    }).map_err(db_error)?.collect::<Result<Vec<_>,_>>().map_err(db_error)?;
    Ok(axum::Json(json!(lineup::references(&db,r).map_err(db_error)?)))
    }).await
}

pub(crate) async fn continue_watching_authenticated(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    path: Path<i64>,
) -> ApiResult {
    let axum::Json(page) = continuation::page(
        State(app),
        Extension(lease),
        path,
        Query(continuation::Page::default()),
    )
    .await?;
    Ok(axum::Json(page["items"].clone()))
}
pub(crate) async fn save_progress(
    State(a): State<App>,
    Path(profile): Path<i64>,
    axum::Json(v): axum::Json<Value>,
) -> ApiResult {
    blocking(move || {
    let id = text(&v, "id", 512)?;
    let kind = media_type(&v)?;
    let name = text(&v, "name", 512)?;
    let p = v["position"].as_f64().ok_or("Invalid position")?;
    let d = v["duration"].as_f64().ok_or("Invalid duration")?;
    if p < 0.0 || d < 0.0 || p > 1e9 || d > 1e9 {
        return Err("Invalid playback position".into());
    }
    let context = matching_context(&v)?;
    let db = a.db.lock().unwrap();
    a.require_profile(&db, profile)?;
    db.execute("INSERT INTO progress(profile_id,id,type,name,poster,position,duration,updated_at,context,title_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT(profile_id,type,id) DO UPDATE SET name=excluded.name,poster=excluded.poster,position=excluded.position,duration=excluded.duration,updated_at=excluded.updated_at,context=json_remove(json_patch(progress.context,excluded.context),'$.watched_override'),title_id=CASE WHEN json_extract(excluded.context,'$.series_id') IS NULL AND json_extract(progress.context,'$.series_id') IS NOT NULL THEN json_extract(progress.context,'$.series_id') ELSE excluded.title_id END",params![profile,id,kind,name,v["poster"].as_str(),p,d,library::activity_time(&db,profile)?,context.to_string(),continuation::title_id(&v)]).map_err(db_error)?;
    Ok(axum::Json(json!({"ok":true})))
    }).await
}
pub(crate) async fn status(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
) -> ApiResult {
    let a = a.with_lease(lease);
    a.prune_playback_owners().await;
    let db_app = a.clone();
    let (providers, addons, profiles) = blocking(move || {
        let providers = db_app
            .providers
            .list()?
            .as_array()
            .map(Vec::len)
            .unwrap_or(0);
        let addons = db_app.addons.entries()?.len();
        let profiles: i64 = db_app
            .db
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM profiles", [], |r| r.get(0))
            .map_err(db_error)?;
        Ok((providers, addons, profiles))
    })
    .await?;
    Ok(axum::Json(
        json!({"providers":providers,"addons":addons,"profiles":profiles,"active_sessions":a.playback.active_count().await,"shared_playback":session::shared::diagnostics(&a),"ffmpeg_available":a.playback.ffmpeg_available().await,"video_acceleration":a.playback.acceleration_status(),"hdr_tone_mapping":a.playback.tone_mapping_status()}),
    ))
}
