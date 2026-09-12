//! Owner account onboarding. Validation is bounded per row; only row numbers,
//! provider IDs and closed outcome names cross back to the client.
use super::*;
use crate::{ApiError, ApiResult, ResourceLease};
use axum::{extract::State, Extension};

pub(crate) fn owner(lease: &ResourceLease, db: &Connection) -> Result<(), String> {
    lease
        .validate(db)
        .map_err(|_| "Account authorization expired")?;
    let crate::auth::Principal::Account { account_id, .. } = &lease.principal;
    active_owner(db, *account_id)
}
pub(crate) fn active_owner(db: &Connection, account_id: i64) -> Result<(), String> {
    let allowed: bool = db
        .query_row(
            "SELECT role='owner' AND disabled=0 FROM auth_accounts WHERE id=?1",
            [account_id],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    if !allowed {
        return Err("Owner access required".into());
    }
    Ok(())
}
fn entries(value: &Value) -> Result<Vec<Value>, String> {
    let values = match &value["entries"] {
        Value::Array(values) => values.clone(),
        Value::String(text) if text.len() <= 64 * 1024 => {
            if text.trim_start().starts_with('[') {
                serde_json::from_str::<Vec<Value>>(text).map_err(|_| "Invalid account JSON")?
            } else {
                text.lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(|line| Value::String(line.into()))
                    .collect()
            }
        }
        _ => return Err("Enter an account array or one Xtream URL per line".into()),
    };
    if values.is_empty() || values.len() > 20 || value.to_string().len() > 64 * 1024 {
        return Err("Import between 1 and 20 accounts, up to 64 KiB per batch".into());
    }
    Ok(values)
}
fn parsed(value: Value, row: usize) -> Result<Value, String> {
    let mut value = if let Some(raw) = value.as_str() {
        let mut url = crate::util::validate_url(raw).map_err(|_| "Invalid Xtream URL")?;
        if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
            return Err("Invalid Xtream URL".into());
        }
        let mut query = HashMap::new();
        for (key, value) in url.query_pairs() {
            if query.insert(key.to_string(), value.to_string()).is_some() {
                return Err("Repeated Xtream URL parameter".into());
            }
        }
        let username = query.remove("username").ok_or("Missing Xtream username")?;
        let password = query.remove("password").ok_or("Missing Xtream password")?;
        let path = url
            .path()
            .strip_suffix("/get.php")
            .or_else(|| url.path().strip_suffix("/player_api.php"))
            .ok_or("Expected an Xtream get.php or player_api.php URL")?
            .to_owned();
        url.set_path(&path);
        url.set_query(None);
        json!({"url":url.as_str(),"username":username,"password":password})
    } else {
        value
    };
    let object = value
        .as_object_mut()
        .ok_or("Account row must be an object or Xtream URL")?;
    if object.keys().any(|key| {
        ![
            "name",
            "warp",
            "url",
            "username",
            "password",
            "enable_live",
            "enable_movies",
            "enable_series",
            "max_connections",
        ]
        .contains(&key.as_str())
    }) {
        return Err("Unknown account field".into());
    }
    object
        .entry("name")
        .or_insert(json!(format!("Xtream account {row}")));
    object.entry("enable_live").or_insert(json!(true));
    object.entry("enable_movies").or_insert(json!(false));
    object.entry("enable_series").or_insert(json!(false));
    for (field, max) in [("name", 200), ("username", 512), ("password", 2048)] {
        required_string(&value, field, max)?;
    }
    value["url"] = json!(base_url(&required_string(&value, "url", 4096)?)?
        .as_str()
        .trim_end_matches('/'));
    for field in ["enable_live", "enable_movies", "enable_series", "warp"] {
        optional_bool(&value, field)?;
    }
    if value
        .get("max_connections")
        .is_some_and(|n| n.as_u64().is_none_or(|n| !(1..=32).contains(&n)))
    {
        return Err("Invalid connection allowance".into());
    }
    Ok(value)
}
pub(super) fn duplicate(
    db: &Connection,
    value: &Value,
    except: Option<i64>,
) -> Result<Option<i64>, String> {
    let mut q = db
        .prepare("SELECT id,url,username FROM providers WHERE (?1 IS NULL OR id<>?1) ORDER BY id")
        .map_err(db_error)?;
    let rows = q
        .query_map([except], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(db_error)?;
    for row in rows {
        let (id, url, user) = row.map_err(db_error)?;
        if user == value["username"].as_str().unwrap_or("")
            && base_url(&url)?.as_str().trim_end_matches('/') == value["url"].as_str().unwrap_or("")
        {
            return Ok(Some(id));
        }
    }
    Ok(None)
}
pub(super) async fn login_report(
    service: &ProviderService,
    value: &Value,
) -> Result<Value, String> {
    tokio::time::timeout(Duration::from_secs(8), async {
        let _permit = service
            .semaphore
            .acquire()
            .await
            .map_err(|_| "Provider service unavailable")?;
        let proxy = if value["warp"] == true {
            Some(super::egress::configured()?)
        } else {
            None
        };
        let client = super::egress::builder(
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(3)),
            proxy.as_deref(),
        )?
        .build()
        .map_err(|_| "Validation unavailable")?;
        let mut url = base_url(value["url"].as_str().unwrap())?
            .join("player_api.php")
            .map_err(|_| "Invalid provider URL")?;
        url.query_pairs_mut()
            .append_pair("username", value["username"].as_str().unwrap())
            .append_pair("password", value["password"].as_str().unwrap());
        let mut response = client
            .get(url)
            .send()
            .await
            .map_err(|_| "Account validation failed")?;
        if !response.status().is_success() {
            return Err(format!(
                "Provider returned HTTP {}",
                response.status().as_u16()
            ));
        }
        if response.content_length().is_some_and(|n| n > 256 * 1024) {
            return Err("Account validation failed".into());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "Account validation failed")?
        {
            if bytes.len() + chunk.len() > 256 * 1024 {
                return Err("Account validation failed".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let data: Value =
            serde_json::from_slice(&bytes).map_err(|_| "Account validation failed")?;
        let user = &data["user_info"];
        let expiry = user["exp_date"].as_i64().or_else(|| {
            user["exp_date"]
                .as_str()
                .and_then(|s| s.parse::<i64>().ok())
        });
        if expiry.is_some_and(|at| at > 0 && at <= crate::util::now())
            || !(user["auth"] == 1 || user["auth"] == "1" || user["auth"] == true)
            || user
                .get("status")
                .is_some_and(|s| s.as_str().is_none_or(|s| !s.eq_ignore_ascii_case("active")))
        {
            return Err("Account credentials expired or rejected".into());
        }
        Ok(user.clone())
    })
    .await
    .map_err(|_| "Account validation timed out")?
}

pub(crate) async fn import(
    State(a): State<crate::App>,
    Extension(lease): Extension<ResourceLease>,
    axum::Json(value): axum::Json<Value>,
) -> ApiResult {
    let values = entries(&value)?;
    let results=stream::iter(values.into_iter().enumerate().map(|(index,value)| {
        let service=a.providers.clone();let lease=lease.clone();
        async move {
            let row=index+1;
            let Ok(value)=parsed(value,row) else {return json!({"row":row,"status":"invalid_entry"});};
            let checking=value.clone();let auth=lease.clone();
            match service.blocking(move |s| {let db=s.lock()?;owner(&auth,&db)?;duplicate(&db,&checking,None)}).await {
                Ok(Some(id))=>return json!({"row":row,"status":"duplicate","provider_id":id}),
                Err(_)=>return json!({"row":row,"status":"save_failed"}),
                Ok(None)=>{},
            }
            if login_report(&service,&value).await.is_err() {return json!({"row":row,"status":"validation_failed"});}
            let result=service.blocking(move |s| {
                let db=s.lock()?;owner(&lease,&db)?;
                if let Some(id)=duplicate(&db,&value,None)? {return Ok((id,false));}
                db.execute("INSERT INTO providers(name,url,username,password,max_connections,enable_live,enable_movies,enable_series) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![value["name"].as_str(),value["url"].as_str(),value["username"].as_str(),value["password"].as_str(),value["max_connections"].as_i64().unwrap_or(1),value["enable_live"].as_bool(),value["enable_movies"].as_bool(),value["enable_series"].as_bool()]).map_err(db_error)?;
                Ok((db.last_insert_rowid(),true))
            }).await;
            match result { Ok((id,created))=>json!({"row":row,"status":if created {"imported"}else{"duplicate"},"provider_id":id}),Err(_)=>json!({"row":row,"status":"save_failed"}) }
        }
    })).buffer_unordered(4).collect::<Vec<_>>().await;
    let mut results = results;
    results.sort_by_key(|v| v["row"].as_u64());
    Ok(axum::Json(json!({"results":results})))
}

pub(crate) async fn renew(
    State(a): State<crate::App>,
    Extension(lease): Extension<ResourceLease>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    axum::Json(patch): axum::Json<Value>,
) -> ApiResult {
    let object = patch.as_object().ok_or("Credentials must be an object")?;
    if object
        .keys()
        .any(|key| !["url", "username", "password"].contains(&key.as_str()))
    {
        return Err("Renewal accepts URL, username and password only".into());
    }
    required_string(&patch, "password", 2048)?;
    let auth = lease.clone();
    let mut credentials=a.providers.blocking(move |s| {
        let db=s.lock()?;owner(&auth,&db)?;
        let (url,username):(String,String)=db.query_row("SELECT url,username FROM providers WHERE id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|_|"Provider not found")?;
        parsed(json!({"url":patch.get("url").cloned().unwrap_or(json!(url)),"username":patch.get("username").cloned().unwrap_or(json!(username)),"password":patch["password"]}),1)
    }).await?;
    credentials["warp"] = json!(super::egress::enabled(&a.db.lock().unwrap(), id));
    login_report(&a.providers, &credentials)
        .await
        .map_err(|_| ApiError::from("Account validation failed; existing credentials were kept"))?;
    let result = a
        .providers
        .blocking(move |s| {
            let mut db = s.lock()?;
            owner(&lease, &db)?;
            if duplicate(&db, &credentials, Some(id))?.is_some() {
                return Err("Provider login already configured on another entry".into());
            }
            let (old_url, old_user): (String, String) = db
                .query_row(
                    "SELECT url,username FROM providers WHERE id=?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(|_| "Provider not found")?;
            let changed_identity = base_url(&old_url)?.as_str().trim_end_matches('/')
                != credentials["url"].as_str().unwrap()
                || old_user != credentials["username"].as_str().unwrap();
            if changed_identity {
                let pool = super::pools::ensure(&db, id)?;
                let gates = s
                    .playback_gates
                    .lock()
                    .map_err(|_| "Account limiter unavailable")?;
                if super::pools::active(&gates, pool) > 0 {
                    return Err("Stop pool playback before changing the login identity".into());
                }
            }
            let tx = db.transaction().map_err(db_error)?;
            let changed = tx
                .execute(
                    "UPDATE providers SET url=?2,username=?3,password=?4 WHERE id=?1",
                    params![
                        id,
                        credentials["url"].as_str(),
                        credentials["username"].as_str(),
                        credentials["password"].as_str()
                    ],
                )
                .map_err(db_error)?;
            if changed == 0 {
                return Err("Provider not found".into());
            }
            tx.execute("DELETE FROM provider_cache WHERE provider_id=?1", [id])
                .map_err(db_error)?;
            tx.execute("DELETE FROM health_accounts WHERE provider_id=?1", [id])
                .map_err(db_error)?;
            tx.execute("DELETE FROM catalog_backoff WHERE provider_id=?1", [id])
                .map_err(db_error)?;
            tx.commit().map_err(db_error)?;
            drop(db);
            s.list()?
                .as_array()
                .and_then(|rows| rows.iter().find(|p| p["id"] == id))
                .cloned()
                .ok_or("Provider not found".into())
        })
        .await?;
    Ok(axum::Json(result))
}
