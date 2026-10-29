use super::*;
mod mutations;
pub(crate) use mutations::{remove, remove_all};
#[cfg(test)]
mod tests;

pub(crate) async fn get(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
) -> Result<Response, account_api::Error> {
    let current = lease.session_id.clone().ok_or_else(auth::unauthorized)?;
    let snapshot = account_api::run(app, lease, move |db, account| {
        let identity = db.query_row(
            "SELECT username,name,role FROM auth_accounts WHERE id=?1",
            [account],
            |row| Ok(json!({"id":account.to_string(),"username":row.get::<_,String>(0)?,"name":row.get::<_,String>(1)?,"role":row.get::<_,String>(2)?})),
        ).map_err(db_error)?;
        let mut statement = db.prepare("SELECT id,device_name,kind,created_at FROM auth_sessions WHERE account_id=?1 AND refresh_expires>?2 AND kind IN ('browser','device') ORDER BY (id=?3) ASC,created_at DESC,id DESC")
            .map_err(db_error)?;
        let devices = statement.query_map(params![account,auth::now(),current], |row| {
            let id: String = row.get(0)?;
            let is_current = id == current;
            Ok(json!({"id":id,"name":row.get::<_,String>(1)?,"kind":row.get::<_,String>(2)?,"created_at":row.get::<_,i64>(3)?,"current":is_current,"can_sign_out":!is_current}))
        }).map_err(db_error)?.collect::<Result<Vec<_>,_>>().map_err(db_error)?;
        Ok(json!({"account":identity,"devices":devices,"can_sign_out_all":true}))
    }).await?;
    Ok(auth::response(snapshot, None))
}
