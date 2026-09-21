use super::*;
use axum::{
    http::{HeaderMap, HeaderValue, Method},
    Json,
};

pub(crate) fn unauthorized() -> ApiError {
    ApiError(StatusCode::UNAUTHORIZED, "Unauthorized".into())
}
pub(crate) fn forbidden() -> ApiError {
    ApiError(StatusCode::FORBIDDEN, "Forbidden".into())
}
pub(crate) fn auth_error(error: ApiError) -> Response {
    let code = if error.1 == MSG_PARENT_REQUIRED {
        "parent_required"
    } else if error.1 == "Refresh token reuse detected" {
        "refresh_reuse"
    } else if error.1 == "authorization_pending" {
        "authorization_pending"
    } else {
        match error.0 {
            StatusCode::UNAUTHORIZED => "unauthorized",
            StatusCode::FORBIDDEN => "forbidden",
            StatusCode::CONFLICT => "conflict",
            StatusCode::TOO_MANY_REQUESTS => "rate_limited",
            StatusCode::BAD_REQUEST => "invalid_request",
            _ => "auth_error",
        }
    };
    let mut r = (error.0, Json(json!({"error":error.1,"error_code":code}))).into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}
pub(crate) fn constant_eq(a: &str, b: &str) -> bool {
    let mut d = a.len() ^ b.len();
    for (i, c) in b.bytes().enumerate() {
        d |= (a.as_bytes().get(i).copied().unwrap_or(0) ^ c) as usize;
    }
    d == 0
}
pub(crate) fn bearer(h: &HeaderMap) -> Option<&str> {
    h.get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}
