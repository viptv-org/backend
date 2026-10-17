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
    if q.parent_identity
        .as_ref()
        .zip(q.record.parent_identity.as_ref())
        .is_some_and(|(a, b)| a != b)
    {
        return Err("Conflicting parent title identities".into());
    }
    let parent = q
        .parent_identity
        .as_ref()
        .or(q.record.parent_identity.as_ref());
    let mut value = lifecycle::identity_value(&q.record.identity, parent)?;
    value["name"] = json!(q.record.title);
    value["poster"] = json!(q.record.poster);
    value["background"] = json!(q.record.landscape);
    value["engine_identity"] = json!(q.record.identity);
    value["engine_parent_identity"] = json!(parent);
    value["engine_provider_id"] = json!(q.record.provider_id);
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
        let card = tracking::verify(&app, profile, &target, &credential).await?;
        value["name"] = json!(card.title);
        if card.poster.is_some() {
            value["poster"] = json!(card.poster);
        }
        if card.landscape.is_some() {
            value["background"] = json!(card.landscape);
        }
        value["engine_identity"] = json!(card.identity);
        value["engine_parent_identity"] = json!(card.parent_identity);
        value["engine_provider_id"] = json!(card.provider_id);
    }
    if q.record.metadata_origin.is_none()
        && q.record
            .presentation
            .as_ref()
            .is_some_and(|card| card.metadata_origin.is_some())
    {
        return Err("Conflicting playback metadata origin".into());
    }
    if q.record.metadata_origin.is_some() {
        {
            let db = app
                .db
                .lock()
                .map_err(|_| ApiError::from("Database unavailable"))?;
            app.require_profile(&db, profile)?;
        }
        let expected = q.record.card();
        let evidence = crate::engine_v3::origins::persistence_card(&app, &expected).await?;
        let tracking = matches!(&evidence,crate::engine_v3::origins::MetadataEvidence::Verified(card,_) if card.metadata_origin.as_ref().is_some_and(|origin|origin.provider_id=="simkl"));
        {
            let mut db = app
                .db
                .lock()
                .map_err(|_| ApiError::from("Database unavailable"))?;
            let tx = db.transaction().map_err(db_error)?;
            crate::engine_v3::scoped_library::record_db(
                &tx, &app, profile, &q.record, &evidence, q.ended,
            )?;
            tx.commit().map_err(db_error)?;
        }
        if tracking {
            schedule_progress(app.clone(), profile, value, false);
        }
        return Ok(axum::Json(json!({"ok":true})));
    }
    // Legacy persistence preserves existing lists, completion policy and history corrections.
    crate::handlers_profiles::save_progress(State(app), Path(profile), axum::Json(value)).await
}
