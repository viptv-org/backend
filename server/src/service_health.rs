//! A bounded, saved-state overview. Opening it never contacts a provider.
use super::*;

#[derive(Default, Deserialize)]
pub(crate) struct Page {
    #[serde(default)]
    offset: u32,
}

pub(crate) async fn list(
    State(a): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Query(page): Query<Page>,
) -> ApiResult {
    let active = a.playback.active_count().await;
    let providers = a.providers.clone();
    providers
        .blocking(move |store| {
            let mut result = store.service_health(&lease, page.offset)?;
            result["active_sessions"] = json!(active);
            Ok(result)
        })
        .await
        .map(axum::Json)
        .map_err(ApiError::from)
}

pub(crate) fn saved(db: &Connection) -> Result<Value, String> {
    let now = util::now();
    let guides = db.query_row(
        "SELECT count(*),COALESCE(sum(EXISTS(SELECT 1 FROM family_programmes f
         JOIN guide_sources s ON s.id=f.source_id AND s.enabled=1
         JOIN guide_mappings m ON m.channel_id=f.channel_id AND m.source_id=f.source_id AND m.guide_id=f.guide_id
         JOIN guide_channels g ON g.source_id=m.source_id AND g.guide_id=m.guide_id AND g.name=m.observed_name
         WHERE f.channel_id=c.id AND f.start<=?1 AND f.end>?1)),0)
         FROM family_channels c WHERE json_extract(c.data,'$.enabled')=1",
        [now], |r| Ok(json!({"enabled_channels":r.get::<_,i64>(0)?,"current_channels":r.get::<_,i64>(1)?})),
    ).map_err(|_| "Service health unavailable".to_owned())?;
    let mut guides = guides;
    let (enabled, failed, updated): (i64, i64, Option<i64>) = db.query_row(
        "SELECT count(*),COALESCE(sum(reason IS NOT NULL),0),max(updated_at) FROM guide_sources WHERE enabled=1",
        [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    ).map_err(|_| "Service health unavailable".to_owned())?;
    guides["enabled_sources"] = json!(enabled);
    guides["failed_sources"] = json!(failed);
    guides["last_updated_at"] = json!(updated);
    guides["last_run"] = db.query_row(
        "SELECT state,last_finish,reason FROM guide_runs WHERE id=1", [],
        |r| Ok(json!({"state":r.get::<_,String>(0)?,"last_finish":r.get::<_,Option<i64>>(1)?,"reason":r.get::<_,Option<String>>(2)?})),
    ).map_err(|_| "Service health unavailable".to_owned())?;
    Ok(
        json!({"at":now,"paused":activity::paused(db),"catalog":automation::summary(db)?,"guides":guides}),
    )
}
