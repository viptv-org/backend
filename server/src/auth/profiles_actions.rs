use super::*;
use axum::http::HeaderMap;

fn with_primary(db: &Connection, account: i64, mut value: Value) -> Result<Value, ApiError> {
    value["is_primary"] = json!(
        value["id"].as_str().and_then(|v| v.parse::<i64>().ok()) == primary_profile(db, account)?
    );
    if let Some(id) = value["id"].as_str().and_then(|v| v.parse::<i64>().ok()) {
        let policy = crate::kids::profile_fields(db, id)?;
        value["kids"] = policy["enabled"].clone();
        value["max_age"] = policy["max_age"].clone();
    }
    Ok(value)
}
pub(crate) fn list_profiles(db: &Connection, account_id: i64) -> Result<Vec<Value>, ApiError> {
    let mut statement = db
        .prepare(
            "SELECT p.id,p.name,p.avatar_style,p.avatar_seed,p.presentation_complete
         FROM profiles p JOIN profile_owners o ON o.profile_id=p.id
         WHERE o.account_id=?1 ORDER BY p.id",
        )
        .map_err(crate::db_error)?;
    let rows = statement
        .query_map([account_id], |row| {
            let id = row.get::<_, i64>(0)?;
            let name = row.get::<_, String>(1)?;
            let style = row.get::<_, String>(2)?;
            let seed = row.get::<_, String>(3)?;
            let complete = row.get::<_, bool>(4)?;
            Ok(profile_json(id, &name, &style, &seed, complete))
        })
        .map_err(crate::db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(crate::db_error)?;
    rows.into_iter()
        .map(|value| with_primary(db, account_id, value))
        .collect()
}
pub(crate) fn create_profile(
    db: &Connection,
    account_id: i64,
    value: &Value,
) -> Result<Value, ApiError> {
    validate_profile_payload(value, false)?;
    let name = crate::text(value, "name", 80)?;
    let style = avatar_style(value.get("avatar_style").and_then(Value::as_str))?;
    let count: i64 = db
        .query_row(
            "SELECT count(*) FROM profile_owners WHERE account_id=?1",
            [account_id],
            |row| row.get(0),
        )
        .map_err(crate::db_error)?;
    if count >= MAX_PROFILES {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Profile limit reached".into(),
        ));
    }
    let seed = selected_avatar_seed(value, style, Uuid::new_v4().simple().to_string())?;
    let timestamp = now();
    db.execute("INSERT INTO profiles(name,avatar_style,avatar_seed,presentation_complete,created_at,updated_at) VALUES(?1,?2,?3,1,?4,?4)",params![name,style,seed,timestamp]).map_err(crate::db_error)?;
    let profile_id = db.last_insert_rowid();
    db.execute(
        "INSERT INTO profile_owners(profile_id,account_id,created_at) VALUES(?1,?2,?3)",
        params![profile_id, account_id, timestamp],
    )
    .map_err(crate::db_error)?;
    with_primary(
        db,
        account_id,
        profile_json(profile_id, name, style, &seed, true),
    )
}
pub(crate) fn update_profile(
    db: &Connection,
    account_id: i64,
    profile_id: i64,
    value: &Value,
) -> Result<Value, ApiError> {
    validate_profile_payload(value, true)?;
    let current=db.query_row("SELECT p.name,p.avatar_style,p.avatar_seed,p.presentation_complete FROM profiles p JOIN profile_owners o ON o.profile_id=p.id WHERE p.id=?1 AND o.account_id=?2",params![profile_id,account_id],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,bool>(3)?))).optional().map_err(crate::db_error)?.ok_or_else(forbidden)?;
    if !current.3
        && (!value.get("name").is_some_and(Value::is_string)
            || !value.get("avatar_style").is_some_and(Value::is_string)
            || value.get("setup_complete") != Some(&Value::Bool(true)))
    {
        return Err(
            "Incomplete profile setup requires name, avatar_style, and setup_complete=true".into(),
        );
    }
    let name = match value.get("name") {
        Some(_) => crate::text(value, "name", 80)?.to_owned(),
        None => current.0,
    };
    let style = match value.get("avatar_style") {
        Some(style) => avatar_style(Some(style.as_str().ok_or("Invalid avatar_style")?))?,
        None => avatar_style(Some(&current.1))?,
    };
    let complete = current.3 || value.get("setup_complete") == Some(&Value::Bool(true));
    let seed = selected_avatar_seed(value, style, current.2)?;
    db.execute("UPDATE profiles SET name=?1,avatar_style=?2,presentation_complete=?3,updated_at=?4,avatar_seed=?6 WHERE id=?5",params![name,style,complete,now(),profile_id,seed]).map_err(crate::db_error)?;
    with_primary(
        db,
        account_id,
        profile_json(profile_id, &name, style, &seed, complete),
    )
}

