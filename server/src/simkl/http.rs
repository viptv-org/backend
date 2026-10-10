use super::*;

fn scope(app: App, lease: ResourceLease, p: i64) -> Result<App, ApiError> {
    let app = app.with_lease(lease);
    {
        let db = app.db.lock().unwrap();
        app.require_profile(&db, p)?;
    }
    Ok(app)
}
pub(crate) async fn status(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(p): Path<i64>,
) -> ApiResult {
    let app = scope(app, lease, p)?;
    let db = app.db.lock().unwrap();
    let row=db.query_row("SELECT user_id,user_name,last_sync,error,counts FROM simkl_connections WHERE profile_id=?1",[p],|r|Ok(json!({"connected":true,"user_id":r.get::<_,String>(0)?,"user_name":r.get::<_,String>(1)?,"last_sync":r.get::<_,i64>(2)?,"error":r.get::<_,Option<String>>(3)?,"counts":serde_json::from_str::<Value>(&r.get::<_,String>(4)?).unwrap_or(json!({}))}))).optional().map_err(db_error)?;
    Ok(axum::Json(row.unwrap_or(
        json!({"connected":false,"configured":app.simkl.client.is_some(),"full_search":false}),
    )))
}
pub(crate) async fn connect(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(p): Path<i64>,
) -> ApiResult {
    let app = scope(app, lease, p)?;
    let principal = app.identity();
    if matches!(&principal,auth::Principal::Account{role,..}if role=="device") {
        return Err("Open your profile settings in the account web UI to link SIMKL".into());
    }
    let client = app.simkl.client().map_err(ApiError::from)?;
    if app.simkl.connected(p) {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Disconnect the existing SIMKL connection first".into(),
        ));
    }
    if app.simkl.vault.is_none() {
        return Err("SIMKL encrypted storage unavailable".into());
    }
    let redirect =
        std::env::var("SIMKL_REDIRECT_URI").map_err(|_| "SIMKL redirect URI is not configured")?;
    let state = auth::token();
    let verifier = auth::token();
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let mut url = url::Url::parse("https://simkl.com/oauth2/authorize").unwrap();
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &client.client_id)
        .append_pair("redirect_uri", &redirect)
        .append_pair("scope", "media:read media:write")
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");
    let db = app.db.lock().unwrap();
    kids::require_parent(&db, &principal)?;
    db.execute(
        "DELETE FROM simkl_oauth WHERE expires<?1 OR profile_id=?2",
        params![util::now(), p],
    )
    .map_err(db_error)?;
    let sealed = app
        .simkl
        .vault
        .as_ref()
        .unwrap()
        .seal(
            principal.account_id().unwrap(),
            "simkl-oauth",
            &auth::hash(&state),
            verifier.as_bytes(),
        )
        .map_err(|_| "SIMKL authorization storage failed")?;
    db.execute(
        "INSERT INTO simkl_oauth VALUES(?1,?2,?3,?4,?5,?6)",
        params![
            auth::hash(&state),
            p,
            principal.account_id(),
            principal.session_id(),
            sealed,
            util::now() + 600
        ],
    )
    .map_err(db_error)?;
    Ok(axum::Json(json!({"authorize_url":url.to_string()})))
}
#[derive(Deserialize)]
pub(crate) struct Callback {
    state: String,
    code: Option<String>,
    error: Option<String>,
}
pub(crate) async fn callback(
    State(app): State<App>,
    Extension(principal): Extension<auth::Principal>,
    Query(q): Query<Callback>,
) -> Result<axum::response::Redirect, ApiError> {
    let _guard = app.simkl.gate.lock().await;
    let state = auth::hash(&q.state);
    let (p, account, session, sealed): (i64, i64, String, String) = {
        let db = app.db.lock().unwrap();
        principal.validate_scope(&db)?;
        let row=db.query_row("SELECT profile_id,account_id,session_id,verifier FROM simkl_oauth WHERE state=?1 AND expires>?2",params![state,util::now()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(db_error)?.ok_or("SIMKL authorization expired")?;
        db.execute("DELETE FROM simkl_oauth WHERE state=?1", [&state])
            .map_err(db_error)?;
        row
    };
    let auth::Principal::Account { profile_id, .. } = &principal;
    if principal.account_id() != Some(account)
        || principal.session_id() != Some(session.as_str())
        || *profile_id != Some(p)
    {
        return Err(auth::forbidden());
    }
    if q.error.is_some() {
        return Ok(axum::response::Redirect::to("/?simkl=cancelled"));
    }
    let vault = app
        .simkl
        .vault
        .as_ref()
        .ok_or("SIMKL encrypted storage unavailable")?;
    let verifier = vault
        .open(account, "simkl-oauth", &state, &sealed)
        .map_err(|_| "SIMKL authorization invalid")?;
    let tokens = app
        .simkl
        .client()
        .map_err(ApiError::from)?
        .token(&[
            ("grant_type", "authorization_code".into()),
            ("code", q.code.ok_or("SIMKL authorization code missing")?),
            (
                "redirect_uri",
                std::env::var("SIMKL_REDIRECT_URI").map_err(|_| "SIMKL redirect URI missing")?,
            ),
            (
                "code_verifier",
                String::from_utf8(verifier.expose().to_vec())
                    .map_err(|_| "SIMKL authorization invalid")?,
            ),
        ])
        .await
        .map_err(|_| "SIMKL authorization failed")?;
    let granted = tokens["scope"].as_str().unwrap_or("");
    if !granted.split_whitespace().any(|s| s == "media:write") {
        return Err("SIMKL write permission was not granted".into());
    }
    let user = app
        .simkl
        .client()
        .map_err(ApiError::from)?
        .get("/users/settings", tokens["access_token"].as_str())
        .await
        .map_err(|_| "SIMKL user lookup failed")?;
    let user_id = user["account"]["id"]
        .as_u64()
        .map(|n| n.to_string())
        .or_else(|| user["account"]["id"].as_str().map(str::to_owned))
        .ok_or("SIMKL user identity missing")?;
    let encrypted = vault
        .seal(
            account,
            "simkl",
            &p.to_string(),
            tokens.to_string().as_bytes(),
        )
        .map_err(|_| "SIMKL token storage failed")?;
    let generation = Uuid::new_v4().to_string();
    let conflict = {
        let db = app.db.lock().unwrap();
        principal.validate_scope(&db)?;
        let other:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM simkl_connections WHERE user_id=?1 AND profile_id!=?2)",params![user_id,p],|r|r.get(0)).map_err(db_error)?;
        other || app.simkl.connected_without_lock(&db, p)
    };
    if conflict {
        if let Some(token) = tokens["refresh_token"].as_str() {
            let _ = app
                .simkl
                .client()
                .map_err(ApiError::from)?
                .revoke(token)
                .await;
        }
        return Err(ApiError(
            StatusCode::CONFLICT,
            "That SIMKL user or profile already has a connection".into(),
        ));
    }
    {
        let db = app.db.lock().unwrap();
        principal.validate_scope(&db)?;
        db.execute("INSERT INTO simkl_connections(profile_id,account_id,user_id,user_name,tokens,expires,generation) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![p,account,user_id,user["user"]["name"].as_str().unwrap_or("SIMKL user"),encrypted,util::now()+tokens["expires_in"].as_i64().unwrap_or(604800),generation]).map_err(db_error)?;
    }
    let service = app.simkl.clone();
    tokio::spawn(async move {
        let _ = service.sync(p, true).await;
    });
    Ok(axum::response::Redirect::to("/?simkl=linked"))
}
pub(crate) async fn disconnect(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(p): Path<i64>,
) -> ApiResult {
    let app = scope(app, lease, p)?;
    let _guard = app.simkl.gate.lock().await;
    {
        let db = app.db.lock().unwrap();
        kids::require_parent(&db, &app.identity())?;
    }
    if app.simkl.connected(p) {
        let (account, sealed): (i64, String) = app
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT account_id,tokens FROM simkl_connections WHERE profile_id=?1",
                [p],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(db_error)?;
        let plain = app
            .simkl
            .vault
            .as_ref()
            .ok_or("SIMKL encrypted storage unavailable")?
            .open(account, "simkl", &p.to_string(), &sealed)
            .map_err(|_| "SIMKL token unavailable")?;
        let tokens: Value =
            serde_json::from_slice(plain.expose()).map_err(|_| "SIMKL token unavailable")?;
        let token = tokens["refresh_token"]
            .as_str()
            .or_else(|| tokens["access_token"].as_str())
            .ok_or("SIMKL token unavailable")?;
        app.simkl
            .client()
            .map_err(ApiError::from)?
            .revoke(token)
            .await
            .map_err(|_| "SIMKL revocation unavailable; try again")?;
    }
    let db = app.db.lock().unwrap();
    app.require_profile(&db, p)?;
    for table in [
        "simkl_connections",
        "simkl_oauth",
        "simkl_exports",
        "simkl_outbox",
        "simkl_events",
        "simkl_library",
        "simkl_user_cache",
    ] {
        db.execute(&format!("DELETE FROM {table} WHERE profile_id=?1"), [p])
            .map_err(db_error)?;
    }
    Ok(axum::Json(json!({"connected":false})))
}
pub(crate) async fn sync_now(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(p): Path<i64>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    let app = scope(app, lease, p)?;
    if !app.simkl.connected(p) {
        return Ok(axum::Json(json!({"connected":false})));
    }
    Ok(axum::Json(
        app.simkl
            .sync(p, q.get("manual").is_none_or(|v| v != "false"))
            .await
            .map_err(ApiError::from)?,
    ))
}
pub(crate) async fn watchlist(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(p): Path<i64>,
    axum::Json(body): axum::Json<Value>,
) -> ApiResult {
    let app = scope(app, lease, p)?;
    let status = body["status"]
        .as_str()
        .ok_or("Missing SIMKL watchlist status")?;
    if !viptv_simkl::STATUSES.contains(&status) {
        return Err("Invalid SIMKL status".into());
    }
    let item = &body["item"];
    let id = text(item, "id", 512)?;
    let kind = media_type(item)?;
    if kind == "movie" && ["watching", "hold"].contains(&status) {
        return Err("Movies do not support this SIMKL status".into());
    }
    {
        let db = app.db.lock().unwrap();
        kids::require_item(&db, &app.identity(), kind, id)?;
    }
    let payload = viptv_simkl::write_item(item).map_err(ApiError::from)?;
    let mut entry = payload
        .get("movie")
        .or_else(|| payload.get("show"))
        .or_else(|| payload.get("anime"))
        .unwrap()
        .clone();
    entry["to"] = json!(status);
    let value = if kind == "movie" {
        json!({"movies":[entry]})
    } else {
        json!({"shows":[entry]})
    };
    let media = json!({"title":item["name"],"year":item["year"],"ids":entry["ids"]});
    let mut mirror = if kind == "movie" {
        json!({"status":status,"movie":media})
    } else if item["simkl_category"] == "anime" {
        json!({"status":status,"anime":media})
    } else {
        json!({"status":status,"show":media})
    };
    mirror["local"] = json!(true);
    mirror["updated_at"] = json!(util::now());
    {
        let db = app.db.lock().unwrap();
        app.require_profile(&db, p)?;
        db.execute("INSERT INTO simkl_library VALUES(?1,?2,?3) ON CONFLICT(profile_id,id) DO UPDATE SET value=excluded.value",params![p,id,mirror.to_string()]).map_err(db_error)?;
        if ["plantowatch", "watching", "hold"].contains(&status) {
            db.execute("INSERT INTO favorites(profile_id,id,type,name,poster) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(profile_id,type,id) DO UPDATE SET name=excluded.name,poster=excluded.poster",params![p,id,kind,item["name"].as_str().unwrap_or(id),item["poster"].as_str()]).map_err(db_error)?;
        }
    }
    if app.simkl.connected(p) {
        app.simkl
            .enqueue(p, id, "add-to-list", &value)
            .map_err(ApiError::from)?;
        let _guard = app.simkl.gate.lock().await;
        app.simkl.flush(p).await.map_err(ApiError::from)?;
    }
    Ok(axum::Json(json!({"saved":true,"status":status})))
}

pub(crate) async fn watchlist_items(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(p): Path<i64>,
) -> ApiResult {
    let app = scope(app, lease, p)?;
    let rows = {
        let db = app.db.lock().unwrap();
        let rows = db
            .prepare(
                "SELECT id,value FROM simkl_library WHERE profile_id=?1 ORDER BY id LIMIT 5000",
            )
            .map_err(db_error)?
            .query_map([p], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        rows
    };
    let mut items = vec![];
    for (id, value) in rows {
        let row: Value =
            serde_json::from_str(&value).map_err(|_| "Invalid saved watchlist item")?;
        let (category, title) = if row["movie"].is_object() {
            (Category::Movie, &row["movie"])
        } else if row["anime"].is_object() {
            (Category::Anime, &row["anime"])
        } else {
            (Category::Tv, &row["show"])
        };
        let Some(mut item) = app
            .simkl
            .cached_item(&id)
            .or_else(|| viptv_simkl::normalize(title, category))
        else {
            continue;
        };
        item["watchlist_status"] = row["status"].clone();
        item["user_rating"] = row["user_rating"].clone();
        if kids::require_item(
            &app.db.lock().unwrap(),
            &app.identity(),
            item["type"].as_str().unwrap_or("series"),
            &id,
        )
        .is_ok()
        {
            items.push(item);
        }
    }
    Ok(axum::Json(json!({"metas":items})))
}
pub(crate) async fn lists(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(p): Path<i64>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    let app = scope(app, lease, p)?;
    let _guard = app.simkl.gate.lock().await;
    let user: String = app
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT user_id FROM simkl_connections WHERE profile_id=?1",
            [p],
            |r| r.get(0),
        )
        .map_err(|_| "Link SIMKL to see custom lists")?;
    let page = q
        .get("page")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(1)
        .clamp(1, 1000);
    Ok(axum::Json(
        app.simkl
            .custom(p, &format!("/lists/user/{user}?page={page}&limit=20&followed=true&collaborants=true"))
            .await
            .map_err(ApiError::from)?,
    ))
}
pub(crate) async fn list(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path((p, id)): Path<(i64, u64)>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    let app = scope(app, lease, p)?;
    let _guard = app.simkl.gate.lock().await;
    let page = q
        .get("page")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(1)
        .clamp(1, 1000);
    let mut result = app
        .simkl
        .custom(p, &format!("/lists/{id}?page={page}&limit=50"))
        .await
        .map_err(ApiError::from)?;
    if result["error"] != "premium_only" {
        let items: Vec<_> = result["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|row| {
                let (category, media) = if row["movie"].is_object() {
                    (Category::Movie, &row["movie"])
                } else if row["show"].is_object() {
                    (Category::Tv, &row["show"])
                } else if row["anime"].is_object() {
                    (Category::Anime, &row["anime"])
                } else {
                    (
                        match row["type"].as_str() {
                            Some("movie" | "movies") => Category::Movie,
                            Some("anime") => Category::Anime,
                            _ => Category::Tv,
                        },
                        row,
                    )
                };
                viptv_simkl::normalize(media, category)
            })
            .collect();
        app.simkl.remember(&items).map_err(ApiError::from)?;
        result["metas"] = json!(items);
    }
    Ok(axum::Json(result))
}
pub(crate) async fn scrobble(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(p): Path<i64>,
    axum::Json(event): axum::Json<viptv_simkl::PlaybackEvent>,
) -> ApiResult {
    let app = scope(app, lease, p)?;
    if !["start", "pause", "stop", "complete"].contains(&event.action.as_str())
        || event.event_id.is_empty()
        || event.event_id.len() > 128
        || event.session_id.is_empty()
        || event.session_id.len() > 128
        || !event.position.is_finite()
        || !event.duration.is_finite()
        || event.position < 0.0
        || event.duration < 0.0
        || event.duration > 1e9
    {
        return Err("Invalid SIMKL playback event".into());
    }
    let _guard = app.simkl.gate.lock().await;
    {
        let db = app.db.lock().unwrap();
        app.require_profile(&db, p)?;
        let seen: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM simkl_events WHERE profile_id=?1 AND event_id=?2)",
                params![p, event.event_id],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        if seen {
            return Ok(axum::Json(json!({"duplicate":true})));
        }
    }

    let mut item = event.item.clone();
    if item["type"] == "live" {
        return Ok(axum::Json(json!({"ignored":true})));
    }
    item["position"] = json!(if event.duration > 0.0 {
        event.position.min(event.duration)
    } else {
        event.position
    });
    item["duration"] = json!(event.duration);
    let _ = save_progress(State(app.clone()), Path(p), axum::Json(item.clone())).await?;
    if event.action == "complete" {
        let db = app.db.lock().unwrap();
        app.require_profile(&db, p)?;
        db.execute("UPDATE progress SET context=json_set(context,'$.watched_override',1,'$.simkl_completion_only',?1) WHERE profile_id=?2 AND id=?3 AND type=?4",params![event.duration<=0.0,p,item["id"].as_str(),item["type"].as_str()]).map_err(db_error)?;
    }
    if !app.simkl.connected(p) {
        return Ok(axum::Json(json!({"connected":false})));
    }
    if event.duration <= 0.0 && event.action != "complete" {
        return Ok(axum::Json(
            json!({"ignored":true,"reason":"duration_unknown"}),
        ));
    }
    let mut payload = viptv_simkl::write_item(&item).map_err(ApiError::from)?;
    payload["progress"] = json!(if event.action == "complete" {
        100.0
    } else {
        (event.position / event.duration * 100.0).clamp(0.0, 100.0)
    });
    let token = app.simkl.token(p).await.map_err(ApiError::from)?;
    let action = if event.action == "complete" {
        "stop"
    } else {
        &event.action
    };
    let result = app
        .simkl
        .client()
        .map_err(ApiError::from)?
        .post(&format!("/scrobble/{action}"), &token, payload)
        .await
        .map_err(|e| ApiError::from(e.to_string()))?;
    let db = app.db.lock().unwrap();
    app.require_profile(&db, p)?;
    db.execute(
        "DELETE FROM simkl_events WHERE created<?1",
        [util::now() - 86400 * 7],
    )
    .map_err(db_error)?;
    db.execute(
        "INSERT OR IGNORE INTO simkl_events VALUES(?1,?2,?3,?4,?5)",
        params![
            p,
            event.event_id,
            event.session_id,
            event.action,
            util::now()
        ],
    )
    .map_err(db_error)?;
    if result["action"] == "scrobble" {
        db.execute(
            "INSERT OR IGNORE INTO simkl_exports VALUES(?1,?2)",
            params![p, format!("watched:{}", item["id"].as_str().unwrap_or(""))],
        )
        .map_err(db_error)?;
        db.execute(
            "INSERT OR IGNORE INTO simkl_exports VALUES(?1,?2)",
            params![
                p,
                format!(
                    "history:{}:{}",
                    item["id"],
                    db.query_row(
                        "SELECT updated_at FROM progress WHERE profile_id=?1 AND id=?2 AND type=?3",
                        params![p, item["id"].as_str(), item["type"].as_str()],
                        |r| r.get::<_, i64>(0)
                    )
                    .map_err(db_error)?
                )
            ],
        )
        .map_err(db_error)?;
    }
    Ok(axum::Json(result))
}
impl Service {
    fn connected_without_lock(&self, db: &Connection, p: i64) -> bool {
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM simkl_connections WHERE profile_id=?1)",
            [p],
            |r| r.get(0),
        )
        .unwrap_or(false)
    }
}

