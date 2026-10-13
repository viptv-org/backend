//! OAuth V2 token lifetimes and serialized refresh; all token material stays sealed.
use super::*;
use zeroize::Zeroizing;

pub(super) fn document(response: &Value) -> Result<Zeroizing<String>, ApiError> {
    let valid = |key: &str| {
        response[key]
            .as_str()
            .is_some_and(|s| !s.is_empty() && s.len() <= 2048 && !s.chars().any(char::is_control))
    };
    let scope = response["scope"].as_str().unwrap_or("");
    let expires = response["expires_in"]
        .as_i64()
        .filter(|n| *n > 0 && *n <= 31_536_000)
        .ok_or("Invalid Simkl token expiry")?;
    if !valid("access_token")
        || !valid("refresh_token")
        || !response["token_type"]
            .as_str()
            .is_some_and(|v| v.eq_ignore_ascii_case("bearer"))
        || !["media:read", "media:write"]
            .iter()
            .all(|required| scope.split_whitespace().any(|s| s == *required))
    {
        return Err("Simkl did not grant the required authorization".into());
    }
    Ok(Zeroizing::new(json!({"access_token":response["access_token"],"refresh_token":response["refresh_token"],"scope":scope,"expires_at":util::now().saturating_add(expires)}).to_string()))
}

pub(super) async fn access(
    app: &App,
    profile: i64,
    credential: &store::Credential,
) -> Result<Option<Zeroizing<String>>, ApiError> {
    let Some(raw) = credential.token.as_ref() else {
        return Ok(None);
    };
    let text = std::str::from_utf8(raw.expose()).map_err(|_| unavailable())?;
    let Ok(token) = serde_json::from_str::<Value>(text) else {
        // Existing links remain readable; new links always contain a V2 document.
        return Ok(Some(Zeroizing::new(text.to_owned())));
    };
    if token["expires_at"]
        .as_i64()
        .is_some_and(|at| at > util::now() + 60)
    {
        return Ok(token["access_token"]
            .as_str()
            .map(|v| Zeroizing::new(v.into())));
    }
    let _gate = app.simkl.refresh_gate.lock().await;
    store::validate(app, profile, credential)?;
    let current = store::credential(app, profile, credential.capability)?;
    let bytes = current.token.as_ref().ok_or_else(unavailable)?;
    let token: Value = serde_json::from_slice(bytes.expose()).map_err(|_| unavailable())?;
    if token["expires_at"]
        .as_i64()
        .is_some_and(|at| at > util::now() + 60)
    {
        return Ok(token["access_token"]
            .as_str()
            .map(|v| Zeroizing::new(v.into())));
    }
    let refresh = token["refresh_token"].as_str().ok_or_else(unavailable)?;
    let config = app.simkl.config.as_ref().ok_or_else(unavailable)?;
    let response=app.simkl.request_checked("/oauth2/token",None,Some(json!({"grant_type":"refresh_token","refresh_token":refresh,"client_id":config.client_id,"client_secret":config.secret.as_str()})),||store::validate(app,profile,credential)).await?;
    let document = document(&response)?;
    store::validate(app, profile, credential)?;
    let account = app.identity().account_id().ok_or_else(auth::forbidden)?;
    let sealed = app
        .secret_vault
        .as_ref()
        .ok_or_else(unavailable)?
        .seal(account, "simkl", &profile.to_string(), document.as_bytes())
        .map_err(|_| unavailable())?;
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_profile(&db, profile)?;
    let changed=db.execute("UPDATE simkl_links SET credential=?4 WHERE profile_id=?1 AND account_id=?2 AND revision=?3 AND credential IS NOT NULL",params![profile,account,credential.revision,sealed]).map_err(db_error)?;
    if changed != 1 {
        return Err(ApiError(StatusCode::CONFLICT, "Simkl link changed".into()));
    }
    Ok(response["access_token"]
        .as_str()
        .map(|v| Zeroizing::new(v.into())))
}
