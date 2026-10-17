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
    // A search deadline may drop its caller after the provider consumed the
    // refresh token. Own the bounded exchange+seal transaction independently;
    // dropping the JoinHandle leaves that transaction running to completion.
    let app = app.clone();
    let capability = credential.capability;
    let revision = credential.revision;
    let configuration_revision = credential.configuration_revision;
    let parent_scope = credential.parent_scope.clone();
    tokio::spawn(async move {
        let _gate = tokio::time::timeout(Duration::from_secs(15), app.simkl.refresh_gate.lock())
            .await
            .map_err(|_| {
                ApiError(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Simkl refresh is busy".into(),
                )
            })?;
        let current = store::credential_access(&app, profile, capability, parent_scope.as_ref())?;
        if current.configuration_revision != configuration_revision {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "Profile configuration changed".into(),
            ));
        }
        if current.revision != revision {
            return Err(ApiError(StatusCode::CONFLICT, "Simkl link changed".into()));
        }
        tokio::time::timeout(
            Duration::from_secs(20),
            refresh_locked(&app, profile, &current),
        )
        .await
        .map_err(|_| {
            ApiError(
                StatusCode::GATEWAY_TIMEOUT,
                "Simkl refresh timed out".into(),
            )
        })?
    })
    .await
    .map_err(|_| unavailable())?
}

async fn refresh_locked(
    app: &App,
    profile: i64,
    credential: &store::Credential,
) -> Result<Option<Zeroizing<String>>, ApiError> {
    store::validate(app, profile, credential)?;
    let current = store::reload_credential(app, profile, credential)?;
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
    let rotation = Rotation::capture(app, profile, &current)?;
    let refresh = token["refresh_token"].as_str().ok_or_else(unavailable)?;
    let config = app.simkl.config.as_ref().ok_or_else(unavailable)?;
    let response=app.simkl.request_checked("/oauth2/token",None,Some(json!({"grant_type":"refresh_token","refresh_token":refresh,"client_id":config.client_id,"client_secret":config.secret.as_str()})),||{store::validate(app,profile,credential)?;rotation.validate_app(app)}).await?;
    let document = document(&response)?;
    rotation.validate_app(app)?;
    let account = rotation.account;
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
    rotation.validate(&db)?;
    let changed=db.execute("UPDATE simkl_links SET credential=?4 WHERE profile_id=?1 AND account_id=?2 AND revision=?3 AND credential=?5",params![profile,account,rotation.revision,sealed,rotation.sealed]).map_err(db_error)?;
    if changed != 1 {
        return Err(ApiError(StatusCode::CONFLICT, "Simkl link changed".into()));
    }
    Ok(response["access_token"]
        .as_str()
        .map(|v| Zeroizing::new(v.into())))
}

/// A consumed refresh belongs to the persistent link, not the caller's temporary
/// viewing grant. Exact ciphertext CAS prevents unlink/relink or numeric-ID reuse
/// from acquiring a response authorized for a different credential incarnation.
struct Rotation {
    account: i64,
    profile: i64,
    revision: u64,
    sealed: String,
}
impl Rotation {
    fn capture(app: &App, profile: i64, credential: &store::Credential) -> Result<Self, ApiError> {
        let db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        store::require_scope(&db, app, profile, credential)?;
        let account = app.identity().account_id().ok_or_else(auth::forbidden)?;
        let(revision,sealed):(u64,String)=db.query_row("SELECT revision,credential FROM simkl_links WHERE profile_id=?1 AND account_id=?2 AND credential IS NOT NULL",params![profile,account],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(db_error)?.ok_or_else(||ApiError(StatusCode::CONFLICT,"Simkl link changed".into()))?;
        if Some(revision) != credential.revision {
            return Err(ApiError(StatusCode::CONFLICT, "Simkl link changed".into()));
        }
        let current = app
            .secret_vault
            .as_ref()
            .ok_or_else(unavailable)?
            .open(account, "simkl", &profile.to_string(), &sealed)
            .map_err(|_| unavailable())?;
        if credential
            .token
            .as_ref()
            .is_none_or(|token| token.expose() != current.expose())
        {
            return Err(ApiError(StatusCode::CONFLICT, "Simkl link changed".into()));
        }
        Ok(Self {
            account,
            profile,
            revision,
            sealed,
        })
    }
    fn validate(&self, db: &Connection) -> Result<(), ApiError> {
        let current:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM simkl_links s JOIN profiles p ON p.id=s.profile_id JOIN profile_owners o ON o.profile_id=p.id AND o.account_id=s.account_id JOIN auth_accounts a ON a.id=s.account_id WHERE s.profile_id=?1 AND s.account_id=?2 AND s.revision=?3 AND s.credential=?4)",params![self.profile,self.account,self.revision,self.sealed],|r|r.get(0)).map_err(db_error)?;
        if !current {
            return Err(ApiError(StatusCode::CONFLICT, "Simkl link changed".into()));
        }
        Ok(())
    }
    fn validate_app(&self, app: &App) -> Result<(), ApiError> {
        self.validate(
            &*app
                .db
                .lock()
                .map_err(|_| ApiError::from("Database unavailable"))?,
        )
    }
}
