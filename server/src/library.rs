//! Bounded personal libraries and deliberate viewing-history corrections.
use super::*;
use serde::Deserialize;

#[derive(Default, Deserialize)]
pub struct Page {
    offset: Option<u32>,
    limit: Option<u32>,
    exclude_live: Option<bool>,
}
pub async fn favorites_page(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    Query(page): Query<Page>,
) -> ApiResult {
    let app = app.with_lease(lease);
    blocking(move || {
        let db=app.db.lock().unwrap(); app.require_profile(&db,profile)?;
        let offset=page.offset.unwrap_or(0); let limit=page.limit.unwrap_or(40).clamp(1,100);
        let allowed=kids::sql_allowed("favorites.type","favorites.id","?1");
        let total:i64=db.query_row(&format!("SELECT count(*) FROM favorites WHERE profile_id=?1 AND (?2=0 OR type!='live') AND {allowed}"),params![profile,page.exclude_live.unwrap_or(false)],|r|r.get(0)).map_err(db_error)?;
        let items=db.prepare(&format!("SELECT id,type,name,poster FROM favorites WHERE profile_id=?1 AND (?4=0 OR type!='live') AND {allowed} ORDER BY name COLLATE NOCASE,type,id LIMIT ?2 OFFSET ?3")).map_err(db_error)?
            .query_map(params![profile,limit,offset,page.exclude_live.unwrap_or(false)],|r|Ok(json!({"id":r.get::<_,String>(0)?,"type":r.get::<_,String>(1)?,"name":r.get::<_,String>(2)?,"poster":r.get::<_,Option<String>>(3)?}))).map_err(db_error)?.collect::<Result<Vec<_>,_>>().map_err(db_error)?;
        Ok(axum::Json(json!({"items":lineup::references(&db,items).map_err(db_error)?,"offset":offset,"total":total,"next_offset":((offset as i64 + limit as i64)<total).then_some(offset as i64+limit as i64)})))
    }).await
}
pub async fn toggle(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    axum::Json(item): axum::Json<Value>,
) -> ApiResult {
    let app = app.with_lease(lease);
    blocking(move || {
        let id=text(&item,"id",512)?; let kind=media_type(&item)?; let name=text(&item,"name",512)?;
        let mut db=app.db.lock().unwrap(); app.require_profile(&db,profile)?;
        let id=if kind=="live" {lineup::canonical_id(&db,id).map_err(db_error)?}else{id.to_owned()};
        kids::require_item(&db,&app.identity(),kind,&id)?;
        let tx=db.transaction().map_err(db_error)?;
        let removed=tx.execute("DELETE FROM favorites WHERE profile_id=?1 AND type=?2 AND (id=?3 OR (?2='live' AND id IN (SELECT live_id FROM family_aliases WHERE channel_id=?3)))",params![profile,kind,id]).map_err(db_error)?;
        if removed==0 {tx.execute("INSERT INTO favorites(profile_id,id,type,name,poster) VALUES(?1,?2,?3,?4,?5)",params![profile,id,kind,name,item["poster"].as_str()]).map_err(db_error)?;}
        tx.commit().map_err(db_error)?;
        Ok(axum::Json(json!({"ok":true,"saved":removed==0})))
    }).await
}

pub async fn history_page(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    Query(page): Query<Page>,
) -> ApiResult {
    let app = app.with_lease(lease);
    blocking(move || {
        let db=app.db.lock().unwrap(); app.require_profile(&db,profile)?;
        let offset=page.offset.unwrap_or(0); let limit=page.limit.unwrap_or(20).clamp(1,100);
        let allowed=kids::sql_allowed("progress.type","progress.id","?1");
        let total:i64=db.query_row(&format!("SELECT count(*) FROM progress WHERE profile_id=?1 AND type!='live' AND {allowed}"),[profile],|r|r.get(0)).map_err(db_error)?;
        let items=db.prepare(&format!("SELECT id,type,name,poster,position,duration,updated_at,context FROM progress WHERE profile_id=?1 AND type!='live' AND {allowed} ORDER BY updated_at DESC,rowid DESC LIMIT ?2 OFFSET ?3")).map_err(db_error)?
            .query_map(params![profile,limit,offset],|r|{
                let mut item=json!({"id":r.get::<_,String>(0)?,"type":r.get::<_,String>(1)?,"name":r.get::<_,String>(2)?,"poster":r.get::<_,Option<String>>(3)?,"position":r.get::<_,f64>(4)?,"duration":r.get::<_,f64>(5)?,"updated_at":r.get::<_,i64>(6)?});
                if let Ok(raw)=serde_json::from_str::<Value>(&r.get::<_,String>(7)?){item["watched"]=json!(watched(&item,&raw));if let Ok(context)=matching_context(&raw){item.as_object_mut().unwrap().extend(context.as_object().unwrap().clone());}}
                Ok(item)
            }).map_err(db_error)?.collect::<Result<Vec<_>,_>>().map_err(db_error)?;
        Ok(axum::Json(json!({"items":items,"offset":offset,"total":total,"next_offset":((offset as i64+limit as i64)<total).then_some(offset as i64+limit as i64)})))
    }).await
}

