use super::*;
use viptv_engine::presentation::{CatalogPage, DetailSnapshot};

pub(crate) async fn discover(State(app):State<App>,Extension(lease):Extension<ResourceLease>,Query(q):Query<crate::handlers_catalog::Discover>)->ApiResult{
    if q.kind!="all" {return discover_one(State(app),Extension(lease),Query(q)).await;}
    if !q.is_search() || q.skip!=0 || q.catalog.is_some() || q.addon_id.is_some() || q.genre.is_some() || q.extras.as_deref().is_some_and(|s|s!="{}") || q.search.as_ref().is_some_and(|s|s.chars().count()>256){return Err("All-types search requires an unfiltered text query at offset zero".into());}
    let auth::Principal::Account {profile_id:Some(profile),..}=lease.principal else{return Err(auth::forbidden());};
    let scoped=app.clone().with_lease(lease.clone());
    let (configuration,_)=configuration::read(&scoped,&lease,profile)?;
    let kinds=if configuration.browsing_plugin=="simkl"{vec!["movie".to_owned(),"series".to_owned(),"anime".to_owned()]}else{
        let (selected,_)=configuration::apply(scoped.clone(),&lease,profile,viptv_engine::capabilities::Capability::Search)?;
        let mut kinds=std::collections::BTreeSet::new();
        for catalog in selected.addons.catalogs()?.as_array().into_iter().flatten(){
            if catalog["supports_search"]!=true{continue;}
            if let Some(kind)=catalog["type"].as_str().filter(|kind|matches!(*kind,"movie"|"series")){kinds.insert(kind.to_owned());}
        }
        kinds.into_iter().collect()
    };
    if kinds.is_empty(){return Err(ApiError(StatusCode::UNPROCESSABLE_ENTITY,"Search is not configured for this profile".into()));}
    let tasks=kinds.into_iter().take(3).map(|kind|{
        let mut query=q.clone();query.kind=kind;
        discover_one(State(app.clone()),Extension(lease.clone()),Query(query))
    });
    let pages=tokio::time::timeout(Duration::from_secs(12),futures::future::join_all(tasks)).await.map_err(|_|ApiError(StatusCode::GATEWAY_TIMEOUT,"Search exceeded its response deadline".into()))?;
    let mut cards=Vec::new();let mut seen=std::collections::HashSet::new();let mut complete=true;
    for result in pages {
        let axum::Json(value)=match result {Ok(value)=>value,Err(error) if error.0.is_server_error()=>{complete=false;continue;},Err(error)=>return Err(error)};
        let page:CatalogPage=serde_json::from_value(value).map_err(|_|ApiError::from("Search response was invalid"))?;
        complete&=page.complete;
        for card in page.cards {if cards.len()<200 && seen.insert((card.identity.kind.clone(),card.identity.id.clone())){cards.push(card);}}
    }
    viptv_engine::search::rank(&mut cards,q.search.as_deref().unwrap_or_default());
    configuration::validate_revision(&scoped,profile,configuration.scope.revision)?;
    Ok(axum::Json(serde_json::to_value(CatalogPage{cards,next_cursor:None,complete}).map_err(|_|ApiError::from("Search unavailable"))?))
}

async fn discover_one(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Query(q): Query<crate::handlers_catalog::Discover>,
) -> ApiResult {
    let capability = if q.is_search() {
        viptv_engine::capabilities::Capability::Search
    } else {
        viptv_engine::capabilities::Capability::Discovery
    };
    let auth::Principal::Account { profile_id, .. } = &lease.principal;
    let profile = profile_id.ok_or_else(auth::forbidden)?;
    let scoped = app.with_lease(lease.clone());
    let (configuration, _) = configuration::read(&scoped, &lease, profile)?;
    if configuration.browsing_plugin == "simkl" {
        let result = super::simkl_catalog::discover(scoped.clone(), lease, profile, q).await?;
        configuration::validate_revision(&scoped, profile, configuration.scope.revision)?;
        return Ok(result);
    }
    let (scoped, revision) = configuration::apply(scoped, &lease, profile, capability)?;
    let axum::Json(value) =
        crate::handlers_catalog::discover(State(scoped.clone()), Extension(lease), Query(q))
            .await?;
    configuration::validate_revision(&scoped, profile, revision)?;
    let items=value["metas"].as_array().cloned().unwrap_or_default();
    let mut cards=Vec::new();
    for items in items.chunks(24).take(9){cards.extend(projection::cards(&scoped,items.to_vec(),"stremio")?);}

    let page = CatalogPage {
        cards,
        next_cursor: value["next_skip"].as_u64().map(|n| n.to_string()),
        complete: value["has_more"].as_bool() == Some(false),
    };
    Ok(axum::Json(
        serde_json::to_value(page).map_err(|_| ApiError::from("Catalog unavailable"))?,
    ))
}

