use super::*;

pub(crate) async fn status(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
) -> ApiResult {
    let app = app.with_lease(lease.clone());
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_profile(&db, profile)?;
    let configured = app.simkl.configured() && app.secret_vault.is_some();
    let links = if configured {
        connections(
            &db,
            lease.principal.account_id().ok_or_else(auth::forbidden)?,
            profile,
        )?
    } else {
        vec![]
    };
    Ok(axum::Json(
        json!({"configured":configured,"linked":links.first().is_some_and(|c|c.revision > 0),"connection":links.first()}),
    ))
}

pub(crate) async fn authorize(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
) -> ApiResult {
    let app = app.with_lease(lease.clone());
    require_edit(&app, &lease, profile)?;
    let config = app.simkl.config.as_ref().ok_or_else(unavailable)?;
    if app.secret_vault.is_none() {
        return Err(unavailable());
    }
    let account = lease.principal.account_id().ok_or_else(auth::forbidden)?;
    let auth::Principal::Account { role, .. } = &lease.principal;
    let state = auth::token();
    let mut db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    lease.validate(&db)?;
    let tx = db.transaction().map_err(db_error)?;
    tx.execute(
        "DELETE FROM simkl_oauth_states WHERE expires<=?1 OR profile_id=?2",
        params![util::now(), profile],
    )
    .map_err(db_error)?;
    tx.execute("INSERT INTO simkl_links(profile_id,account_id,revision) VALUES(?1,?2,1) ON CONFLICT(profile_id) DO UPDATE SET revision=revision+1 WHERE account_id=?2", params![profile,account]).map_err(db_error)?;
    let revision: u64 = tx
        .query_row(
            "SELECT revision FROM simkl_links WHERE profile_id=?1 AND account_id=?2",
            params![profile, account],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    tx.execute(
        "INSERT INTO simkl_oauth_states VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            auth::hash(&state),
            profile,
            account,
            lease.session_id,
            role,
            lease.policy_revision,
            revision,
            util::now() + 600
        ],
    )
    .map_err(db_error)?;
    tx.commit().map_err(db_error)?;
    let mut url =
        url::Url::parse("https://simkl.com/oauth/authorize").map_err(|_| unavailable())?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &config.client_id)
        .append_pair("redirect_uri", &config.redirect)
        .append_pair("state", &state);
    Ok(axum::Json(
        json!({"authorization_url":url.as_str(),"expires_in_seconds":600}),
    ))
}

#[derive(Deserialize)]
pub(crate) struct Callback {
    pub(super) state: String,
    pub(super) code: Option<String>,
    pub(super) error: Option<String>,
}

pub(crate) async fn callback(
    State(app): State<App>,
    Query(query): Query<Callback>,
) -> Result<Response, ApiError> {
    if query.state.is_empty() || query.state.len() > 256 {
        return Err(auth::forbidden());
    }
    let config = app.simkl.config.as_ref().ok_or_else(unavailable)?;
    let (lease, profile, revision) = {
        let mut db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        let tx = db.transaction().map_err(db_error)?;
        let found = tx.query_row("SELECT profile_id,account_id,session_id,role,policy_revision,link_revision FROM simkl_oauth_states WHERE state_hash=?1 AND expires>?2", params![auth::hash(&query.state),util::now()], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,i64>(4)?,r.get::<_,u64>(5)?))).optional().map_err(db_error)?.ok_or_else(auth::forbidden)?;
        tx.execute(
            "DELETE FROM simkl_oauth_states WHERE state_hash=?1",
            [auth::hash(&query.state)],
        )
        .map_err(db_error)?;
        let lease = ResourceLease {
            policy_revision: found.4,
            session_id: Some(found.2.clone()),
            principal: auth::Principal::Account {
                account_id: found.1,
                role: found.3,
                profile_id: Some(found.0),
                session_id: Some(found.2),
            },
        };
        tx.commit().map_err(db_error)?;
        lease.validate(&db)?;
        (lease, found.0, found.5)
    };
    if query.error.is_some() {
        return Ok(axum::response::Redirect::to("/?simkl=cancelled").into_response());
    }
    let code = query
        .code
        .filter(|code| {
            !code.is_empty() && code.len() <= 1024 && !code.chars().any(char::is_control)
        })
        .ok_or_else(auth::forbidden)?;
    let scoped = app.clone().with_lease(lease.clone());
    let response = app.simkl.request_checked("/oauth/token", None, Some(json!({"code":code,"client_id":config.client_id,"client_secret":config.secret.as_str(),"redirect_uri":config.redirect,"grant_type":"authorization_code"})), || {
        require_edit(&scoped, &lease, profile)?;
        let db = app.db.lock().map_err(|_| ApiError::from("Database unavailable"))?;
        lease.validate(&db)?;
        let current: Option<u64> = db.query_row("SELECT revision FROM simkl_links WHERE profile_id=?1 AND account_id=?2", params![profile,lease.principal.account_id()], |r| r.get(0)).optional().map_err(db_error)?;
        if current != Some(revision) {
            return Err(ApiError(StatusCode::CONFLICT, "Simkl link changed. Start again.".into()));
        }
        Ok(())
    }).await?;
    let token = response["access_token"]
        .as_str()
        .filter(|token| {
            !token.is_empty() && token.len() <= 2048 && !token.chars().any(char::is_control)
        })
        .ok_or_else(|| ApiError::from("Simkl authorization response was invalid"))?;
    store::save_token(&app, &lease, profile, revision, token)?;
    Ok((
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::REFERRER_POLICY, "no-referrer"),
        ],
        axum::response::Redirect::to("/?simkl=linked"),
    )
        .into_response())
}

pub(crate) async fn disconnect(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
) -> ApiResult {
    let app = app.with_lease(lease.clone());
    require_edit(&app, &lease, profile)?;
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    lease.validate(&db)?;
    db.execute("UPDATE simkl_links SET credential=NULL,revision=revision+1 WHERE profile_id=?1 AND account_id=?2", params![profile,lease.principal.account_id()]).map_err(db_error)?;
    db.execute(
        "DELETE FROM simkl_oauth_states WHERE profile_id=?1 AND account_id=?2",
        params![profile, lease.principal.account_id()],
    )
    .map_err(db_error)?;
    Ok(axum::Json(json!({"linked":false})))
}
