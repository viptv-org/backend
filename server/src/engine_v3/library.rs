use super::*;
use viptv_engine::presentation::Card;

pub(crate) type Page = viptv_engine::library::LibraryQuery;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Save {
    card: Card,
}

pub(super) fn card(item: &Value) -> Option<Card> {
    let projected = projection::card(item, "library")?;
    let mut saved = serde_json::from_value::<Card>(item["engine_favorite"].clone())
        .ok()
        .filter(|c| {
            c.identity.id == projected.identity.id && c.identity.kind == projected.identity.kind
        })
        .unwrap_or(projected);
    saved.metadata_origin = None;
    saved.title = item["name"].as_str().unwrap_or(&saved.title).to_owned();
    saved.poster = item["poster"].as_str().map(str::to_owned);
    saved.progress = None;
    saved.added_at_millis = item["added_at_millis"].as_u64();
    Some(saved)
}
pub(super) fn cards(
    db: &Connection,
    principal: &auth::Principal,
    items: Vec<Value>,
) -> Result<Vec<Card>, ApiError> {
    let mut items = kids::filter_items(db, principal, items)?;
    if kids::restricted(db, principal)? {
        for item in &mut items {
            item["engine_favorite"] = Value::Null;
        }
    }
    Ok(items.iter().filter_map(card).collect())
}
fn validate(card: &Card) -> Result<(), ApiError> {
    card.validate_library()
        .map_err(|_| ApiError::from("Invalid library card"))
}
pub(crate) mod hydration;
#[cfg(test)]
pub(crate) mod hydration_tests;
mod listing;
pub(crate) mod membership;
pub(crate) use listing::list;

pub(crate) async fn save(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    axum::Json(input): axum::Json<Save>,
) -> ApiResult {
    let app = app.with_lease(lease);
    let mut input = input;
    super::watched::strip(&mut input.card);
    validate(&input.card)?;
    let evidence = if input.card.metadata_origin.is_some() {
        Some(origins::persistence_card(&app, &input.card).await?)
    } else {
        None
    };
    let canonical = if input.card.metadata_origin.is_none() {
        super::anime_show::canonical_show(&app, &input.card.identity).await
    } else {
        None
    };
    blocking(move || {
        if let Some(evidence) = evidence {
            let mut db=app.db.lock().map_err(|_|ApiError::from("Database unavailable"))?;
            app.require_profile(&db, profile)?;
            let tx=db.transaction().map_err(db_error)?;
            scoped_library::save_db(&tx,&app,profile,&evidence)?;
            let membership=membership::snapshot_db(&app,&app.request_lease(),profile,&tx)?;
            tx.commit().map_err(db_error)?;
            return Ok(axum::Json(json!({"ok":true,"saved":true,"library_membership":membership})));
        }
        let mut db = app.db.lock().map_err(|_|ApiError::from("Database unavailable"))?;
        app.require_profile(&db, profile)?;
        let mut card = input.card;
        if let Some(show) = canonical {
            card.identity = show;
            card.parent_identity = None;
        }
        kids::require_item(&db, &app.identity(), &card.identity.kind, &card.identity.id)?;
        card.identity.id=membership::resolve(&db,profile,&card.identity.kind,&card.identity.id)?;
        card.progress = None;
        card.added_at_millis=None;
        if kids::restricted(&db, &app.identity())? {
            let item = json!({"type":card.identity.kind,"id":card.identity.id});
            let approved = kids::filter_items(&db, &app.identity(), vec![item])?;
            card = approved.first().and_then(|item| projection::card(item, "library")).ok_or_else(auth::forbidden)?;
        }
        let tx = db.transaction().map_err(db_error)?;
        tx.execute("INSERT INTO favorites(profile_id,id,type,name,poster,context,added_at_millis) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(profile_id,type,id) DO UPDATE SET name=excluded.name,poster=excluded.poster,context=excluded.context",params![profile,card.identity.id,card.identity.kind,card.title,card.poster,serde_json::to_string(&card).map_err(|_|ApiError::from("Invalid library card"))?,util::now_millis()]).map_err(db_error)?;
        let membership = membership::snapshot_db(&app, &app.request_lease(), profile, &tx)?;
        tx.commit().map_err(db_error)?;
        Ok(axum::Json(json!({"ok":true,"saved":true,"library_membership":membership})))
    }).await
}
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct Removal {
    metadata_origin: Option<String>,
    card: Option<String>,
}
pub(crate) async fn remove(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path((profile, kind, id)): Path<(i64, String, String)>,
    Query(selector): Query<Removal>,
) -> ApiResult {
    let app = app.with_lease(lease);
    let canonical = if selector.metadata_origin.is_none() && selector.card.is_none() {
        super::anime_show::canonical_id(&app, &kind, &id).await
    } else {
        None
    };
    blocking(move || {
        if !["movie", "series"].contains(&kind.as_str())
            || id.is_empty()
            || id.len() > 512
            || id.chars().any(char::is_control)
        {
            return Err("Invalid library identity".into());
        }
        let mut db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        app.require_profile(&db, profile)?;
        if selector.metadata_origin.is_some() || selector.card.is_some() {
            let origin: viptv_engine::origin::MetadataOrigin = serde_json::from_str(
                selector
                    .metadata_origin
                    .as_deref()
                    .ok_or("Metadata origin is required")?,
            )
            .map_err(|_| ApiError::from("Invalid metadata origin"))?;
            let card: Card = serde_json::from_str(
                selector
                    .card
                    .as_deref()
                    .filter(|s| s.len() <= 65536)
                    .ok_or("Stored card is required")?,
            )
            .map_err(|_| ApiError::from("Invalid stored card"))?;
            if card.identity.kind != kind
                || card.identity.id != id
                || card.metadata_origin.as_ref() != Some(&origin)
            {
                return Err("Stored selector does not match the requested title".into());
            }
            let tx = db.transaction().map_err(db_error)?;
            scoped_library::remove_db(&tx, &app, profile, &card)?;
            let membership = membership::snapshot_db(&app, &app.request_lease(), profile, &tx)?;
            tx.commit().map_err(db_error)?;
            return Ok(axum::Json(
                json!({"ok":true,"saved":false,"library_membership":membership}),
            ));
        }
        kids::require_item(&db, &app.identity(), &kind, &id)?;
        let id = canonical.unwrap_or(id);
        let id = membership::resolve(&db, profile, &kind, &id)?;
        let tx = db.transaction().map_err(db_error)?;
        tx.execute(
            "DELETE FROM favorites WHERE profile_id=?1 AND type=?2 AND id=?3",
            params![profile, kind, id],
        )
        .map_err(db_error)?;
        let membership = membership::snapshot_db(&app, &app.request_lease(), profile, &tx)?;
        tx.commit().map_err(db_error)?;
        Ok(axum::Json(
            json!({"ok":true,"saved":false,"library_membership":membership}),
        ))
    })
    .await
}

#[cfg(test)]
mod receipt_tests;