#[derive(Default, Deserialize)]
pub(crate) struct DetailPage {
    #[serde(default)]
    pub(crate) offset: usize,
    pub(crate) limit:Option<usize>,
}

pub(crate) async fn detail(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path((kind, id)): Path<(String, String)>,
    Query(page): Query<DetailPage>,
) -> ApiResult {
    if id.is_empty()
        || id.len() > 512
        || !["movie", "series"].contains(&kind.as_str())
        || page.offset > 100_000 || page.limit.is_some_and(|limit|!(1..=128).contains(&limit))
    {
        return Err("Invalid metadata request".into());
    }
    let auth::Principal::Account { profile_id, .. } = &lease.principal;
    let profile = profile_id.ok_or_else(auth::forbidden)?;
    let app = app.with_lease(lease.clone());
    let (configuration, _) = configuration::read(&app, &lease, profile)?;
    if configuration.browsing_plugin == "simkl" {
        let result =
            super::simkl_catalog::detail(app.clone(), lease.clone(), profile, kind, id, page.offset,page.limit)
                .await?;
        configuration::validate_revision(&app, profile, configuration.scope.revision)?;
        super::source_integrations::cache_detail(&app,&lease,configuration.scope.revision,&result.0);
        return Ok(result);
    }
    let (app, revision) = configuration::apply(
        app,
        &lease,
        profile,
        viptv_engine::capabilities::Capability::Metadata,
    )?;
    {
        let db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        app.require_media(&db)?;
        kids::require_item(&db, &app.identity(), &kind, &id)?;
    }
    let value = app.addons.meta(&kind, &id).await?;
    configuration::validate_revision(&app, profile, revision)?;
    let meta = if value["meta"].is_object() {
        value["meta"].clone()
    } else {
        value
    };
    if meta["id"].as_str() != Some(id.as_str()) {
        return Err(ApiError(
            StatusCode::BAD_GATEWAY,
            "Provider returned a different title".into(),
        ));
    }
    let mut parent = meta.clone();
    parent["type"] = json!(kind);
    parent["id"] = json!(id);
    let item = projection::cards(&app, vec![parent.clone()], "stremio")?
        .into_iter()
        .next()
        .ok_or_else(auth::forbidden)?;
    let mut parent=parent;
    let videos=parent["videos"].as_array().cloned().unwrap_or_default().into_iter().map(|mut video|{video["type"]=json!("series");video}).collect();
    {let db=app.db.lock().map_err(|_|ApiError::from("Database unavailable"))?;app.require_media(&db)?;
    parent["videos"]=json!(kids::filter_items(&db,&app.identity(),videos)?);}
    let mut projected=viptv_engine::stremio::detail_page(&json!({"meta":parent}).to_string(),"stremio",&kind,&id,page.offset,page.limit.unwrap_or(24)).map_err(|_|ApiError::from("Invalid episode metadata"))?;
    let seasons=projected.seasons;let episodes=projected.episodes;let next_cursor=projected.next_cursor;
    let recommendations=std::mem::take(&mut projected.recommendations);
    let mut recommendations=super::projection::allowed_cards(&app,recommendations)?;
    if recommendations.is_empty(){
        if let Some(genre)=item.metadata.as_ref().and_then(|metadata|metadata.genres.first()){
            if let Ok((discovery,_))=configuration::apply(app.clone(),&lease,profile,viptv_engine::capabilities::Capability::Discovery){
                if let Ok(Ok(result))=tokio::time::timeout(Duration::from_secs(2),discovery.addons.discover(kind.clone(),None,None,0,None,Some(genre.clone()))).await{
                    recommendations=projection::cards(&discovery,result["metas"].as_array().cloned().unwrap_or_default(),"stremio")?.into_iter().filter(|card|card.identity.id!=item.identity.id && card.identity.kind==item.identity.kind && card.identity.episode.is_none()).take(24).collect();
                }
            }
        }
    }
    configuration::validate_revision(&app,profile,revision)?;
    let value=serde_json::to_value(DetailSnapshot{item,recommendations,episodes,next_cursor,seasons}).map_err(|_|ApiError::from("Title unavailable"))?;
    super::source_integrations::cache_detail(&app,&lease,revision,&value);
    Ok(axum::Json(value))
}
