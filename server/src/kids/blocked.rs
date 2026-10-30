use super::*;
use viptv_engine::{origin::verified_storage_identity, presentation::Card};

pub(crate) fn blocked(
    db: &Connection,
    p: &auth::Principal,
    kind: &str,
    id: &str,
) -> Result<bool, ApiError> {
    let auth::Principal::Account {
        profile_id: Some(profile),
        ..
    } = p
    else {
        return Ok(false);
    };
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM kids_blocked WHERE profile_id=?1 AND kind=?2 AND id=?3)",
        params![profile, kind, id],
        |r| r.get(0),
    )
    .map_err(db_error)
}
pub(crate) fn card_blocked(
    db: &Connection,
    p: &auth::Principal,
    card: &Card,
) -> Result<bool, ApiError> {
    if blocked(db, p, &card.identity.kind, &card.identity.id)? {
        return Ok(true);
    }
    if let Some(parent) = &card.parent_identity {
        if blocked(db, p, &parent.kind, &parent.id)? {
            return Ok(true);
        }
    }
    let canonical = verified_storage_identity(card);
    blocked(db, p, &canonical.kind, &canonical.id)
}
fn text(value: Option<&String>, limit: usize) -> Option<String> {
    value
        .map(|value| {
            value
                .chars()
                .filter(|c| !c.is_control())
                .take(limit)
                .collect::<String>()
        })
        .filter(|value| !value.is_empty())
}

#[derive(Deserialize, Default)]
pub(crate) struct BlockPage {
    #[serde(default)]
    offset: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BlockRequest {
    #[serde(rename = "type")]
    kind: String,
    id: String,
    blocked: bool,
    name: Option<String>,
    poster: Option<String>,
}
pub(crate) async fn list(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    Query(page): Query<BlockPage>,
) -> ApiResult {
    let app = app.with_lease(lease);
    blocking(move || {
        let db = app.db.lock().unwrap();
        app.request_lease().validate(&db)?;
        manager(&db, &app.identity(), Some(profile))?;
        let mut statement = db
            .prepare("SELECT id,kind,name,poster FROM kids_blocked WHERE profile_id=?1 ORDER BY blocked_at,id LIMIT 500 OFFSET ?2")
            .map_err(db_error)?;
        let items = statement
            .query_map(params![profile, page.offset.min(i64::MAX as usize)], |r| {
                Ok(json!({"id":r.get::<_,String>(0)?,"type":r.get::<_,String>(1)?,"name":r.get::<_,Option<String>>(2)?,"poster":r.get::<_,Option<String>>(3)?}))
            })
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        Ok(axum::Json(json!({"items":items})))
    })
    .await
}
pub(crate) async fn set(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    axum::Json(request): axum::Json<BlockRequest>,
) -> ApiResult {
    if !matches!(request.kind.as_str(), "movie" | "series") {
        return Err("Block movies or series only".into());
    }
    if request.id.is_empty()
        || request.id.len() > 512
        || request.id.chars().any(char::is_control)
    {
        return Err("Invalid blocked title".into());
    }
    let app = app.with_lease(lease);
    let name = text(request.name.as_ref(), 512);
    let poster = text(request.poster.as_ref(), 1024);
    blocking(move || {
        let mut db = app.db.lock().unwrap();
        let tx = db.transaction().map_err(db_error)?;
        app.request_lease().validate(&tx)?;
        manager(&tx, &app.identity(), Some(profile))?;
        if request.blocked {
            let count: i64 = tx
                .query_row(
                    "SELECT count(*) FROM kids_blocked WHERE profile_id=?1",
                    [profile],
                    |r| r.get(0),
                )
                .map_err(db_error)?;
            let exists: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM kids_blocked WHERE profile_id=?1 AND kind=?2 AND id=?3)",
                    params![profile, request.kind, request.id],
                    |r| r.get(0),
                )
                .map_err(db_error)?;
            if count >= 500 && !exists {
                return Err(
                    "Up to 500 blocked titles are supported. Unblock a title before adding another"
                        .into(),
                );
            }
            tx.execute("INSERT INTO kids_blocked(profile_id,kind,id,name,poster,blocked_at) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(profile_id,kind,id) DO UPDATE SET name=excluded.name,poster=excluded.poster",params![profile,request.kind,request.id,name,poster,util::now()]).map_err(db_error)?;
        } else {
            tx.execute(
                "DELETE FROM kids_blocked WHERE profile_id=?1 AND kind=?2 AND id=?3",
                params![profile, request.kind, request.id],
            )
            .map_err(db_error)?;
        }
        tx.execute("INSERT INTO kids_profiles(profile_id,revision) VALUES(?1,1) ON CONFLICT(profile_id) DO UPDATE SET revision=revision+1",[profile]).map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        Ok(axum::Json(json!({"blocked":request.blocked})))
    })
    .await
}
