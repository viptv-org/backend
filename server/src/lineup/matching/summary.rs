use super::*;

pub(super) fn summary(
    db: &Connection,
    channel: Option<&str>,
    offset: usize,
) -> Result<Value, ApiError> {
    let rules = policy(db)?;
    let mut q=db.prepare("SELECT data,status FROM family_match_results WHERE (?1 IS NULL OR channel_id=?1) ORDER BY CASE status WHEN 'review' THEN 0 WHEN 'active' THEN 1 WHEN 'reserve' THEN 2 ELSE 3 END,live_id LIMIT 100 OFFSET ?2").map_err(db_error)?;
    let rows = q
        .query_map(params![channel, offset.min(500_000)], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(db_error)?
        .map(|row| {
            let (data, status) = row.map_err(db_error)?;
            let mut v: Value = serde_json::from_str(&data)
                .map_err(|_| ApiError::from("Matching result unavailable"))?;
            v["status"] = json!(status);
            Ok(v)
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    let mut counts = Vec::new();
    for c in super::channels(db).map_err(db_error)? {
        let id = c["id"].as_str().unwrap();
        let active: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM family_candidates WHERE channel_id=?1",
                [id],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        let reserves:i64=db.query_row("SELECT COUNT(*) FROM family_match_results WHERE channel_id=?1 AND status='reserve'",[id],|r|r.get(0)).map_err(db_error)?;
        let review: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM family_match_results WHERE channel_id=?1 AND status='review'",
                [id],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        counts.push(json!({"id":id,"name":c["name"],"active":active,"reserves":reserves,"review":review,"shortage":rules.active_candidates.saturating_sub(active as usize)}));
    }
    let mut q = db
        .prepare("SELECT alias FROM family_match_aliases WHERE channel_id=?1 ORDER BY alias")
        .map_err(db_error)?;
    let aliases = q
        .query_map([channel], |r| r.get::<_, String>(0))
        .map_err(db_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error)?;
    let mut q=db.prepare("SELECT p.id,p.name,COALESCE(g.upstream_group,'') FROM providers p LEFT JOIN family_provider_groups g ON p.id=g.provider_id ORDER BY p.id").map_err(db_error)?;
    let providers=q.query_map([],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"name":r.get::<_,String>(1)?,"upstream_group":r.get::<_,String>(2)?}))).map_err(db_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
    Ok(
        json!({"settings":rules,"channels":counts,"matches":rows,"aliases":aliases,"providers":providers}),
    )
}
#[derive(Deserialize)]
pub(crate) struct MatchQuery {
    channel_id: Option<String>,
    #[serde(default)]
    offset: usize,
}
pub(crate) async fn list(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Query(q): Query<MatchQuery>,
) -> ApiResult {
    blocking(move || {
        let db = a.db.lock().unwrap();
        owner(&lease, &db)?;
        summary(&db, q.channel_id.as_deref(), q.offset).map(axum::Json)
    })
    .await
}
pub(crate) async fn run(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
) -> ApiResult {
    blocking(move || {
        let db = a.db.lock().unwrap();
        owner(&lease, &db)?;
        drop(db);
        match_owned(&a, &lease).map(axum::Json)
    })
    .await
}
pub(crate) async fn configure(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    axum::Json(v): axum::Json<Value>,
) -> ApiResult {
    let rules: Policy =
        serde_json::from_value(v).map_err(|_| ApiError::from("Invalid matching settings"))?;
    if !(80..=100).contains(&rules.confidence)
        || rules.ambiguity_margin > 30
        || !(1..=20).contains(&rules.active_candidates)
        || !matches!(rules.ambiguity_policy.as_str(), "review" | "reject")
    {
        return Err("Matching requires confidence 80–100, margin 0–30, candidates 1–20 and review/reject ambiguity policy".into());
    }
    blocking(move || {
        let db = a.db.lock().unwrap();
        owner(&lease, &db)?;
        db.execute(
            "UPDATE family_matching_settings SET data=?1 WHERE id=1",
            [serde_json::to_string(&rules).unwrap()],
        )
        .map_err(db_error)?;
        summary(&db, None, 0).map(axum::Json)
    })
    .await
}
pub(crate) async fn group(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<i64>,
    axum::Json(v): axum::Json<Value>,
) -> ApiResult {
    let group = v["upstream_group"]
        .as_str()
        .filter(|s| s.len() <= 64)
        .ok_or("Upstream group must be at most 64 characters")?
        .trim()
        .to_owned();
    blocking(move||{let db=a.db.lock().unwrap();owner(&lease,&db)?;let exists:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM providers WHERE id=?1)",[id],|r|r.get(0)).map_err(db_error)?;if !exists{return Err("Provider not found".into());}db.execute("INSERT INTO family_provider_groups(provider_id,upstream_group) VALUES(?1,?2) ON CONFLICT(provider_id) DO UPDATE SET upstream_group=excluded.upstream_group",params![id,group]).map_err(db_error)?;Ok(axum::Json(json!({"saved":true})))}).await
}
pub(crate) async fn correct(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(channel): Path<String>,
    axum::Json(v): axum::Json<Value>,
) -> ApiResult {
    blocking(move||{
        let mut db=a.db.lock().unwrap();owner(&lease,&db)?;
        let current=super::channels(&db).map_err(db_error)?.into_iter().find(|c|c["id"]==channel).ok_or("Channel not found")?;
        let tx=db.transaction().map_err(db_error)?;
        if let Some(values)=v.get("aliases") {
            let aliases=values.as_array().filter(|v|v.len()<=20).ok_or("Use at most 20 literal aliases")?;
            let names=aliases.iter().map(|v|v.as_str().filter(|s|!s.trim().is_empty()&&s.len()<=128).map(normalized).ok_or_else(||ApiError::from("Aliases must be nonempty names up to 128 characters"))).collect::<Result<Vec<_>,_>>()?;
            tx.execute("DELETE FROM family_match_aliases WHERE channel_id=?1",[&channel]).map_err(db_error)?;
            for name in names {tx.execute("INSERT OR IGNORE INTO family_match_aliases(channel_id,alias) VALUES(?1,?2)",params![channel,name]).map_err(db_error)?;}
        }
        if let Some(id)=v.get("candidate_id") {
            let id=id.as_str().filter(|s|s.len()<=128).ok_or("Candidate ID required")?;
            let decision=v["decision"].as_str().filter(|s|matches!(*s,"pin"|"reject"|"clear")).ok_or("Choose pin, reject or clear")?;
            let name:Option<String>=tx.query_row("SELECT name FROM provider_live WHERE id=?1",[id],|r|r.get(0)).optional().map_err(db_error)?;
            if decision=="pin" {
                if v["verified"]!=true||name.as_deref()!=v["observed_name"].as_str()||name.is_none(){return Err("Reload and verify the current US English feed before pinning".into());}
                let name=name.as_deref().unwrap();
                let input=Input{id:id.into(),provider:0,name:name.into(),category:tx.query_row("SELECT COALESCE(category,'') FROM provider_live WHERE id=?1",[id],|r|r.get(0)).map_err(db_error)?,epg:String::new(),source:String::new(),pool:0,group:String::new()};
                if evidence(&current,&input,&[],false,true).is_none_or(|e|!e.safe){return Err("Candidate conflicts with this channel's country, language or feed".into());}
                let legacy:Option<String>=tx.query_row("SELECT channel_id FROM family_aliases WHERE live_id=?1",[id],|r|r.get(0)).optional().map_err(db_error)?;
                if legacy.is_some_and(|c|c!=channel){return Err("Candidate already belongs to another channel".into());}
                manual_saved(&tx,&channel,None,&[json!({"id":id,"name":name})]).map_err(db_error)?;
            }else if decision=="reject" {
                tx.execute("INSERT INTO family_match_overrides(channel_id,live_id,decision,observed_name) VALUES(?1,?2,'reject',?3) ON CONFLICT(channel_id,live_id) DO UPDATE SET decision='reject'",params![channel,id,name.unwrap_or_default()]).map_err(db_error)?;
                tx.execute("DELETE FROM family_candidates WHERE channel_id=?1 AND live_id=?2",params![channel,id]).map_err(db_error)?;
            }else{tx.execute("DELETE FROM family_match_overrides WHERE channel_id=?1 AND live_id=?2",params![channel,id]).map_err(db_error)?;}
        }
        tx.commit().map_err(db_error)?;
        drop(db);
        match_owned(&a,&lease)?;
        let db=a.db.lock().unwrap();
        summary(&db,Some(&channel),0).map(axum::Json)
    }).await
}
