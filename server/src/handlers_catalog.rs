use super::*;

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
#[derive(Clone, Deserialize)]
pub(crate) struct Discover {
    #[serde(rename = "type", default = "movie")]
    pub(crate) kind: String,
    pub(crate) catalog: Option<String>,
    pub(crate) addon_id: Option<i64>,
    #[serde(default)]
    pub(crate) skip: usize,
    pub(crate) search: Option<String>,
    pub(crate) genre: Option<String>,
    pub(crate) extras: Option<String>,
    pub(crate) values: Option<String>,
}
fn movie() -> String {
    "movie".into()
}
impl Discover {
    pub(crate) fn selections(
        &self,
    ) -> Result<Vec<viptv_provider::discovery_values::ExtraValues>, ApiError> {
        match self.values.as_deref() {
            Some(value) if value.len() <= 8192 => serde_json::from_str(value)
                .map_err(|_| ApiError::from("Invalid catalog selections")),
            Some(_) => Err("Catalog selections too large".into()),
            None => Ok(vec![]),
        }
    }
    pub(crate) fn is_search(&self) -> bool {
        self.search
            .as_deref()
            .is_some_and(|term| !term.trim().is_empty())
    }
}
pub(crate) async fn discover(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Query(q): Query<Discover>,
) -> ApiResult {
    let (value, _) = discover_origin(a, lease, q).await?;
    Ok(axum::Json(value))
}
pub(crate) async fn discover_origin(
    a: App,
    lease: ResourceLease,
    q: Discover,
) -> Result<(Value, Option<i64>), ApiError> {
    let a = a.with_lease(lease);
    if q.search.as_ref().is_some_and(|s| s.chars().count() > 256) {
        return Err("Search too long".into());
    }
    if q.genre.as_ref().is_some_and(|s| s.chars().count() > 128) {
        return Err("Genre too long".into());
    }
    let values = q.selections()?;
    let extras = match q.extras {
        Some(value) if value.len() <= 8192 => {
            serde_json::from_str::<HashMap<String, String>>(&value)
                .map_err(|_| ApiError::from("Invalid catalog options"))?
        }
        Some(_) => return Err("Catalog options too large".into()),
        None => HashMap::new(),
    };
    Ok(a.addons
        .discover_with_origin(addon::DiscoverOptions {
            kind: q.kind,
            catalog: q.catalog,
            addon: q.addon_id,
            skip: q.skip,
            search: q.search,
            genre: q.genre,
            extras,
            values,
        })
        .await?)
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
    if v.get("metadata_origin").is_none() && v["card"]["metadata_origin"].is_object() {
        return Err("Metadata origin is required".into());
    }
    let metadata_origin = crate::engine_v3::search::source_origin(&a, &v)?;
    let metadata_proof = if let Some(origin) = v.get("metadata_origin") {
        let origin: viptv_engine::origin::MetadataOrigin =
            serde_json::from_value(origin.clone())
                .map_err(|_| ApiError::from("Invalid metadata origin"))?;
        let expected: viptv_engine::presentation::Card = serde_json::from_value(v["card"].clone())
            .map_err(|_| ApiError::from("A scoped title selection is required"))?;
        if expected.metadata_origin.as_ref() != Some(&origin)
            || v["type"].as_str() != Some(expected.identity.kind.as_str())
            || v["id"].as_str() != Some(expected.identity.id.as_str())
        {
            return Err("Conflicting metadata expectation".into());
        }
        Some(crate::engine_v3::origins::MetadataProof::verified(&a, &expected).await?)
    } else {
        None
    };
    if let Some(proof) = &metadata_proof {
        let mut safe = crate::engine_v3::origins::source_value(&proof.card);
        for key in [
            "only_provider_id",
            "only_addons",
            "metadata_provider_id",
            "metadata_addon_id",
        ] {
            if let Some(value) = v.get(key) {
                safe[key] = value.clone();
            }
        }
        v = safe;
    }
    let allow_iptv = metadata_proof.as_ref().is_none_or(|_| {
        !kids::restricted(&a.db.lock().unwrap(), &a.identity()).unwrap_or(true)
            || !v["imdb_id"].is_null()
            || !v["tmdb_id"].is_null()
            || v["id"].as_str().is_some_and(|id| id.starts_with("simkl:"))
    });
    let metadata_app = if let Some((provider, addon, _)) = &metadata_origin {
        let lease = a.request_lease();
        let auth::Principal::Account {
            profile_id: Some(profile),
            ..
        } = lease.principal
        else {
            return Err(auth::forbidden());
        };
        crate::engine_v3::search::metadata_scope(a.clone(), &lease, profile, provider, *addon)?.0
    } else {
        a.clone()
    };
    let metadata_provider = if let Some((provider, _, _)) = &metadata_origin {
        provider.clone()
    } else {
        let lease = a.request_lease();
        let auth::Principal::Account {
            profile_id: Some(profile),
            ..
        } = lease.principal
        else {
            return Err(auth::forbidden());
        };
        crate::engine_v3::configuration::read(&a, &lease, profile)?
            .0
            .browsing_plugin
    };
    let requested_kind = media_type(&v)?;
    let requested_id = text(&v, "id", 512)?;
    if metadata_proof.is_none() {
        kids::consumers::require(
            &a,
            requested_kind,
            requested_id,
            &metadata_provider,
            metadata_origin.is_some(),
        )?;
    }
    let resolved_lookup = if metadata_origin
        .as_ref()
        .is_some_and(|(provider, _, _)| provider == "stremio")
    {
        None
    } else {
        crate::simkl::resolve_request(&metadata_app, &mut v).await?
    };
    let context = matching_context(&v)?;
    v.as_object_mut()
        .ok_or("Invalid stream request")?
        .extend(context.as_object().unwrap().clone());
    let kind = media_type(&v)?.to_string();
    let id = text(&v, "id", 512)?.to_string();
    let lookup_id = resolved_lookup.unwrap_or_else(|| id.clone());
    a.prune();
    let addons = a.addons.clone();
    let (sources, source_errors) = blocking(move || Ok(addons.entries_with_errors()?)).await?;
    let source_errors = source_errors.into_iter().take(32).collect::<Vec<_>>();
    let sources = sources
        .into_iter()
        .filter(|(_, _, m)| {
            only_provider.is_none() && addon::supports(m, "stream", &kind, &lookup_id)
        })
        .filter(|(source, _, _)| {
            metadata_proof.as_ref().is_none_or(|proof| {
                if viptv_engine::origin::shared_identity(&proof.card.identity) {
                    return true;
                }
                proof
                    .card
                    .metadata_origin
                    .as_ref()
                    .and_then(|o| o.connection_id.as_deref())
                    .and_then(|id| id.strip_prefix("addon:"))
                    .and_then(|id| id.parse::<i64>().ok())
                    == Some(*source)
            })
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
    let sdk_sources = if only_provider.is_none() {
        crate::engine_v3::source_integrations::start(&a, &v).await
    } else {
        vec![]
    };
    let lease = a.request_lease();
    let configuration_revision = match lease.principal {
        auth::Principal::Account {
            profile_id: Some(profile),
            ..
        } => crate::engine_v3::configuration::read(&a, &lease, profile)
            .ok()
            .map(|(c, _)| (profile, c.scope.revision)),
        _ => None,
    };
    let job = Arc::new(Job {
        metadata_proof,
        source_slots: std::sync::atomic::AtomicUsize::new(0),
        cancelled: std::sync::atomic::AtomicBool::new(false),
        configuration_revision,
        media_id: Some(id.clone()),
        exact_vod,
        kind: kind.clone(),
        created: Instant::now(),
        state: Mutex::new(JobState {
            events: vec![],
            pending: sdk_sources.len()
                + sources.len()
                + usize::from(!only_addons && allow_iptv)
                + if only_provider.is_none() {
                    source_errors.len()
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
    #[cfg(feature = "vsources-runtime")]
    for (source, card, config, proof, revision) in sdk_sources {
        crate::engine_v3::source_integrations::spawn(
            a.clone(),
            job.clone(),
            source,
            card,
            config,
            proof,
            revision,
        );
    }
    if only_provider.is_none() {
        for (id, error) in source_errors {
            emit(&a, &job, &format!("addon:{id}"), Err(error));
        }
    }
    for (aid, u, _) in sources {
        let a = a.clone();
        let j = job.clone();
        let kind = kind.clone();
        let id = lookup_id.clone();
        tokio::spawn(async move {
            let source = format!("addon:{aid}");
            let r = tokio::time::timeout(Duration::from_secs(30), a.addons.streams(&u, &kind, &id))
                .await
                .unwrap_or_else(|_| Err("Addon timed out".into()));
            emit(&a, &j, &source, r);
        });
    }
    if !only_addons && allow_iptv {
        tokio::spawn(async move {
            enrich_matching(&mut v, &Value::Null);
            if only_provider.is_none()
                && !id.starts_with("simkl:")
                && kind != "live"
                && (blank_name(&v)
                    || v["year"].is_null()
                    || v["imdb_id"].is_null()
                    || v["tmdb_id"].is_null())
            {
                let base = enrichment_id(&v, &kind, &id);
                if let Ok(Ok(m)) = tokio::time::timeout(
                    Duration::from_secs(8),
                    metadata_app.addons.meta(&kind, &base),
                )
                .await
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
            emit(&a, &job, "iptv", r.map(|_| Vec::new()));
        });
    }
    Ok(axum::Json(json!({"id":jid})))
}
