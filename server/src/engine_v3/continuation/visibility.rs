use super::*;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Visibility {
    identity: MediaIdentity,
    parent_identity: Option<MediaIdentity>,
    hidden: bool,
    metadata_origin: Option<viptv_engine::origin::MetadataOrigin>,
    card: Option<Card>,
}
pub(crate) async fn update(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    axum::Json(input): axum::Json<Visibility>,
) -> ApiResult {
    validate(&input.identity, input.parent_identity.as_ref())?;
    let app = app.with_lease(lease.clone());
    let mut db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_profile(&db, profile)?;
    lease.validate(&db)?;
    let configuration_revision = configuration_revision(&db, profile)?;
    if input.metadata_origin.is_some() || input.card.is_some() {
        let card = input.card.ok_or("Stored continuation card is required")?;
        let origin = input.metadata_origin.ok_or("Metadata origin is required")?;
        if card.metadata_origin.as_ref() != Some(&origin)
            || card.identity != input.identity
            || card.parent_identity != input.parent_identity
        {
            return Err("Continuation selector does not match the stored title".into());
        }
        let tx = db.transaction().map_err(db_error)?;
        let identity =
            super::super::scoped_library::visibility_db(&tx, &app, profile, &card, input.hidden)?;
        let revision =
            configuration_revision.saturating_add(super::super::history::revision(&tx, profile)?);
        tx.commit().map_err(db_error)?;
        return Ok(axum::Json(
            serde_json::to_value(QueueVisibilityReceipt {
                scope: super::super::scope(&lease, profile, configuration_revision),
                revision,
                identity,
                hidden: input.hidden,
            })
            .map_err(|_| ApiError::from("Visibility unavailable"))?,
        ));
    }
    kids::require_item(
        &db,
        &lease.principal,
        &input.identity.kind,
        &input.identity.id,
    )?;
    let context:Option<(String,String)>=db.query_row("SELECT title_id,context FROM progress WHERE profile_id=?1 AND type=?2 AND (id=?3 OR (?4=1 AND title_id=?3)) ORDER BY (id=?3) DESC,updated_at DESC,rowid DESC LIMIT 1",params![profile,input.identity.kind,input.identity.id,input.identity.episode.is_none()],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(db_error)?;
    let (id, parent) = if let Some((title, raw)) = context {
        let context: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
        let stored =
            serde_json::from_value::<MediaIdentity>(context["engine_parent_identity"].clone()).ok();
        if input.parent_identity.as_ref().is_some_and(|p| {
            p.id != title
                || stored
                    .as_ref()
                    .is_some_and(|known| known.id != p.id || known.kind != p.kind)
        }) {
            return Err("Continuation parent does not match saved history".into());
        }
        (
            if title.is_empty() {
                input.identity.id.clone()
            } else {
                title
            },
            stored,
        )
    } else {
        // A prepared next episode has no artificial history row. Its verified cache is proof.
        let mut query=db.prepare("SELECT series_id,result FROM continuation_cache WHERE profile_id=?1 AND updated_at>?2 ORDER BY updated_at DESC LIMIT 200").map_err(db_error)?;
        let rows = query
            .query_map(params![profile, util::now() - 3600], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        let known = rows
            .into_iter()
            .find_map(|(parent, raw)| {
                let value = serde_json::from_str::<Value>(&raw).ok()?;
                (value["status"] == "next"
                    && value["item"]["id"].as_str() == Some(&input.identity.id)
                    && value["item"]["type"].as_str() == Some(&input.identity.kind))
                .then_some((parent, value))
            })
            .ok_or_else(|| {
                ApiError(
                    StatusCode::NOT_FOUND,
                    "Continuation item is unavailable".into(),
                )
            })?;
        if input
            .parent_identity
            .as_ref()
            .is_some_and(|p| p.id != known.0)
        {
            return Err("Continuation parent does not match prepared target".into());
        }
        (
            known.0,
            serde_json::from_value::<MediaIdentity>(
                known.1["item"]["engine_parent_identity"].clone(),
            )
            .ok(),
        )
    };
    let mut identity = parent.unwrap_or_else(|| MediaIdentity {
        id: id.clone(),
        kind: input.identity.kind.clone(),
        ids: vec![],
        episode: None,
        edition: None,
    });
    identity.id = id;
    kids::require_item(&db, &lease.principal, &identity.kind, &identity.id)?;
    let tx = db.transaction().map_err(db_error)?;
    if input.hidden {
        crate::continuation::hide(&tx, profile, &identity.kind, &identity.id).map(|_| 0)
    } else {
        tx.execute(
            "DELETE FROM queue_hidden WHERE profile_id=?1 AND type=?2 AND title_id=?3",
            params![profile, identity.kind, identity.id],
        )
    }
    .map_err(db_error)?;
    let revision =
        configuration_revision.saturating_add(super::super::history::revision(&tx, profile)?);
    tx.commit().map_err(db_error)?;
    Ok(axum::Json(
        serde_json::to_value(QueueVisibilityReceipt {
            scope: super::super::scope(&lease, profile, configuration_revision),
            revision,
            identity,
            hidden: input.hidden,
        })
        .map_err(|_| ApiError::from("Visibility unavailable"))?,
    ))
}
