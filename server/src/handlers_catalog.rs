use super::*;

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
pub(crate) async fn catalogs_revision(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
) -> ApiResult {
    let a = a.with_lease(lease);
    blocking(move || Ok(axum::Json(json!({"revision":a.addons.revision()?})))).await
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
    Query(options): Query<HashMap<String, String>>,
) -> ApiResult {
    let a = a.with_lease(lease);
    if id.len() > 512 || !["movie", "series"].contains(&kind.as_str()) {
        return Err("Invalid metadata request".into());
    }
    if options.get("summary").is_some_and(|value| value == "true") {
        let auth::Principal::Account { profile_id, .. } = a.identity();
        return Ok(axum::Json(a.simkl.summary(&kind, &id, profile_id).await.map_err(ApiError::from)?));
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
    let mut addon_id = Some(id.clone());
    let mut mapping_error = None;
    if viptv_simkl::parse_id(&id).is_some() {
        let auth::Principal::Account { profile_id, .. } = a.identity();
        match a.simkl.mapped(&id, &kind, &v, profile_id).await {
            Ok(item) => {
                enrich_matching(&mut v, &item);
                for (external, field) in [("imdb", "imdb_id"), ("tmdb", "tmdb_id")] {
                    if !item["simkl_ids"][external].is_null() {
                        v[field] = item["simkl_ids"][external].clone();
                    }
                }
                match viptv_simkl::stream_id(&item) {
                    Ok(mapped) => addon_id = Some(mapped),
                    Err(error) => {
                        addon_id = None;
                        mapping_error = Some(error.to_owned());
                    }
                }
            }
            Err(error) => {
                addon_id = None;
                mapping_error = Some(error);
            }
        }
        // A missing addon ID is not a missing IPTV title. Preserve the original
        // identity for playback authorization and try IPTV independently.
        if only_addons && addon_id.is_none() {
            return Err(mapping_error
                .unwrap_or_else(|| "SIMKL stream mapping unavailable".into())
                .into());
        }
    }
    a.prune();
    let addons = a.addons.clone();
    let (sources, source_errors) = blocking(move || Ok(addons.entries_with_errors()?)).await?;
    let source_errors = source_errors.into_iter().take(32).collect::<Vec<_>>();
    let sources = sources
        .into_iter()
        .filter(|(_, _, m)| {
            only_provider.is_none()
                && addon_id
                    .as_ref()
                    .is_some_and(|mapped| addon::supports(m, "stream", &kind, mapped))
        })
        .take(32)
        .collect::<Vec<_>>();
    a.require_media(&a.db.lock().unwrap())?;
    let exact_vod = match kind.as_str() {
        "movie" => Some(app_state::ExactVod {
            title: id.clone(),
            series: None,
            season: None,
            episode: None,
        }),
        "series" => match (v["season"].as_u64(), v["episode"].as_u64()) {
            (Some(season), Some(episode)) => Some(app_state::ExactVod {
                title: id.clone(),
                series: v["series_id"].as_str().map(str::to_owned),
                season: Some(season as u32),
                episode: Some(episode as u32),
            }),
            _ => None,
        },
        _ => None,
    };
    let job = Arc::new(Job {
        exact_vod,
        kind: kind.clone(),
        created: Instant::now(),
        state: Mutex::new(JobState {
            events: vec![],
            pending: sources.len()
                + usize::from(!only_addons)
                + if only_provider.is_none() {
                    source_errors.len() + usize::from(mapping_error.is_some())
                } else {
                    0
                },
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
    if only_provider.is_none() {
        if let Some(error) = mapping_error {
            emit(&a, &job, "addon", Err(error));
        }
        for (id, error) in source_errors {
            emit(&a, &job, &format!("addon:{id}"), Err(error));
        }
    }
    for (aid, u, _) in sources {
        let a = a.clone();
        let j = job.clone();
        let kind = kind.clone();
        let id = addon_id.clone().expect("mapped stream producer");
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