pub(crate) fn dispatch_profiles(
    db: &Connection,
    path: &str,
    p: Option<Principal>,
    h: &HeaderMap,
    v: &Value,
    prepared: &PreparedAuth,
) -> Result<(Value, Option<(String, String)>), ApiError> {
    match path {
        "/auth/setup" => {
            let principal = p.as_ref().ok_or_else(unauthorized)?;
            let id = principal.account_id().ok_or_else(forbidden)?;
            if !v.is_object() || v.to_string().len() > 4096 {
                return Err("Invalid onboarding state".into());
            }
            let mut state = db
                .query_row(
                    "SELECT state FROM auth_onboarding WHERE account_id=?1",
                    [id],
                    |r| r.get::<_, String>(0),
                )
                .optional()
                .map_err(crate::db_error)?
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .unwrap_or(json!({}));
            for (k, value) in v.as_object().unwrap() {
                if ![
                    "household_name",
                    "timezone",
                    "language",
                    "completed",
                    "step",
                    "profile_id",
                ]
                .contains(&k.as_str())
                {
                    return Err("Unknown onboarding field".into());
                }
                match k.as_str() {
                    "household_name"
                        if !value
                            .as_str()
                            .is_some_and(|text| !text.trim().is_empty() && text.len() <= 100) =>
                    {
                        return Err("Invalid household_name".into())
                    }
                    "timezone"
                        if !value
                            .as_str()
                            .is_some_and(|text| !text.trim().is_empty() && text.len() <= 100) =>
                    {
                        return Err("Invalid timezone".into())
                    }
                    "language"
                        if !value
                            .as_str()
                            .is_some_and(|text| !text.trim().is_empty() && text.len() <= 35) =>
                    {
                        return Err("Invalid language".into())
                    }
                    "completed" if !value.is_boolean() => return Err("Invalid completed".into()),
                    "step" if !value.as_str().is_some_and(|text| text.len() <= 80) => {
                        return Err("Invalid step".into())
                    }
                    "profile_id"
                        if value
                            .as_i64()
                            .or_else(|| value.as_str()?.parse::<i64>().ok())
                            .is_none() =>
                    {
                        return Err("Invalid profile_id".into())
                    }
                    _ => {}
                }
                state[k] = value.clone();
            }
            db.execute("INSERT INTO auth_onboarding VALUES(?1,?2) ON CONFLICT(account_id) DO UPDATE SET state=excluded.state",params![id,state.to_string()]).map_err(crate::db_error)?;
            Ok((state, None))
        }
        "/auth/sessions" => {
            let principal = p.as_ref().ok_or_else(unauthorized)?;
            let id = principal.account_id().ok_or_else(forbidden)?;
            db.execute(
                "DELETE FROM auth_sessions WHERE account_id=?1 AND kind='browser'",
                [id],
            )
            .map_err(crate::db_error)?;
            event(db, "sessions_revoked", Some(id))?;
            Ok((json!({"ok":true}), Some((String::new(), String::new()))))
        }
        "/auth/logout" => {
            let t = bearer(h)
                .or_else(|| cookie(h, "viptv_session"))
                .ok_or_else(unauthorized)?;
            db.execute("DELETE FROM auth_sessions WHERE access_hash=?1", [hash(t)])
                .map_err(crate::db_error)?;
            Ok((json!({"ok":true}), Some((String::new(), String::new()))))
        }
        "/auth/profile" => {
            let principal = p.as_ref().ok_or_else(unauthorized)?;
            let profile = identifier(v, "profile_id")?;
            principal.require_profile(db, profile)?;
            crate::kids::switch_profile(db, principal, profile)?;
            let t = bearer(h)
                .or_else(|| cookie(h, "viptv_session"))
                .ok_or_else(unauthorized)?;
            db.execute(
                "UPDATE auth_sessions SET profile_id=?1 WHERE access_hash=?2",
                params![profile, hash(t)],
            )
            .map_err(crate::db_error)?;
            Ok((json!({"profile_id":profile.to_string()}), None))
        }
        _ => dispatch_devices(db, path, p, h, v, prepared),
    }
}