pub(crate) fn cookie<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|v| v.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}
pub(crate) fn parse_origin(value: &str) -> Result<url::Url, ApiError> {
    let u = url::Url::parse(value).map_err(|_| forbidden())?;
    if u.scheme() != "https"
        || u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.path() != "/"
        || u.query().is_some()
        || u.fragment().is_some()
    {
        return Err(forbidden());
    }
    Ok(u)
}
fn configured_origins() -> Result<Vec<url::Url>, ApiError> {
    match std::env::var("VIPTV_AUTH_ORIGIN") {
        Ok(value) if value.trim().is_empty() => Ok(Vec::new()),
        Ok(value) => value
            .split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(parse_origin)
            .collect(),
        Err(std::env::VarError::NotPresent) => Ok(Vec::new()),
        Err(_) => Err(forbidden()),
    }
}
/// The canonical origin is the first configured entry: pairing QR codes and
/// verification URLs always send devices there. Additional entries are extra
/// accepted browser origins — for example a reverse-proxy hostname that
/// serves the same bundle behind the same backend.
pub(crate) fn canonical_origin() -> Result<Option<url::Url>, ApiError> {
    Ok(required_origins()?.into_iter().next())
}
pub(crate) fn required_origins() -> Result<Vec<url::Url>, ApiError> {
    let configured = configured_origins()?;
    // Production/release builds fail at startup and on QR generation without a
    // pinned HTTPS origin. Debug builds retain Host fallback only for isolated
    // in-memory integration tests and explicit local development.
    if configured.is_empty() && !cfg!(debug_assertions) {
        return Err(ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "VIPTV_AUTH_ORIGIN is required".into(),
        ));
    }
    Ok(configured)
}
pub(crate) fn origin(h: &HeaderMap) -> Result<(), ApiError> {
    check_origin(h, &required_origins()?)
}
pub(crate) fn check_origin(h: &HeaderMap, configured: &[url::Url]) -> Result<(), ApiError> {
    if h.get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == "cross-site")
    {
        return Err(forbidden());
    }
    if let Some(o) = h.get(header::ORIGIN) {
        let u = parse_origin(o.to_str().map_err(|_| forbidden())?)?;
        let allowed: Vec<url::Origin> = if !configured.is_empty() {
            configured.iter().map(|c| c.origin()).collect()
        } else {
            let host = h
                .get(header::HOST)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(forbidden)?;
            vec![parse_origin(&format!("https://{host}"))?.origin()]
        };
        if !allowed.contains(&u.origin()) {
            return Err(forbidden());
        }
    }
    Ok(())
}
pub(crate) fn csrf_for_session(
    db: &Connection,
    h: &HeaderMap,
    session_id: &str,
    primary: &str,
) -> Result<(), ApiError> {
    if !h.contains_key(header::ORIGIN) {
        return Err(forbidden());
    }
    origin(h)?;
    let supplied = h
        .get("x-csrf-token")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(forbidden)?;
    let supplied_hash = hash(supplied);
    if constant_eq(&supplied_hash, primary) {
        return Ok(());
    }
    let valid: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM auth_csrf_tokens WHERE session_id=?1 AND csrf_hash=?2 AND expires>?3)",
        params![session_id,supplied_hash,now()],
        |row| row.get(0),
    ).map_err(crate::db_error)?;
    if valid {
        Ok(())
    } else {
        Err(forbidden())
    }
}
pub(crate) fn issue_csrf(db: &Connection, session_id: &str) -> Result<String, ApiError> {
    db.execute("DELETE FROM auth_csrf_tokens WHERE expires<=?1", [now()])
        .map_err(crate::db_error)?;
    let raw = token();
    db.execute(
        "INSERT INTO auth_csrf_tokens(session_id,csrf_hash,expires,created_at) VALUES(?1,?2,?3,?4)",
        params![session_id, hash(&raw), now() + REFRESH, now()],
    )
    .map_err(crate::db_error)?;
    db.execute(
        "DELETE FROM auth_csrf_tokens WHERE rowid IN(SELECT rowid FROM auth_csrf_tokens WHERE session_id=?1 ORDER BY created_at DESC,rowid DESC LIMIT -1 OFFSET 16)",
        [session_id],
    ).map_err(crate::db_error)?;
    Ok(raw)
}
pub(crate) fn public(path: &str, method: &Method) -> bool {
    let canonical = if path.starts_with("/device/") {
        format!("/auth{path}")
    } else {
        path.to_owned()
    };
    let path = canonical.as_str();
    (method == Method::GET && matches!(path, "/auth/status" | "/auth/device/qr"))
        || (method == Method::POST
            && matches!(
                path,
                "/auth/register"
                    | "/auth/login"
                    | "/auth/refresh"
                    | "/auth/recover"
                    | "/auth/device/code"
                    | "/auth/device/token"
                    | "/auth/device/refresh"
            ))
}
pub async fn authenticate(State(app): State<App>, mut req: Request, next: Next) -> Response {
    let path = req
        .uri()
        .path()
        .strip_prefix("/api")
        .unwrap_or(req.uri().path());
    if public(path, req.method()) {
        return next.run(req).await;
    }
    let headers = req.headers().clone();
    let method = req.method().clone();
    let result = tokio::task::spawn_blocking(move || {
        let from_cookie = bearer(&headers).is_none();
        let t = bearer(&headers)
            .or_else(|| cookie(&headers, "viptv_session"))
            .ok_or_else(unauthorized)?;
        let db = app.db.lock().map_err(|_| unauthorized())?;
        let row=db.query_row("SELECT a.id,a.role,s.profile_id,s.csrf_hash,s.kind,s.id FROM auth_sessions s JOIN auth_accounts a ON a.id=s.account_id WHERE s.access_hash=?1 AND s.access_expires>?2 AND a.disabled=0",params![hash(t),now()],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<i64>>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?))).optional().map_err(crate::db_error)?.ok_or_else(unauthorized)?;
        if from_cookie && row.4 != "browser" {
            return Err(unauthorized());
        }
        if from_cookie && !matches!(method, Method::GET | Method::HEAD | Method::OPTIONS) {
            csrf_for_session(&db, &headers, &row.5, &row.3)?;
        }
        Ok(Principal::Account {
            account_id: row.0,
            role: if row.4 == "device" {
                "device".into()
            } else {
                row.1
            },
            profile_id: row.2,
            session_id:Some(row.5),
        })
    }).await.unwrap_or_else(|_|Err(unauthorized()));
    match result {
        Ok(p) => {
            req.extensions_mut().insert(p);
            next.run(req).await
        }
        Err(e) => auth_error(e),
    }
}
/// Use this variant when mounting outside an already authenticated API router.
pub fn router_with_auth(app: App) -> Router<App> {
    router().route_layer(axum::middleware::from_fn_with_state(app, authenticate))
}
pub fn router() -> Router<App> {
    let mut r = Router::new()
        .route("/auth/status", get(status))
        .route("/auth/device/qr", get(device_qr));
    for p in [
        "register",
        "login",
        "refresh",
        "recover",
        "logout",
        "profile",
        "device/code",
        "device/lookup",
        "device/approve",
        "device/token",
        "device/refresh",
        "device/revoke",
    ] {
        r = r.route(&format!("/auth/{p}"), post(action));
    }
    for p in ["me", "accounts", "devices", "events"] {
        r = r.route(&format!("/auth/{p}"), get(info));
    }
    for p in ["code", "token", "refresh", "lookup", "approve", "deny"] {
        r = r.route(&format!("/device/{p}"), post(action));
    }
    r.route("/auth/sessions", get(info).delete(remove))
        .route("/devices", get(info))
        .route("/devices/:id", axum::routing::delete(remove))
        .route("/admin/devices", get(info))
        .route("/admin/devices/:id", axum::routing::delete(remove))
        .route("/accounts", get(info))
        .route("/onboarding", get(info).patch(action))
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
        .layer(axum::middleware::from_fn(no_store))
}
async fn no_store(req: Request, next: Next) -> Response {
    let mut r = next.run(req).await;
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}
