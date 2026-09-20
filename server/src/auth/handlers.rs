use super::*;
use axum::{extract::OriginalUri, http::HeaderMap, Json};

pub(crate) async fn remove(
    state: State<App>,
    uri: OriginalUri,
    p: Option<Extension<Principal>>,
    h: HeaderMap,
) -> Response {
    action(state, uri, p, h, Json(json!({}))).await
}
pub(crate) async fn status(
    State(app): State<App>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    crate::blocking(move || {
        let db = app.db.lock().map_err(|_| unauthorized())?;
        let access_valid = cookie(&headers, "viptv_session").is_some_and(|access| {
            db.query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_sessions s JOIN auth_accounts a ON a.id=s.account_id WHERE s.access_hash=?1 AND s.access_expires>?2 AND s.kind='browser' AND a.disabled=0)",
                params![hash(access), now()],
                |r| r.get::<_, bool>(0),
            ).unwrap_or(false)
        });
        let refresh_valid = cookie(&headers, "viptv_refresh").is_some_and(|refresh| {
            db.query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_sessions s JOIN auth_accounts a ON a.id=s.account_id WHERE s.refresh_hash=?1 AND s.refresh_expires>?2 AND s.kind='browser' AND a.disabled=0)",
                params![hash(refresh), now()],
                |r| r.get::<_, bool>(0),
            ).unwrap_or(false)
        });
        let authenticated = access_valid || refresh_valid;
        let session_id = if access_valid {
            cookie(&headers, "viptv_session").and_then(|access| db.query_row(
                "SELECT id FROM auth_sessions WHERE access_hash=?1 AND kind='browser'",
                [hash(access)], |row| row.get::<_,String>(0),
            ).optional().ok().flatten())
        } else if refresh_valid {
            cookie(&headers, "viptv_refresh").and_then(|refresh| db.query_row(
                "SELECT id FROM auth_sessions WHERE refresh_hash=?1 AND kind='browser'",
                [hash(refresh)], |row| row.get::<_,String>(0),
            ).optional().ok().flatten())
        } else { None };
        let csrf_token = if let Some(session_id) = session_id {
            Some(issue_csrf(&db, &session_id)?)
        } else { None };
        Ok(Json(json!({
            "registration_enabled": true,
            "authenticated": authenticated,
            "csrf_token": csrf_token
        })))
    })
    .await
}

pub(crate) async fn info(
    State(app): State<App>,
    OriginalUri(uri): OriginalUri,
    Extension(p): Extension<Principal>,
) -> Result<Response, ApiError> {
    crate::blocking(move || {
    let db = app.db.lock().map_err(|_| unauthorized())?;
    let path = uri.path().strip_prefix("/api").unwrap_or(uri.path());
    let mut args = json!({});
    let canonical = canonical(path, &mut args)?;
    let path = canonical.as_str();
    if matches!(&p,Principal::Account{role,..} if role=="device") && path != "/auth/me" {
        return Err(forbidden());
    }
    if path != "/auth/me" {crate::kids::require_parent(&db,&p)?;}
    let v = match path {
        "/auth/setup" => {
            let id = p.account_id().ok_or_else(forbidden)?;
            db.query_row(
                "SELECT state FROM auth_onboarding WHERE account_id=?1",
                [id],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(crate::db_error)?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(json!({"step":"profile","completed":false}))
        }
        "/auth/me" => {
            let Principal::Account { account_id, role, profile_id, .. } = &p;
            let account = db.query_row(
                "SELECT username,name,role FROM auth_accounts WHERE id=?1",
                [account_id],
                |row| Ok(json!({"id":account_id.to_string(),"username":row.get::<_,String>(0)?,"name":row.get::<_,String>(1)?,"role":row.get::<_,String>(2)?})),
            ).map_err(crate::db_error)?;
            let profiles = list_profiles(&db, *account_id)?;
            let csrf_token = if role != "device" {
                p.session_id().map(|session_id| issue_csrf(&db, session_id)).transpose()?
            } else { None };
            let presentation_required = profiles.is_empty() || profiles.iter().any(|profile| profile["setup_complete"] != true);
            json!({
                "account_id":account_id.to_string(),
                "account":account,
                "user":account,
                "role":role,
                "profile_id":profile_id.map(|value| value.to_string()),
                "restricted":crate::kids::restricted(&db,&p)?,
                "profiles":profiles,
                "capabilities":{"create_profiles":true,"can_create_profile":true,"manage_profiles":true,"can_manage_profiles":true},
                "can_create_profile":true,
                "can_manage_profiles":true,
                "profile_setup_required":presentation_required,
                "csrf_token":csrf_token
            })
        }
        "/auth/accounts" => {
            p.require_owner()?;
            let mut s = db
                .prepare("SELECT id,username,name,role,disabled FROM auth_accounts ORDER BY id")
                .map_err(crate::db_error)?;
            let rows=s.query_map([],|r|Ok(json!({"id":r.get::<_,i64>(0)?.to_string(),"username":r.get::<_,String>(1)?,"name":r.get::<_,String>(2)?,"role":r.get::<_,String>(3)?,"disabled":r.get::<_,bool>(4)?,"enabled":!r.get::<_,bool>(4)?}))).map_err(crate::db_error)?.collect::<Result<Vec<_>,_>>().map_err(crate::db_error)?;
            json!(rows)
        }
        "/auth/devices" | "/auth/devices-own" | "/auth/admin/devices" | "/auth/sessions" => {
            let admin_devices = path == "/auth/admin/devices";
            if admin_devices {
                p.require_owner()?;
            }
            let kind = if path == "/auth/sessions" {
                "browser"
            } else {
                "device"
            };
            let mut statement=db.prepare("SELECT id,account_id,device_name,kind,profile_id,created_at FROM auth_sessions WHERE (?1 OR account_id=?2) AND refresh_expires>?3 AND kind=?4 ORDER BY created_at DESC LIMIT 1000").map_err(crate::db_error)?;
            let rows=statement.query_map(params![admin_devices,p.account_id(),now(),kind],|row|{
                let session_id=row.get::<_,String>(0)?;
                Ok(json!({"id":session_id,"session_id":session_id,"account_id":row.get::<_,i64>(1)?.to_string(),"device_name":row.get::<_,String>(2)?,"kind":row.get::<_,String>(3)?,"profile_id":row.get::<_,Option<i64>>(4)?.map(|value|value.to_string()),"created_at":row.get::<_,i64>(5)?.to_string()}))
            }).map_err(crate::db_error)?.collect::<Result<Vec<_>,_>>().map_err(crate::db_error)?;
            if path == "/auth/sessions" {
                json!({"sessions":rows})
            } else if admin_devices || path == "/auth/devices-own" {
                json!(rows)
            } else {
                json!({"devices":rows})
            }
        }
        "/auth/events" => {
            p.require_owner()?;
            let mut s = db
                .prepare(
                    "SELECT kind,account_id,created_at FROM auth_events ORDER BY id DESC LIMIT 100",
                )
                .map_err(crate::db_error)?;
            let rows=s.query_map([],|r|Ok(json!({"kind":r.get::<_,String>(0)?,"account_id":r.get::<_,Option<i64>>(1)?,"created_at":r.get::<_,i64>(2)?}))).map_err(crate::db_error)?.collect::<Result<Vec<_>,_>>().map_err(crate::db_error)?;
            json!({"events":rows})
        }
        _ => return Err(ApiError(StatusCode::NOT_FOUND, "Unknown endpoint".into())),
    };
    Ok(response(v, None))
    }).await
}
