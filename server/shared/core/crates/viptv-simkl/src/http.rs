use serde_json::Value;
use std::{sync::Arc, time::Duration};
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct Client {
    client: reqwest::Client,
    pub client_id: String,
    pub secret: String,
    api: String,
    cdn: String,
    gate: Arc<Mutex<tokio::time::Instant>>,
}
#[derive(Debug, Clone)]
pub struct Error {
    pub status: u16,
    pub code: String,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SIMKL {} ({})", self.code, self.status)
    }
}
impl std::error::Error for Error {}
impl Client {
    pub fn new(client_id: String, secret: String) -> Result<Self, Error> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("viptv-simkl/0.1")
            .build()
            .map_err(|_| Error {
                status: 503,
                code: "client_unavailable".into(),
            })?;
        Ok(Self {
            client,
            client_id,
            secret,
            api: "https://api.simkl.com".into(),
            cdn: "https://data.simkl.in".into(),
            gate: Arc::new(Mutex::new(tokio::time::Instant::now())),
        })
    }
    /// Explicit injected transport for in-process integration fixtures.
    pub fn with_origins(mut self, api: String, cdn: String) -> Self {
        self.api = api;
        self.cdn = cdn;
        self
    }
    pub async fn get(&self, path: &str, token: Option<&str>) -> Result<Value, Error> {
        self.request("GET", path, token, None).await
    }
    pub async fn post(&self, path: &str, token: &str, body: Value) -> Result<Value, Error> {
        self.request("POST", path, Some(token), Some(body)).await
    }
    pub async fn request(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> Result<Value, Error> {
        let cdn = path.starts_with("/discover/") || path.starts_with("/calendar/");
        let mut url = url::Url::parse(&format!(
            "{}{path}",
            if cdn { &self.cdn } else { &self.api }
        ))
        .map_err(|_| Error {
            status: 400,
            code: "invalid_path".into(),
        })?;
        url.query_pairs_mut()
            .append_pair("client_id", &self.client_id)
            .append_pair("app-name", "viptv")
            .append_pair("app-version", "0.1");
        // Serialize and pace uncached requests. Cached catalog/CDN GETs are exempt.
        let parts: Vec<_> = path.split('?').next().unwrap_or(path).split('/').filter(|part| !part.is_empty()).collect();
        let cached_detail = method == "GET" && parts.first().is_some_and(|part| ["movies", "tv", "anime"].contains(part))
            && ((parts.len() == 2 && parts[1].parse::<u64>().is_ok())
                || (parts.len() == 3 && parts[1] == "episodes" && parts[2].parse::<u64>().is_ok()));
        // SIMKL explicitly allows parallel CDN and cached title-detail reads.
        // They must not queue behind a user's potentially large library sync.
        let mut gate = if cdn || cached_detail { None } else { Some(self.gate.lock().await) };
        if let Some(deadline) = gate.as_ref() {
            tokio::time::sleep_until(**deadline).await;
        }
        let mut attempts = 0;
        loop {
            let mut req = self
                .client
                .request(method.parse().unwrap_or(reqwest::Method::GET), url.clone());
            if let Some(t) = token {
                req = req.bearer_auth(t);
            }
            if let Some(v) = &body {
                req = req.json(v);
            }
            let response = req.send().await.map_err(|_| Error {
                status: 503,
                code: "network_unavailable".into(),
            })?;
            let status = response.status().as_u16();
            let retry = response
                .headers()
                .get("retry-after")
                .and_then(|s| s.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok());
            let data: Value = response.json().await.unwrap_or(Value::Null);
            if let Some(deadline) = gate.as_mut() {
                **deadline = tokio::time::Instant::now()
                    + Duration::from_millis(if method == "GET" { 100 } else { 1000 });
            }
            if (200..300).contains(&status) {
                return Ok(data);
            }
            let code = data["error"]
                .as_str()
                .unwrap_or("upstream_error")
                .to_owned();
            // Scrobbles are retried on the next real player event, not on a timer.
            if !path.starts_with("/scrobble/")
                && (matches!(status, 429 | 500 | 502 | 503)
                    || (status == 400 && code == "rate_limit"))
                && attempts < 4
            {
                attempts += 1;
                tokio::time::sleep(Duration::from_secs(retry.unwrap_or(1 << attempts).min(60)))
                    .await;
                continue;
            }
            return Err(Error { status, code });
        }
    }
    pub async fn token(&self, fields: &[(&str, String)]) -> Result<Value, Error> {
        let mut form = fields.to_vec();
        form.push(("client_id", self.client_id.clone()));
        form.push(("client_secret", self.secret.clone()));
        let _gate = self.gate.lock().await;
        let response = self
            .client
            .post(format!("{}/oauth2/token", self.api))
            .form(&form)
            .send()
            .await
            .map_err(|_| Error {
                status: 503,
                code: "token_unavailable".into(),
            })?;
        let status = response.status().as_u16();
        let v: Value = response.json().await.unwrap_or(Value::Null);
        if (200..300).contains(&status) {
            Ok(v)
        } else {
            Err(Error {
                status,
                code: v["error"].as_str().unwrap_or("token_failed").into(),
            })
        }
    }
    pub async fn revoke(&self, token: &str) -> Result<(), Error> {
        let response = self
            .client
            .post(format!("{}/oauth2/revoke", self.api))
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.secret.as_str()),
                ("token", token),
            ])
            .send()
            .await
            .map_err(|_| Error {
                status: 503,
                code: "revoke_unavailable".into(),
            })?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(Error {
                status: response.status().as_u16(),
                code: "revoke_failed".into(),
            })
        }
    }
    pub async fn resolve(
        &self,
        ids: &[(&str, String)],
        token: &str,
    ) -> Result<(String, u64), Error> {
        let mut url = url::Url::parse(&format!("{}/redirect", self.api)).unwrap();
        url.query_pairs_mut()
            .append_pair("to", "simkl")
            .append_pair("client_id", &self.client_id)
            .append_pair("app-name", "viptv")
            .append_pair("app-version", "0.1")
            .extend_pairs(ids.iter().map(|(k, v)| (*k, v.as_str())));
        let _gate = self.gate.lock().await;
        let response = self
            .client
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| Error {
                status: 503,
                code: "resolve_unavailable".into(),
            })?;
        let location = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| url::Url::parse(s).ok())
            .ok_or(Error {
                status: 404,
                code: "mapping_unavailable".into(),
            })?;
        if location.host_str() != Some("simkl.com") {
            return Err(Error {
                status: 404,
                code: "mapping_unavailable".into(),
            });
        }
        let p: Vec<_> = location.path_segments().unwrap().collect();
        let n = p.get(1).and_then(|s| s.parse().ok()).ok_or(Error {
            status: 404,
            code: "mapping_unavailable".into(),
        })?;
        Ok((p[0].into(), n))
    }
}
