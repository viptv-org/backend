use super::*;
use viptv_engine::{identity::MediaIdentity, playback::PlaybackRecord};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Progress {
    record: PlaybackRecord,
    parent_identity: Option<MediaIdentity>,
    #[serde(default)]
    ended: bool,
}

/// Typed history write; client timestamps never become server activity/completion authority.
pub(crate) async fn progress(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    axum::Json(q): axum::Json<Progress>,
) -> ApiResult {
    if q.record.position_millis > 1_000_000_000_000 || q.record.duration_millis > 1_000_000_000_000
    {
        return Err("Invalid playback position".into());
    }
    let mut value = lifecycle::identity_value(&q.record.identity, q.parent_identity.as_ref())?;
    value["name"] = json!(q.record.title);
    value["poster"] = json!(q.record.poster);
    value["position"] = json!(
        if q.ended && q.record.duration_millis > 0 {
            q.record.duration_millis
        } else {
            q.record.position_millis
        } as f64
            / 1000.0
    );
    value["duration"] = json!(q.record.duration_millis as f64 / 1000.0);
    let app = app.with_lease(lease);
    if q.record.identity.id.starts_with("simkl:") {
        let target = tracking::Watched::canonical(&value)?;
        let credential = store::credential(&app, profile, Capability::Metadata)?;
        tracking::verify(&app, profile, &target, &credential).await?;
    }
    // Legacy persistence preserves existing lists, completion policy and history corrections.
    crate::handlers_profiles::save_progress(State(app), Path(profile), axum::Json(value)).await
}
