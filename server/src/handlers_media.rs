use super::*;

pub(crate) fn emit(a: &App, j: &Job, source: &str, result: Result<Vec<Value>, String>) {
    emit_batch(a, j, source, result, true);
}
pub(crate) fn emit_batch(
    a: &App,
    j: &Job,
    source: &str,
    result: Result<Vec<Value>, String>,
    complete: bool,
) {
    if a.request_lease().validate(&a.db.lock().unwrap()).is_err() {
        let mut state = j.state.lock().unwrap();
        state.events.clear();
        state.pending = 0;
        drop(state);
        j.notify.notify_waiters();
        return;
    }
    let (streams, error) = match result {
        Ok(r) => a.register(source, r, &j.kind),
        Err(e) => (vec![], Some(e)),
    };
    let mut state = j.state.lock().unwrap();
    let seq = state.events.len() + 1;
    let mut e = json!({"seq":seq,"source":source,"streams":streams});
    if let Some(error) = error {
        e["error"] = json!(error);
    }
    state.events.push(e);
    if complete {
        state.pending = state.pending.saturating_sub(1);
    }
    drop(state);
    j.notify.notify_waiters();
}
#[derive(Deserialize, Default)]
pub(crate) struct Cursor {
    #[serde(default)]
    after: usize,
}
pub(crate) fn job(a: &App, id: &str) -> Result<Arc<Job>, ApiError> {
    a.prune();
    a.jobs.lock().unwrap().get(id).cloned().ok_or(ApiError(
        StatusCode::NOT_FOUND,
        "Discovery job expired or not found".into(),
    ))
}
pub(crate) async fn poll_streams(
    State(a): State<App>,
    Path(id): Path<String>,
    Query(q): Query<Cursor>,
) -> ApiResult {
    let j = job(&a, &id)?;
    let lease = a.resource_lease("job", &id);
    let db = a.db.lock().unwrap();
    if let Some(lease) = lease {
        lease.validate(&db)?;
    }
    a.request_lease().validate(&db)?;
    let s = j.state.lock().unwrap();
    Ok(axum::Json(
        json!({"events":s.events.iter().skip(q.after).cloned().collect::<Vec<_>>(),"done":s.pending==0}),
    ))
}
pub(crate) async fn stream_events(
    State(a): State<App>,
    Path(id): Path<String>,
    Query(q): Query<Cursor>,
) -> Result<impl IntoResponse, ApiError> {
    let j = job(&a, &id)?;
    let lease = a.resource_lease("job", &id);
    let viewer = a.request_lease();
    if let Some(lease) = &lease {
        lease.validate(&a.db.lock().unwrap())?;
    }
    viewer.validate(&a.db.lock().unwrap())?;
    let stream = async_stream::stream! {
        let mut cursor = q.after;
        let started = Instant::now();
        loop {
            if viewer.validate(&a.db.lock().unwrap()).is_err() || lease.as_ref().is_some_and(|lease| lease.validate(&a.db.lock().unwrap()).is_err()) { break; }
            let notified = j.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let batch = {
                let db = a.db.lock().unwrap();
                if viewer.validate(&db).is_err() || lease.as_ref().is_some_and(|lease| lease.validate(&db).is_err()) { None } else {
                let s = j.state.lock().unwrap();
                Some((s.events.iter().skip(cursor).cloned().collect::<Vec<_>>(), s.pending == 0))
                }
            };
            let Some((events, done)) = batch else { break; };
            for e in events {
                if viewer.validate(&a.db.lock().unwrap()).is_err() || lease.as_ref().is_some_and(|lease| lease.validate(&a.db.lock().unwrap()).is_err()) { return; }
                cursor = e["seq"].as_u64().unwrap_or(0) as usize;
                yield Ok::<_, Infallible>(Event::default().event("streams").id(cursor.to_string()).data(e.to_string()));
            }
            if done {
                yield Ok(Event::default().event("done").data("{}"));
                break;
            }
            if started.elapsed() >= Duration::from_secs(45) {
                yield Ok(Event::default().event("timeout").data("{}"));
                break;
            }
            // Recheck revocation even when an upstream producer is silent.
            let _ = tokio::time::timeout(Duration::from_secs(1), notified).await;
        }
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(10))))
}
#[derive(Deserialize)]
pub(crate) struct LiveQuery {
    view: Option<String>,
    collection: Option<String>,
    category: Option<String>,
    search: Option<String>,
    #[serde(default)]
    offset: usize,
    #[serde(default = "hundred")]
    limit: usize,
}
fn hundred() -> usize {
    100
}
pub(crate) async fn live(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Query(q): Query<LiveQuery>,
) -> ApiResult {
    let a = a.with_lease(lease);
    blocking(move || {
        if q.view.as_deref() == Some("us") {
            let db = a.db.lock().unwrap();
            a.require_media(&db)?;
            let auth::Principal::Account { profile_id, .. } = a.identity();
            return Ok(axum::Json(live_catalog::browse(
                &db,
                q.category.as_deref(),
                q.search.as_deref(),
                q.collection.as_deref(),
                profile_id,
                q.offset,
                q.limit,
            )?));
        }
        Ok(axum::Json(a.providers.live(
            q.category,
            q.search,
            q.offset,
            q.limit.min(200),
        )?))
    })
    .await
}
pub(crate) async fn live_categories(State(a): State<App>, Query(q): Query<LiveQuery>) -> ApiResult {
    blocking(move || {
        if q.view.as_deref() == Some("us") {
            return Ok(axum::Json(live_catalog::categories(&a.db.lock().unwrap())?));
        }
        Ok(axum::Json(
            a.providers.live_categories(q.offset, q.limit.min(100))?,
        ))
    })
    .await
}
pub(crate) async fn guide(State(a): State<App>, Path(id): Path<String>) -> ApiResult {
    Ok(axum::Json(a.providers.guide(id).await?))
}
pub(crate) async fn matches(State(a): State<App>) -> ApiResult {
    blocking(move || Ok(axum::Json(a.providers.matches()?))).await
}
pub(crate) async fn override_match(
    State(a): State<App>,
    axum::Json(v): axum::Json<Value>,
) -> ApiResult {
    blocking(move || {
        a.providers.override_match(v)?;
        Ok(axum::Json(json!({"ok":true})))
    })
    .await
}
