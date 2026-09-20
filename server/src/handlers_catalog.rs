use super::*;

pub(crate) async fn providers(State(a): State<App>) -> ApiResult {
    blocking(move || Ok(axum::Json(a.providers.list()?))).await
}
pub(crate) async fn add_provider(
    State(a): State<App>,
    axum::Json(v): axum::Json<Value>,
) -> ApiResult {
    blocking(move || Ok(axum::Json(a.providers.add(v)?))).await
}
pub(crate) async fn delete_provider(State(a): State<App>, Path(id): Path<i64>) -> ApiResult {
    blocking(move || {
        a.providers.delete(id)?;
        Ok(axum::Json(json!({"ok":true})))
    })
    .await
}
pub(crate) async fn update_provider(
    State(a): State<App>,
    Path(id): Path<i64>,
    axum::Json(v): axum::Json<Value>,
) -> ApiResult {
    blocking(move || {
        a.providers
            .update(id, v)
            .map(axum::Json)
            .map_err(|message| {
                if message == "Stop provider playback before changing max_connections" {
                    ApiError(StatusCode::CONFLICT, message)
                } else {
                    ApiError::from(message)
                }
            })
    })
    .await
}
pub(crate) async fn sync_provider(State(a): State<App>, Path(id): Path<i64>) -> ApiResult {
    Ok(axum::Json(a.providers.sync(id).await?))
}
pub(crate) async fn addons(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
) -> ApiResult {
    let a = a.with_lease(lease);
    blocking(move || Ok(axum::Json(a.addons.list()?))).await
}
pub(crate) async fn update_addon(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<i64>,
    axum::Json(v): axum::Json<Value>,
) -> ApiResult {
    let a = a.with_lease(lease);
    blocking(move || Ok(axum::Json(a.addons.update(id, v)?))).await
}
pub(crate) async fn add_addon(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    axum::Json(v): axum::Json<Value>,
) -> ApiResult {
    let a = a.with_lease(lease);
    Ok(axum::Json(
        a.addons.add(text(&v, "manifest_url", 4096)?).await?,
    ))
}
pub(crate) async fn delete_addon(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<i64>,
) -> ApiResult {
    let a = a.with_lease(lease);
    blocking(move || {
        a.addons.delete(id)?;
        Ok(axum::Json(json!({"ok":true})))
    })
    .await
}
pub(crate) async fn catalogs(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
) -> ApiResult {
    let a = a.with_lease(lease);
    blocking(move || Ok(axum::Json(a.addons.catalogs()?))).await
}
#[derive(Deserialize)]
pub(crate) struct Discover {
    #[serde(rename = "type", default = "movie")]
    kind: String,
    catalog: Option<String>,
    addon_id: Option<i64>,
    #[serde(default)]
    skip: usize,
    search: Option<String>,
    genre: Option<String>,
    extras: Option<String>,
}
fn movie() -> String {
    "movie".into()
}
pub(crate) async fn discover(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Query(q): Query<Discover>,
) -> ApiResult {
    let a = a.with_lease(lease);
    if q.search.as_ref().is_some_and(|s| s.chars().count() > 256) {
        return Err("Search too long".into());
    }
    if q.genre.as_ref().is_some_and(|s| s.chars().count() > 128) {
        return Err("Genre too long".into());
    }
    let extras = match q.extras {
        Some(value) if value.len() <= 8192 => {
            serde_json::from_str::<HashMap<String, String>>(&value)
                .map_err(|_| ApiError::from("Invalid catalog options"))?
        }
        Some(_) => return Err("Catalog options too large".into()),
        None => HashMap::new(),
    };
    Ok(axum::Json(
        a.addons
            .discover_with_options(addon::DiscoverOptions {
                kind: q.kind,
                catalog: q.catalog,
                addon: q.addon_id,
                skip: q.skip,
                search: q.search,
                genre: q.genre,
                extras,
            })
            .await?,
    ))
}
pub(crate) async fn meta(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path((kind, id)): Path<(String, String)>,
) -> ApiResult {
    let a = a.with_lease(lease);
    if id.len() > 512 || !["movie", "series"].contains(&kind.as_str()) {
        return Err("Invalid metadata request".into());
    }
    Ok(axum::Json(a.addons.meta(&kind, &id).await?))
}
pub(crate) async fn start_streams(
    State(a): State<App>,
    axum::Json(mut v): axum::Json<Value>,
) -> ApiResult {
    // Explicit IPTV continuation scopes discovery, without changing ordinary source browsing.
    let only_provider = match v.get("only_provider_id") {
        None => None,
        Some(value) => Some(
            value
                .as_i64()
                .filter(|id| *id > 0)
                .ok_or("Invalid provider scope")?,
        ),
    };
    let only_addons = match v.get("only_addons") {
        None => false,
        Some(value) => value.as_bool().ok_or("Invalid addon scope")?,
    };
    if only_addons && only_provider.is_some() {
        return Err("Conflicting discovery scopes".into());
    }
    let context = matching_context(&v)?;
    v.as_object_mut()
        .ok_or("Invalid stream request")?
        .extend(context.as_object().unwrap().clone());
    let kind = media_type(&v)?.to_string();
    let id = text(&v, "id", 512)?.to_string();
    a.prune();
    let addons = a.addons.clone();
    let sources = blocking(move || Ok(addons.entries()?))
        .await?
        .into_iter()
        .filter(|(_, _, m)| only_provider.is_none() && addon::supports(m, "stream", &kind, &id))
        .take(32)
        .collect::<Vec<_>>();
    a.require_media(&a.db.lock().unwrap())?;
    let job = Arc::new(Job {
        kind: kind.clone(),
        created: Instant::now(),
        state: Mutex::new(JobState {
            events: vec![],
            pending: sources.len() + usize::from(!only_addons),
        }),
        notify: Notify::new(),
    });
    let jid = Uuid::new_v4().to_string();
    {
        let mut jobs = a.jobs.lock().unwrap();
        if jobs.len() >= 256 {
            return Err(ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many discovery jobs; retry later".into(),
            ));
        }
        jobs.insert(jid.clone(), job.clone());
        a.own_resource("job", &jid);
    }
    for (aid, u, _) in sources {
        let a = a.clone();
        let j = job.clone();
        let kind = kind.clone();
        let id = id.clone();
        tokio::spawn(async move {
            let source = format!("addon:{aid}");
            let r = tokio::time::timeout(Duration::from_secs(30), a.addons.streams(&u, &kind, &id))
                .await
                .unwrap_or_else(|_| Err("Addon timed out".into()));
            emit(&a, &j, &source, r);
        });
    }
    if !only_addons {
        tokio::spawn(async move {
            enrich_matching(&mut v, &Value::Null);
            if only_provider.is_none()
                && kind != "live"
                && (blank_name(&v)
                    || v["year"].is_null()
                    || v["imdb_id"].is_null()
                    || v["tmdb_id"].is_null())
            {
                let base = enrichment_id(&v, &kind, &id);
                if let Ok(Ok(m)) =
                    tokio::time::timeout(Duration::from_secs(8), a.addons.meta(&kind, &base)).await
                {
                    enrich_matching(&mut v, &m["meta"]);
                }
            }
            let r = a
                .providers
                .stream_batches(v, |source, batch| {
                    emit_batch(&a, &job, &source, batch, false);
                })
                .await;
            // One pending token represents the entire IPTV producer, not each batch.
            // Final completion never discards already registered sources.
            emit(&a, &job, "iptv", r.map(|_| Vec::new()));
        });
    }
    Ok(axum::Json(json!({"id":jid})))
}
