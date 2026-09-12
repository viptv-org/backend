use futures::StreamExt;
use serde_json::Value;
use url::Url;

pub fn validate_url(raw: &str) -> Result<Url, String> {
    let u = Url::parse(raw).map_err(|_| "Invalid HTTP URL".to_string())?;
    if !matches!(u.scheme(), "http" | "https")
        || u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.fragment().is_some()
    {
        return Err("Only HTTP(S) URLs without userinfo or fragments are supported".into());
    }
    Ok(u)
}
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
pub async fn json_get(client: &reqwest::Client, url: &str) -> Result<Value, String> {
    validate_url(url)?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|_| "Upstream request failed".to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "Upstream returned HTTP {}",
            response.status().as_u16()
        ));
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "Upstream body failed".to_string())?;
        if body.len() + chunk.len() > 32 * 1024 * 1024 {
            return Err("Upstream response exceeds size limit".into());
        }
        body.extend_from_slice(&chunk);
    }
    tokio::task::spawn_blocking(move || {
        serde_json::from_slice(&body).map_err(|_| "Upstream returned invalid JSON".to_string())
    })
    .await
    .map_err(|_| "Upstream parser stopped".to_string())?
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_protocols_and_userinfo() {
        for u in [
            "file:///etc/passwd",
            "concat:http://a|http://b",
            "http://user:secret@host/a",
            "ftp://host/a",
        ] {
            assert!(validate_url(u).is_err());
        }
        assert!(validate_url("http://192.168.1.2:8080/a").is_ok());
    }
}
