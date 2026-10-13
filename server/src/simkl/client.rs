use super::*;
use zeroize::Zeroizing;

pub(super) struct Config {
    pub client_id: String,
    pub secret: Zeroizing<String>,
    pub redirect: String,
}

pub(crate) struct Service {
    pub(super) config: Option<Config>,
    client: reqwest::Client,
    base: String,
    pub(super) data_base: String,
    pub(super) trending: super::discovery::PublicCache,
    pub(super) public_documents: tokio::sync::Mutex<std::collections::HashMap<String,(Instant,Arc<Value>)>>,
    pub(super) metadata: super::cache::Cache,
    pub(super) tracking: super::automatic::Queue,
    next: tokio::sync::Mutex<tokio::time::Instant>,
    pub(super) refresh_gate: tokio::sync::Mutex<()>,
}

impl Service {
    pub(crate) fn configured(&self) -> bool {
        self.config.is_some()
    }
    pub(crate) fn from_env() -> Result<Self, String> {
        let client_id = std::env::var("SIMKL_CLIENT_ID")
            .ok()
            .filter(|v| !v.trim().is_empty());
        let secret = std::env::var("SIMKL_CLIENT_SECRET")
            .ok()
            .filter(|v| !v.trim().is_empty());
        let config = match (client_id, secret) {
            (Some(client_id), Some(secret)) => {
                if client_id.len() > 512
                    || secret.len() > 512
                    || client_id.chars().any(char::is_control)
                    || secret.chars().any(char::is_control)
                {
                    return Err("Invalid Simkl application configuration".into());
                }
                let origin = auth::canonical_origin()
                    .map_err(|_| "Invalid Simkl callback origin")?
                    .ok_or("A pinned HTTPS origin is required for Simkl")?;
                let redirect = origin
                    .join("/api/v3/simkl/callback")
                    .map_err(|_| "Invalid Simkl callback origin")?
                    .to_string();
                Some(Config {
                    client_id,
                    secret: Zeroizing::new(secret),
                    redirect,
                })
            }
            _ => None,
        };
        Self::new(config, "https://api.simkl.com")
    }

    fn new(config: Option<Config>, base: &str) -> Result<Self, String> {
        Ok(Self {
            config,
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_secs(10))
                .user_agent(concat!("VIPTV/", env!("CARGO_PKG_VERSION")))
                .build()
                .map_err(|_| "Simkl HTTP client unavailable")?,
            base: base.into(),
            data_base: "https://data.simkl.in".into(),
            trending: Default::default(),
            public_documents: Default::default(),
            metadata: Default::default(),
            tracking: Default::default(),
            next: tokio::sync::Mutex::new(tokio::time::Instant::now()),
            refresh_gate: tokio::sync::Mutex::new(()),
        })
    }

    pub(super) async fn request_checked(
        &self,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
        check: impl FnOnce() -> Result<(), ApiError>,
    ) -> Result<Value, ApiError> {
        let config = self.config.as_ref().ok_or_else(unavailable)?;
        if !path.starts_with('/') || path.starts_with("//") {
            return Err("Invalid Simkl request".into());
        }
        {
            let mut next = self.next.lock().await;
            tokio::time::sleep_until(*next).await;
            *next = tokio::time::Instant::now() + Duration::from_millis(125);
        }
        check()?;
        let url = format!("{}{path}", self.base);
        let mut request = if let Some(body) = body {
            if path == "/oauth2/token" {
                self.client.post(url).form(&body)
            } else {
                self.client.post(url).json(&body)
            }
        } else {
            self.client.get(url)
        }
        .header("simkl-api-key", &config.client_id)
        .header("app-name", "viptv")
        .header("app-version", env!("CARGO_PKG_VERSION"));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        let mut response = request
            .send()
            .await
            .map_err(|_| ApiError(StatusCode::BAD_GATEWAY, "Simkl is unavailable".into()))?;
        if !response.status().is_success() {
            let authorization_required = matches!(response.status().as_u16(), 401 | 403);
            let status = match response.status().as_u16() {
                401 | 403 => StatusCode::BAD_GATEWAY,
                409 => StatusCode::CONFLICT,
                429 => StatusCode::TOO_MANY_REQUESTS,
                _ => StatusCode::BAD_GATEWAY,
            };
            return Err(ApiError(
                status,
                if authorization_required {
                    "Simkl authorization is required"
                } else {
                    "Simkl request was not accepted"
                }
                .into(),
            ));
        }
        let mut bytes = Zeroizing::new(Vec::new());
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| ApiError::from("Simkl response interrupted"))?
        {
            if bytes.len() + chunk.len() > 4 * 1024 * 1024 {
                return Err("Simkl response exceeds the limit".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| ApiError(StatusCode::BAD_GATEWAY, "Simkl response was invalid".into()))
    }

    #[cfg(test)]
    pub(super) fn fixture(base: &str) -> Self {
        let mut service = Self::new(
            Some(Config {
                client_id: "fixture-client".into(),
                secret: Zeroizing::new("fixture-secret".into()),
                redirect: "https://tv.example/api/v3/simkl/callback".into(),
            }),
            base,
        )
        .unwrap();
        service.data_base = base.into();
        service
    }
}
