//! Typed personal favorites over the existing profile-owned library rows.
use super::*;
use viptv_engine::presentation::{Card, CatalogPage};

#[derive(Default, Deserialize)]
pub(crate) struct Page {
    #[serde(default)]
    offset: u32,
    limit: Option<u32>,
}
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
    // Legacy updates to these shared columns remain visible in typed reads.
    saved.title = item["name"].as_str().unwrap_or(&saved.title).to_owned();
    saved.poster = item["poster"].as_str().map(str::to_owned);
    saved.progress = None;
    saved.added_at_millis=item["added_at_millis"].as_u64();
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
pub(crate) async fn list(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    Query(page): Query<Page>,
) -> ApiResult {
    let app = app.with_lease(lease);
    blocking(move || {
        let db = app.db.lock().map_err(|_|ApiError::from("Database unavailable"))?;
        app.require_profile(&db, profile)?;
        let limit = page.limit.unwrap_or(40).clamp(1, 100);
        let allowed = kids::sql_allowed("favorites.type", "favorites.id", "?1");
        let total: u64 = db.query_row(&format!("SELECT count(*) FROM favorites WHERE profile_id=?1 AND type IN ('movie','series') AND {allowed}"), [profile], |r|r.get(0)).map_err(db_error)?;
        let mut query = db.prepare(&format!("SELECT id,type,name,poster,context,added_at_millis FROM favorites WHERE profile_id=?1 AND type IN ('movie','series') AND {allowed} ORDER BY added_at_millis DESC,name COLLATE NOCASE,type,id LIMIT ?2 OFFSET ?3")).map_err(db_error)?;
        let items = query.query_map(params![profile,limit,page.offset], |r|Ok(json!({"id":r.get::<_,String>(0)?,"type":r.get::<_,String>(1)?,"name":r.get::<_,String>(2)?,"poster":r.get::<_,Option<String>>(3)?,"added_at_millis":r.get::<_,Option<u64>>(5)?,"engine_favorite":serde_json::from_str::<Value>(&r.get::<_,String>(4)?).unwrap_or(Value::Null)}))).map_err(db_error)?.collect::<Result<Vec<_>,_>>().map_err(db_error)?;
        let cards = cards(&db, &app.identity(), items)?;
        let next = u64::from(page.offset) + u64::from(limit);
        let page = CatalogPage {cards, next_cursor:(next < total).then(||next.to_string()), complete:next >= total};
        Ok(axum::Json(serde_json::to_value(page).map_err(|_|ApiError::from("Library unavailable"))?))
    }).await
}
pub(crate) async fn save(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    axum::Json(input): axum::Json<Save>,
) -> ApiResult {
    let app = app.with_lease(lease);
    blocking(move || {
        validate(&input.card)?;
        let db = app.db.lock().map_err(|_|ApiError::from("Database unavailable"))?;
        app.require_profile(&db, profile)?;
        kids::require_item(&db, &app.identity(), &input.card.identity.kind, &input.card.identity.id)?;
        let mut card = input.card;
        card.progress = None;
        card.added_at_millis=None;
        if kids::restricted(&db, &app.identity())? {
            // Restricted profiles persist only authoritative approved metadata.
            let item = json!({"type":card.identity.kind,"id":card.identity.id});
            let approved = kids::filter_items(&db, &app.identity(), vec![item])?;
            card = approved.first().and_then(|item| projection::card(item, "library")).ok_or_else(auth::forbidden)?;
        }
        db.execute("INSERT INTO favorites(profile_id,id,type,name,poster,context,added_at_millis) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(profile_id,type,id) DO UPDATE SET name=excluded.name,poster=excluded.poster,context=excluded.context",params![profile,card.identity.id,card.identity.kind,card.title,card.poster,serde_json::to_string(&card).map_err(|_|ApiError::from("Invalid library card"))?,util::now_millis()]).map_err(db_error)?;
        Ok(axum::Json(json!({"ok":true,"saved":true})))
    }).await
}
pub(crate) async fn remove(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path((profile, kind, id)): Path<(i64, String, String)>,
) -> ApiResult {
    let app = app.with_lease(lease);
    blocking(move || {
        if !["movie", "series"].contains(&kind.as_str())
            || id.is_empty()
            || id.len() > 512
            || id.chars().any(char::is_control)
        {
            return Err("Invalid library identity".into());
        }
        let db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        app.require_profile(&db, profile)?;
        kids::require_item(&db, &app.identity(), &kind, &id)?;
        db.execute(
            "DELETE FROM favorites WHERE profile_id=?1 AND type=?2 AND id=?3",
            params![profile, kind, id],
        )
        .map_err(db_error)?;
        Ok(axum::Json(json!({"ok":true,"saved":false})))
    })
    .await
}
