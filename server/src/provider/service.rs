use super::*;

#[derive(Clone)]
pub struct ProviderService {
    pub db: Arc<Mutex<Connection>>,
    pub client: reqwest::Client,
    pub semaphore: Arc<Semaphore>,
    pub(crate) account: Option<i64>,
    pub(crate) vault: Option<Arc<crate::secret_store::Vault>>,
    #[cfg(test)]
    pub(crate) allow_test_loopback: bool,
    pub(super) playback_gates: Arc<Mutex<HashMap<i64, PlaybackGate>>>,
}
pub(super) struct PlaybackGate {
    pub(super) semaphore: Arc<Semaphore>,
}

#[derive(Clone)]
pub(super) struct Provider {
    pub(super) id: i64,
    pub(super) name: String,
    pub(super) url: String,
    pub(super) username: String,
    pub(super) password: String,
    pub(super) sealed: Option<(i64, String)>,
}

impl Provider {
    pub(super) fn ensure_current(&self, db: &Connection) -> Result<(), String> {
        if let Some((account, envelope)) = &self.sealed {
            let current:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM providers p JOIN provider_credentials_v2 c ON c.provider_id=p.id JOIN provider_ownership o ON o.provider_id=p.id WHERE p.id=?1 AND p.enabled=1 AND p.credentials_version=1 AND p.url='' AND p.username='' AND p.password='' AND c.account_id=?2 AND o.account_id=?2 AND c.secret=?3)",params![self.id,account,envelope],|r|r.get(0)).map_err(db_error)?;
            return if current {
                Ok(())
            } else {
                Err("source_configuration_changed".into())
            };
        }
        if credentials_v2::sealed(db, self.id)? {
            return Err("source_configuration_changed".into());
        }
        let current:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM providers WHERE id=?1 AND url=?2 AND username=?3 AND password=?4 AND enabled=1)",params![self.id,self.url,self.username,self.password],|r|r.get(0)).map_err(db_error)?;
        if !current {
            return Err(
                "Provider credentials changed or account became unavailable; retry the request"
                    .into(),
            );
        }
        Ok(())
    }
}

// Single enabled-provider row mapping, shared by locked and already-locked callers.
pub(super) fn provider_row(
    db: &Connection,
    id: i64,
    vault: Option<&crate::secret_store::Vault>,
) -> Result<Provider, String> {
    let mut provider = db
        .query_row(
            "SELECT id,name,url,username,password FROM providers WHERE id=?1 AND enabled=1",
            [id],
            |r| {
                Ok(Provider {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    url: r.get(2)?,
                    username: r.get(3)?,
                    password: r.get(4)?,
                    sealed: None,
                })
            },
        )
        .map_err(|_| "Provider not found or disabled".to_string())?;
    credentials_v2::read(db, vault, &mut provider)?;
    Ok(provider)
}

impl ProviderService {
    #[cfg(test)]
    pub(crate) fn add(&self, value: Value) -> Result<Value, String> {
        let db = self.lock()?;
        db.execute("INSERT INTO providers(name,url,username,password,enabled,max_connections,enable_live,enable_movies,enable_series) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![value["name"].as_str().unwrap_or("Fixture"),value["url"].as_str().unwrap_or("https://fixture.invalid"),value["username"].as_str().unwrap_or("fixture"),value["password"].as_str().unwrap_or("fixture"),value["enabled"].as_bool().unwrap_or(true),value["max_connections"].as_i64().unwrap_or(1),value["enable_live"].as_bool().unwrap_or(true),value["enable_movies"].as_bool().unwrap_or(true),value["enable_series"].as_bool().unwrap_or(true)]).map_err(db_error)?;
        Ok(json!({"id":db.last_insert_rowid()}))
    }
    #[cfg(test)]
    pub(crate) fn update(&self, id: i64, value: Value) -> Result<Value, String> {
        let db = self.lock()?;
        if credentials_v2::sealed(&db, id)? {
            return Err("client_update_required".into());
        }
        for name in ["enabled", "enable_live", "enable_movies", "enable_series"] {
            if let Some(value) = value[name].as_bool() {
                db.execute(
                    &format!("UPDATE providers SET {name}=?2 WHERE id=?1"),
                    params![id, value],
                )
                .map_err(db_error)?;
            }
        }
        Ok(json!({"id":id}))
    }
    pub fn new(db: Arc<Mutex<Connection>>, client: reqwest::Client) -> Self {
        Self {
            db,
            client,
            semaphore: Arc::new(Semaphore::new(4)),
            account: None,
            vault: None,
            #[cfg(test)]
            allow_test_loopback: false,
            playback_gates: Default::default(),
        }
    }

    pub(crate) fn for_account(&self, account: i64) -> Self {
        let mut service = self.clone();
        service.account = Some(account);
        service
    }

    pub(crate) fn require_owner(&self, db: &Connection, provider: i64) -> Result<(), String> {
        if let Some(account) = self.account {
            let allowed: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM provider_ownership WHERE provider_id=?1 AND account_id=?2)",params![provider,account],|r|r.get(0)).map_err(db_error)?;
            if !allowed {
                return Err("Provider unavailable in this account".into());
            }
        }
        Ok(())
    }

    // Only an internal integer principal is interpolated, never request SQL/text.
    pub(super) fn ownership_predicate(&self) -> String {
        self.account.map_or_else(String::new, |account|format!(" AND EXISTS(SELECT 1 FROM provider_ownership own WHERE own.provider_id=p.id AND own.account_id={account}) "))
    }

    pub async fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce(Self) -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        let service = self.clone();
        tokio::task::spawn_blocking(move || work(service))
            .await
            .map_err(|_| "Provider database worker stopped".to_string())?
    }

    pub(super) fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, String> {
        self.db
            .lock()
            .map_err(|_| "Provider database is unavailable".into())
    }

    pub(super) fn provider(&self, id: i64) -> Result<Provider, String> {
        let db = self.lock()?;
        self.require_owner(&db, id)?;
        provider_row(&db, id, self.vault.as_deref())
    }

    pub(super) fn scopes(&self, id: i64) -> Result<[bool; 3], String> {
        self.lock()?.query_row("SELECT enable_live,enable_movies,enable_series FROM providers WHERE id=?1 AND enabled=1", [id], |r| Ok([r.get(0)?,r.get(1)?,r.get(2)?])).map_err(|_| "Provider not found or disabled".into())
    }

    pub(super) fn provider_for_kind(&self, id: i64, kind: &str) -> Result<Provider, String> {
        let db = self.lock()?;
        self.require_owner(&db, id)?;
        let allowed:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM providers WHERE id=?1 AND enabled=1 AND CASE ?2 WHEN 'live' THEN enable_live WHEN 'movie' THEN enable_movies WHEN 'series' THEN enable_series ELSE 0 END=1)",params![id,kind],|r|r.get(0)).map_err(db_error)?;
        if !allowed {
            return Err("Provider not found or content scope disabled".into());
        }
        provider_row(&db, id, self.vault.as_deref())
    }
}
