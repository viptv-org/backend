use super::*;

pub(crate) fn allow_item(
    db: &Connection,
    p: &auth::Principal,
    kind: &str,
    id: &str,
) -> Result<bool, ApiError> {
    let auth::Principal::Account { profile_id, .. } = p;
    // Share the collection predicate so episode cards and direct source requests agree.
    // This reads only policy fields instead of reparsing a full series for every episode.
    let allowed = sql_allowed("?1", "?2", "?3");
    db.query_row(
        &format!("SELECT {allowed}"),
        params![kind, id, profile_id],
        |r| r.get(0),
    )
    .map_err(db_error)
}

pub(crate) fn require_item(
    db: &Connection,
    p: &auth::Principal,
    kind: &str,
    id: &str,
) -> Result<(), ApiError> {
    if allow_item(db, p, kind, id)? {
        Ok(())
    } else {
        Err(forbidden())
    }
}
pub(crate) fn filter_items(
    db: &Connection,
    p: &auth::Principal,
    values: Vec<Value>,
) -> Result<Vec<Value>, ApiError> {
    if !restricted(db, p)? {
        return Ok(values);
    }
    let mut safe = Vec::new();
    for mut value in values {
        let kind = value["type"].as_str().unwrap_or("");
        let id = value["id"].as_str().unwrap_or("");
        if allow_item(db, p, kind, id)? {
            if let Some((metadata, _, _, _)) = media(db, p, kind, id)? {
                for key in ["name", "poster", "background", "series_id"] {
                    if !metadata[key].is_null() {
                        value[key] = metadata[key].clone();
                    }
                }
            }
            safe.push(value);
        }
    }
    Ok(safe)
}
fn live_allowed(db: &Connection, id: &str) -> Result<bool, ApiError> {
    if !id.starts_with("family:") {
        return Ok(false);
    }
    let metadata: Option<String> = db
        .query_row("SELECT data FROM family_channels WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .optional()
        .map_err(db_error)?;
    let value: Value = metadata
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    Ok(value["enabled"] == true
        && value["category"]
            .as_str()
            .is_some_and(|s| s.eq_ignore_ascii_case("kids"))
        && value["country"] == "US"
        && value["language"] == "en")
}

fn query(req: &Request) -> HashMap<String, String> {
    url::form_urlencoded::parse(req.uri().query().unwrap_or("").as_bytes())
        .into_owned()
        .collect()
}
fn decode_path(value: &str) -> Result<String, ApiError> {
    let bytes = value.as_bytes();
    let mut output = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err("Invalid path".into());
            }
            let n = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|v| u8::from_str_radix(v, 16).ok())
                .ok_or("Invalid path")?;
            output.push(n);
            i += 3;
        } else {
            output.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(output).map_err(|_| "Invalid path".into())
}
/// A restricted request never falls through to arbitrary addon catalogs or raw provider live listings.
enum PolicyDecision {
    Pass,
    Respond(Value),
    Sanitize,
}
fn decide_request(
    app: &App,
    path: &str,
    method: &axum::http::Method,
    q: &HashMap<String, String>,
) -> Result<PolicyDecision, ApiError> {
    let parts: Vec<_> = path.trim_matches('/').split('/').collect();
    let restricted_now = {
        let db = app.db.lock().unwrap();
        app.request_lease().validate(&db)?;
        restricted(&db, &app.identity())?
    };
    if !restricted_now {
        return Ok(PolicyDecision::Pass);
    }
    {
        let db = app.db.lock().unwrap();
        app.request_lease().validate(&db)?;
        let p = app.identity();
        match parts.as_slice() {
            ["parent", ..] => return Ok(PolicyDecision::Pass),
            ["profiles"] if method == axum::http::Method::GET => return Ok(PolicyDecision::Pass),
            ["profiles", _, "kids" | "approvals"] => {
                require_parent(&db, &p)?;
                return Ok(PolicyDecision::Pass);
            }
            ["profiles", _, "preferences"] => return Ok(PolicyDecision::Pass),
            ["profiles", _, "favorites" | "progress" | "continue", ..] => {}
            ["catalogs"] => {
                return Ok(PolicyDecision::Respond(
                    json!([{"addon_id":0,"id":"kids-movies","type":"movie","name":"Family movies","supports_search":true,"supports_skip":true,"extra":[],"genres":[]},{"addon_id":0,"id":"kids-series","type":"series","name":"Family series","supports_search":true,"supports_skip":true,"extra":[],"genres":[]}]),
                ))
            }
            ["discover"] => {
                let kind = q.get("type").map(String::as_str).unwrap_or("movie");
                let search = q
                    .get("search")
                    .map(|v| v.to_lowercase())
                    .unwrap_or_default();
                let offset = q
                    .get("skip")
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(0)
                    .min(10000);
                let auth::Principal::Account { profile_id, .. } = p;
                let allowed = sql_allowed("catalog.kind", "catalog.id", "?3");
                let predicate = format!("catalog.account_id=?1 AND catalog.kind=?2 AND catalog.id=catalog.parent_id AND instr(lower(json_extract(catalog.metadata,'$.name')),?4)>0 AND {allowed}");
                let total: usize = db
                    .query_row(
                        &format!("SELECT count(*) FROM kids_media catalog WHERE {predicate}"),
                        params![p.account_id(), kind, profile_id, search],
                        |r| r.get(0),
                    )
                    .map_err(db_error)?;
                let mut stmt = db.prepare(&format!("SELECT json_remove(metadata,'$.videos','$.links','$.recommendations') FROM kids_media catalog WHERE {predicate} ORDER BY updated_at DESC,id LIMIT 80 OFFSET ?5")).map_err(db_error)?;
                let values = stmt
                    .query_map(
                        params![p.account_id(), kind, profile_id, search, offset],
                        |r| r.get::<_, String>(0),
                    )
                    .map_err(db_error)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(db_error)?
                    .into_iter()
                    .filter_map(|s| serde_json::from_str::<Value>(&s).ok())
                    .map(|mut v| {
                        v["type"] = json!(kind);
                        v
                    })
                    .collect::<Vec<_>>();
                return Ok(PolicyDecision::Respond(
                    json!({"metas":values,"has_more":offset+80<total,"next_skip":offset+80,"total":total}),
                ));
            }
            ["meta", kind, id] => {
                let id = decode_path(id)?;
                require_item(&db, &p, kind, &id)?;
                let (mut meta, _, _, _) = media(&db, &p, kind, &id)?.ok_or_else(forbidden)?;
                if let Some(videos) = meta["videos"].as_array_mut() {
                    let mut eligible = Vec::new();
                    for video in std::mem::take(videos) {
                        if let Some(episode) = video["id"].as_str() {
                            if allow_item(&db, &p, "series", episode)? {
                                eligible.push(video);
                            }
                        }
                    }
                    *videos = eligible;
                }
                return Ok(PolicyDecision::Respond(json!({"meta":meta})));
            }
            ["live"] => {
                let auth::Principal::Account { profile_id, .. } = p;
                let mut result = live_catalog::browse(
                    &db,
                    Some("Kids"),
                    q.get("search").map(String::as_str),
                    q.get("collection").map(String::as_str),
                    profile_id,
                    0,
                    500,
                )?;
                let rows = result["channels"]
                    .as_array_mut()
                    .map(std::mem::take)
                    .unwrap_or_default();
                let rows = rows
                    .into_iter()
                    .filter(|v| {
                        v["id"]
                            .as_str()
                            .is_some_and(|id| live_allowed(&db, id).unwrap_or(false))
                    })
                    .collect::<Vec<_>>();
                let total = rows.len();
                let offset = q
                    .get("offset")
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(0);
                let limit = q
                    .get("limit")
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(40)
                    .clamp(1, 100);
                return Ok(PolicyDecision::Respond(
                    json!({"channels":rows.into_iter().skip(offset).take(limit).collect::<Vec<_>>(),"total":total}),
                ));
            }
            ["live", "categories"] => {
                return Ok(PolicyDecision::Respond(
                    json!({"categories":[{"id":"category:Kids","name":"Kids"}],"total":1}),
                ))
            }
            ["guide", id] => {
                require_item(&db, &p, "live", &decode_path(id)?)?;
                return Ok(PolicyDecision::Pass);
            }
            ["streams"] | ["playback"] => {}
            ["streams", ..] | ["playback", ..] => return Ok(PolicyDecision::Pass),
            _ => {
                require_parent(&db, &p)?;
                return Ok(PolicyDecision::Pass);
            }
        }
        if method == axum::http::Method::GET {
            if parts.last() == Some(&"series") {
                require_item(
                    &db,
                    &p,
                    "series",
                    q.get("series_id").map(String::as_str).unwrap_or(""),
                )?;
            }
            return Ok(PolicyDecision::Pass);
        }
        if method == axum::http::Method::DELETE {
            return Ok(PolicyDecision::Pass);
        }
        if parts.last() == Some(&"settings") {
            return Ok(PolicyDecision::Pass);
        }
    }
    Ok(PolicyDecision::Sanitize)
}
fn sanitize_request(app: &App, path: &str, mut value: Value) -> Result<Value, ApiError> {
    {
        let db = app.db.lock().unwrap();
        app.request_lease().validate(&db)?;
        let p = app.identity();
        if path == "/playback" {
            if let Some(id) = value["channel_id"].as_str() {
                require_item(&db, &p, "live", id)?;
            }
            // stream_id must have been issued to this exact profile/revision; existing playback authorization checks it.
        } else {
            let kind = value["type"].as_str().unwrap_or("").to_owned();
            let id = value["id"].as_str().unwrap_or("").to_owned();
            require_item(&db, &p, &kind, &id)?;
            if kind != "live" {
                let (meta, parent, _, _) = media(&db, &p, &kind, &id)?.ok_or_else(forbidden)?;
                let object = value.as_object_mut().ok_or("Invalid item")?;
                for key in [
                    "series_id",
                    "season",
                    "episode",
                    "imdb_id",
                    "tmdb_id",
                    "aliases",
                    "year",
                    "name",
                    "poster",
                    "title",
                    "seriesName",
                    "title_id",
                ] {
                    object.remove(key);
                }
                for key in ["name", "poster", "season", "episode"] {
                    if !meta[key].is_null() {
                        value[key] = meta[key].clone();
                    }
                }
                if kind == "series" {
                    value["series_id"] = json!(parent);
                }
                // Never let an approved title's opaque request smuggle an unrelated provider ID.
                if path == "/streams" {
                    let mut safe = json!({"type":kind,"id":id,"name":meta["name"]});
                    for key in ["series_id", "season", "episode"] {
                        if !value[key].is_null() {
                            safe[key] = value[key].clone();
                        }
                    }
                    enrich_matching(&mut safe, &meta);
                    value = safe;
                }
            }
        }
    }
    Ok(value)
}
pub(crate) async fn before(app: &App, req: &mut Request) -> Result<Option<Value>, ApiError> {
    let path = req.uri().path().trim_start_matches("/api").to_owned();
    let method = req.method().clone();
    let q = query(req);
    let decision_app = app.clone();
    let decision_path = path.clone();
    match blocking(move || decide_request(&decision_app, &decision_path, &method, &q)).await? {
        PolicyDecision::Pass => return Ok(None),
        PolicyDecision::Respond(value) => return Ok(Some(value)),
        PolicyDecision::Sanitize => {}
    }
    let bytes = axum::body::to_bytes(
        std::mem::replace(req.body_mut(), axum::body::Body::empty()),
        65536,
    )
    .await
    .map_err(|_| ApiError::from("Request too large"))?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| "Invalid request")?;
    let app = app.clone();
    let value = blocking(move || sanitize_request(&app, &path, value)).await?;
    req.headers_mut().remove(header::CONTENT_LENGTH);
    *req.body_mut() = axum::body::Body::from(value.to_string());
    Ok(None)
}
pub(crate) async fn after(app: &App, path: &str, response: Response) -> Response {
    if !response.status().is_success() || path.contains("/events") {
        return response;
    }
    let sensitive = path.starts_with("/streams")
        || path.starts_with("/playback")
        || path.starts_with("/live")
        || path.starts_with("/guide")
        || path == "/discover"
        || path.starts_with("/meta/")
        || (path.starts_with("/profiles/")
            && (path.contains("/favorites")
                || path.contains("/progress")
                || path.contains("/continue")));
    if !sensitive {
        return response;
    }
    let restricted_now = {
        let db = app.db.lock().unwrap();
        if let Err(error) = app.request_lease().validate(&db) {
            return error.into_response();
        }
        restricted(&db, &app.identity()).unwrap_or(true)
    };
    let observe_response = path == "/discover" || path.starts_with("/meta/");
    let personal = path.starts_with("/profiles/")
        && (path.contains("/favorites")
            || path.contains("/progress")
            || path.contains("/continue"));
    if !observe_response && !(restricted_now && personal) {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let bytes = match axum::body::to_bytes(body, 8 * 1024 * 1024).await {
        Ok(v) => v,
        Err(_) => return ApiError::from("Response too large").into_response(),
    };
    let mut value: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return ApiError::from("Invalid response").into_response(),
    };
    let worker_app = app.clone();
    let worker_path = path.to_owned();
    let result = blocking(move || -> Result<Value, ApiError> {
        let app = worker_app;
        let path = worker_path;
        let mut guard = app.db.lock().unwrap();
        let db = guard.transaction().map_err(db_error)?;
        app.request_lease().validate(&db)?;
        let p = app.identity();
        if !restricted_now && observe_response {
            if let Some(items) = value["metas"].as_array() {
                for item in items.iter().take(500) {
                    observe(
                        &db,
                        p.account_id().unwrap_or(0),
                        item["type"].as_str().unwrap_or("movie"),
                        item,
                        false,
                    )?;
                }
            }
            if value["meta"].is_object() {
                let kind = path.trim_matches('/').split('/').nth(1).unwrap_or("movie");
                observe(
                    &db,
                    p.account_id().unwrap_or(0),
                    kind,
                    &value["meta"],
                    false,
                )?;
            }
        }
        if !restricted_now && observe_response {
            prune_observations(&db, p.account_id().unwrap_or(0))?;
        }
        if restricted_now && personal {
            filter_response_items(&db, &p, &mut value)?;
        }
        db.commit().map_err(db_error)?;
        Ok(value)
    })
    .await;
    let value = match result {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    parts.headers.remove(header::CONTENT_LENGTH);
    parts.headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    Response::from_parts(parts, axum::body::Body::from(value.to_string()))
}
fn filter_response_items(
    db: &Connection,
    p: &auth::Principal,
    value: &mut Value,
) -> Result<(), ApiError> {
    if let Some(items) = value.as_array_mut() {
        *items = filter_items(db, p, std::mem::take(items))?;
        return Ok(());
    }
    if let Some(items) = value["items"].as_array_mut() {
        *items = filter_items(db, p, std::mem::take(items))?;
    }
    if value["item"].is_object() {
        let mut safe = filter_items(db, p, vec![value["item"].clone()])?;
        if let Some(item) = safe.pop() {
            value["item"] = item;
        } else {
            *value = json!({"status":"unavailable"});
        }
    }
    Ok(())
}

/// SQL counterpart used before pagination; arguments are compile-time column/parameter names.
pub(crate) fn sql_allowed(kind: &str, id: &str, profile: &str) -> String {
    format!("(NOT EXISTS(SELECT 1 FROM kids_profiles k0 WHERE k0.profile_id={profile} AND k0.enabled=1) OR EXISTS(SELECT 1 FROM kids_media m JOIN profile_owners o ON o.account_id=m.account_id JOIN kids_profiles k ON k.profile_id=o.profile_id JOIN kids_media root ON root.account_id=m.account_id AND root.kind=m.kind AND root.id=m.parent_id WHERE o.profile_id={profile} AND m.kind={kind} AND m.id={id} AND NOT EXISTS(SELECT 1 FROM kids_ambiguous bad WHERE bad.account_id=m.account_id AND bad.kind=m.kind AND bad.id=m.id) AND COALESCE(m.age,0)<18 AND COALESCE(root.age,0)<18 AND (m.id=m.parent_id OR EXISTS(SELECT 1 FROM json_each(root.metadata,'$.videos') video WHERE json_extract(video.value,'$.id')=m.id)) AND (EXISTS(SELECT 1 FROM kids_approvals a WHERE a.profile_id={profile} AND a.kind=m.kind AND a.id=m.parent_id) OR (m.conflict=0 AND root.conflict=0 AND root.age<=k.max_age AND m.age<=k.max_age))) OR ({kind}='live' AND EXISTS(SELECT 1 FROM family_channels f WHERE f.id={id} AND f.id LIKE 'family:%' AND json_extract(f.data,'$.enabled')=1 AND lower(json_extract(f.data,'$.category'))='kids' AND json_extract(f.data,'$.country')='US' AND json_extract(f.data,'$.language')='en')))")
}