pub async fn correct(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    axum::Json(item): axum::Json<Value>,
) -> ApiResult {
    use rusqlite::OptionalExtension;
    let app = app.with_lease(lease);
    blocking(move || {
        let id=text(&item,"id",512)?; let kind=media_type(&item)?; let name=text(&item,"name",512)?;
        if kind=="live" {return Err("Live channels do not have episode progress".into());}
        let action=text(&item,"action",32)?;
        let context=matching_context(&item)?;
        let mut db=app.db.lock().unwrap(); app.require_profile(&db,profile)?;
        kids::require_item(&db,&app.identity(),kind,id)?;
        let tx=db.transaction().map_err(db_error)?;
        let previous:Option<(f64,String)>=tx.query_row("SELECT duration,context FROM progress WHERE profile_id=?1 AND type=?2 AND id=?3",params![profile,kind,id],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(db_error)?;
        let duration=previous.as_ref().map(|v|v.0).filter(|v|*v>0.0).unwrap_or_else(||item["duration"].as_f64().unwrap_or(0.0));
        if !duration.is_finite() || !(0.0..=1e9).contains(&duration){return Err("Invalid duration".into());}
        let position=match action {
            "watched"=>duration,
            "unwatched"=>0.0,
            "position"=>item["position"].as_f64().ok_or("Invalid position")?,
            _=>return Err("Invalid progress action".into()),
        };
        if !position.is_finite() || !(0.0..=1e9).contains(&position) || (duration>0.0 && position>duration){return Err("Invalid position".into());}
        let mut merged=previous.as_ref().and_then(|v|serde_json::from_str::<Value>(&v.1).ok()).filter(|v|v.is_object()).unwrap_or(json!({}));
        merged.as_object_mut().unwrap().extend(context.as_object().unwrap().clone());
        merged["progress_corrected"]=json!(true);
        merged["watched_override"]=if action=="watched" {json!(true)}else{Value::Null};
        let mut identity=merged.clone(); identity["id"]=json!(id); identity["type"]=json!(kind);
        let title=continuation::title_id(&identity);
        // A correction made in the same second must become the series' newest activity.
        let updated=activity_time(&tx,profile)?;
        tx.execute("INSERT INTO progress(profile_id,id,type,name,poster,position,duration,updated_at,context,title_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT(profile_id,type,id) DO UPDATE SET name=excluded.name,poster=COALESCE(excluded.poster,progress.poster),position=excluded.position,duration=excluded.duration,updated_at=excluded.updated_at,context=excluded.context,title_id=excluded.title_id",params![profile,id,kind,name,item["poster"].as_str(),position,duration,updated,merged.to_string(),title]).map_err(db_error)?;
        tx.execute("DELETE FROM queue_hidden WHERE profile_id=?1 AND type=?2 AND title_id=?3",params![profile,kind,title]).map_err(db_error)?;
        tx.execute("DELETE FROM continuation_cache WHERE profile_id=?1 AND series_id=?2",params![profile,title]).map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        Ok(axum::Json(json!({"ok":true,"position":position,"duration":duration})))
    }).await
}

#[derive(Deserialize)]
pub struct SeriesHistory {
    series_id: String,
}
pub async fn series_history(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    Query(query): Query<SeriesHistory>,
) -> ApiResult {
    let app = app.with_lease(lease);
    blocking(move || {
        if query.series_id.is_empty() || query.series_id.len()>512 {return Err("Invalid series identity".into());}
        let db=app.db.lock().unwrap(); app.require_profile(&db,profile)?;
        kids::require_item(&db,&app.identity(),"series",&query.series_id)?;
        // Matches the existing metadata episode ceiling; never fetch another show's history.
        let items=db.prepare("SELECT id,position,duration,updated_at,context FROM progress WHERE profile_id=?1 AND type='series' AND title_id=?2 ORDER BY updated_at DESC,rowid DESC LIMIT 2000").map_err(db_error)?
            .query_map(params![profile,query.series_id],|r|{
                let mut item=json!({"id":r.get::<_,String>(0)?,"type":"series","position":r.get::<_,f64>(1)?,"duration":r.get::<_,f64>(2)?,"updated_at":r.get::<_,i64>(3)?});
                if let Ok(raw)=serde_json::from_str::<Value>(&r.get::<_,String>(4)?){item["watched"]=json!(watched(&item,&raw));if let Ok(context)=matching_context(&raw){item.as_object_mut().unwrap().extend(context.as_object().unwrap().clone());}}
                Ok(item)
            }).map_err(db_error)?.collect::<Result<Vec<_>,_>>().map_err(db_error)?;
        Ok(axum::Json(json!(items)))
    }).await
}

pub fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE INDEX IF NOT EXISTS favorites_paging ON favorites(profile_id,name COLLATE NOCASE,type,id); CREATE INDEX IF NOT EXISTS progress_paging ON progress(profile_id,updated_at DESC);")
}

/// Explicit completion does not turn an unknown runtime into a fabricated duration.
pub fn watched(item: &Value, context: &Value) -> bool {
    context["watched_override"].as_bool().unwrap_or_else(|| {
        let duration = item["duration"].as_f64().unwrap_or(0.0);
        duration > 0.0 && item["position"].as_f64().unwrap_or(0.0) / duration >= 0.95
    })
}

// Keep activity ordering stable when a correction and playback arrive in one second.
pub fn activity_time(db: &Connection, profile: i64) -> Result<i64, ApiError> {
    db.query_row(
        "SELECT MAX(?2,COALESCE(MAX(updated_at),0)+1) FROM progress WHERE profile_id=?1",
        params![profile, util::now()],
        |r| r.get(0),
    )
    .map_err(db_error)
}
