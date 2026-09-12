//! A provider's API, guide, media probe and playback share one explicit egress route.
use super::*;
pub(crate) const HEADER: &str = "x-viptv-egress-proxy";
pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS provider_routes(provider_id INTEGER PRIMARY KEY REFERENCES providers(id) ON DELETE CASCADE,warp INTEGER NOT NULL DEFAULT 0);")
}
pub(crate) fn enabled(db: &Connection, id: i64) -> bool {
    use rusqlite::OptionalExtension;
    db.query_row(
        "SELECT warp FROM provider_routes WHERE provider_id=?1",
        [id],
        |r| r.get(0),
    )
    .optional()
    .ok()
    .flatten()
    .unwrap_or(false)
}
pub(crate) fn configured() -> Result<String, String> {
    let raw = std::env::var("VIPTV_WARP_PROXY")
        .map_err(|_| "WARP proxy is not configured on this server")?;
    let url = Url::parse(&raw).map_err(|_| "Invalid server WARP proxy")?;
    if url.scheme() != "http"
        || url.host_str().is_none()
        || url.query().is_some()
        || url.fragment().is_some()
        || raw.chars().any(char::is_control)
    {
        return Err("Invalid server WARP proxy".into());
    }
    Ok(raw)
}
pub(crate) fn proxy(db: &Connection, id: i64) -> Result<Option<String>, String> {
    if enabled(db, id) {
        configured().map(Some)
    } else {
        Ok(None)
    }
}
pub(crate) fn builder(
    builder: reqwest::ClientBuilder,
    proxy: Option<&str>,
) -> Result<reqwest::ClientBuilder, String> {
    match proxy {
        Some(url) => Ok(builder
            .no_proxy()
            .proxy(reqwest::Proxy::all(url).map_err(|_| "Invalid WARP route")?)),
        None => Ok(builder.no_proxy()),
    }
}
pub(crate) fn set(db: &Connection, id: i64, warp: bool) -> Result<(), String> {
    if warp {
        configured()?;
    }
    let changed = enabled(db, id) != warp;
    db.execute("INSERT INTO provider_routes(provider_id,warp) VALUES(?1,?2) ON CONFLICT(provider_id) DO UPDATE SET warp=excluded.warp",params![id,warp]).map_err(db_error)?;
    db.execute("DELETE FROM provider_cache WHERE provider_id=?1", [id])
        .map_err(db_error)?;
    if changed {
        db.execute("DELETE FROM health_accounts WHERE provider_id=?1", [id])
            .map_err(db_error)?;
        db.execute("DELETE FROM catalog_backoff WHERE provider_id=?1", [id])
            .map_err(db_error)?;
        db.execute("UPDATE candidate_health SET next_check=0 WHERE live_id IN (SELECT id FROM provider_live WHERE provider_id=?1)", [id]).map_err(db_error)?;
    }
    Ok(())
}
pub(crate) fn headers(db: &Connection, id: i64) -> Result<HashMap<String, String>, String> {
    Ok(proxy(db, id)?
        .map(|p| HashMap::from([(HEADER.to_owned(), p)]))
        .unwrap_or_default())
}
pub(crate) fn candidate_headers(
    db: &Connection,
    id: &str,
) -> Result<HashMap<String, String>, String> {
    let provider = db
        .query_row(
            "SELECT provider_id FROM provider_live WHERE id=?1",
            [id],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    headers(db, provider)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn changing_route_retries_observations_without_clearing_owner_exclusions() {
        let db = Connection::open_in_memory().unwrap();
        crate::provider::init(&db).unwrap();
        crate::automation::init(&db).unwrap();
        db.execute_batch("INSERT INTO providers(id,name,url,username,password) VALUES(1,'Test','http://fixture.invalid','u','p'); INSERT INTO provider_routes VALUES(1,1); INSERT INTO provider_live(id,provider_id,stream_id,name) VALUES('iptv:1:1',1,'1','USA: CNN'); INSERT INTO health_accounts VALUES(1,9999999999,'authentication_failed'); INSERT INTO catalog_backoff VALUES(1,9999999999,'http_403',1);").unwrap();
        let before = crate::health::fingerprint(&db, "iptv:1:1").unwrap();
        db.execute("INSERT INTO candidate_health(live_id,disabled,excluded_until,source_key,state,next_check) VALUES('iptv:1:1',1,9999999999,?1,'cooling_down',9999999999)", [&before]).unwrap();
        set(&db, 1, false).unwrap();
        assert_ne!(before, crate::health::fingerprint(&db, "iptv:1:1").unwrap());
        assert!(!crate::health::eligible(&db, "iptv:1:1").unwrap());
        let row: (i64, i64, i64) = db
            .query_row(
                "SELECT disabled,excluded_until,next_check FROM candidate_health",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(row, (1, 9999999999, 0));
        db.execute(
            "UPDATE candidate_health SET disabled=0,excluded_until=0",
            [],
        )
        .unwrap();
        assert!(crate::health::eligible(&db, "iptv:1:1").unwrap());
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM catalog_backoff", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[tokio::test]
    async fn explicit_proxy_routes_requests_without_resolving_the_provider_locally() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut data = [0u8; 4096];
            let n = stream.read(&mut data).await.unwrap();
            assert!(String::from_utf8_lossy(&data[..n])
                .starts_with("GET http://provider.invalid/player_api.php HTTP/1.1"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .await
                .unwrap();
        });
        let client = builder(
            reqwest::Client::builder().timeout(Duration::from_secs(2)),
            Some(&proxy),
        )
        .unwrap()
        .build()
        .unwrap();
        assert_eq!(
            client
                .get("http://provider.invalid/player_api.php")
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        task.await.unwrap();
    }
}