/// First unwatched episode per actively watched show, projected from the sync snapshot.
/// Opening this screen never crawls episodes or sends another SIMKL request.
pub(crate) async fn up_next(
    State(app): State<App>, Extension(lease): Extension<ResourceLease>, Path(p): Path<i64>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    let app = scope(app, lease, p)?;
    let rows: Vec<(String, String)> = {
        let db = app.db.lock().unwrap();
        let mut query = db.prepare("SELECT id,value FROM simkl_library WHERE profile_id=?1 AND json_extract(value,'$.status')='watching' ORDER BY id").map_err(db_error)?;
        let rows = query.query_map([p], |row| Ok((row.get(0)?, row.get(1)?))).map_err(db_error)?
            .collect::<Result<_, _>>().map_err(db_error)?;
        rows
    };
    let mut items = Vec::new();
    for (id, value) in rows {
        let row: Value = serde_json::from_str(&value).map_err(|_| "Invalid SIMKL library snapshot")?;
        let Some(marker) = row["next_to_watch"].as_str() else { continue };
        let (category, title) = if row["anime"].is_object() { (Category::Anime, &row["anime"]) } else { (Category::Tv, &row["show"]) };
        let Some(parent) = app.simkl.cached_item(&id).or_else(|| viptv_simkl::normalize(title, category)) else { continue };
        if kids::require_item(&app.db.lock().unwrap(), &app.identity(), "series", &id).is_err() { continue }
        let mut episode = row["next_to_watch_info"].clone();
        if !episode.is_object() { episode = json!({}); }
        if episode["episode"].as_u64().is_none() {
            if let Some((season, number)) = marker.strip_prefix('S').and_then(|v| v.split_once('E')) {
                episode["season"] = json!(season.parse::<u64>().ok()); episode["episode"] = json!(number.parse::<u64>().ok());
            } else { episode["episode"] = json!(marker.trim_start_matches('E').parse::<u64>().ok()); }
        }
        let Some(mut item) = viptv_simkl::episode(&parent, &episode) else { continue };
        item["type"] = json!("episode");
        item["watchlist_status"] = json!("watching");
        item["watched"] = json!(false);
        items.push(item);
    }
    if q.get("sort").is_some_and(|v| v == "latest") {
        fn aired(item: &Value) -> i64 {
            item["released"].as_str().and_then(|date| chrono::DateTime::parse_from_rfc3339(date).ok())
                .map(|date| date.timestamp()).unwrap_or(i64::MIN)
        }
        items.sort_by_key(|item| std::cmp::Reverse(aired(item)));
    }
    Ok(axum::Json(json!({"metas":items,"coverage":"first_unwatched_per_show","connected":app.simkl.connected(p)})))
}
