use super::*;
use axum::{body::Bytes, Json};
mod scrub;
#[cfg(test)]
mod tests;

pub(crate) const MAX_REPORT_BYTES: usize = 64 * 1024;
pub(crate) const KEPT_PER_ACCOUNT: i64 = 20;
const SUMMARY_CHARS: usize = 128;

pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS crash_reports(
            seq INTEGER PRIMARY KEY AUTOINCREMENT,
            id TEXT NOT NULL UNIQUE,
            account_id INTEGER NOT NULL REFERENCES auth_accounts(id) ON DELETE CASCADE,
            received_at INTEGER NOT NULL,
            device_model TEXT NOT NULL,
            app_version TEXT NOT NULL,
            build_commit TEXT NOT NULL,
            body TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS crash_reports_account ON crash_reports(account_id,seq);",
    )
}

fn summary_field(report: &Value, name: &str) -> String {
    report
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .chars()
        .take(SUMMARY_CHARS)
        .collect()
}

fn parse(body: &Bytes) -> Result<(Value, String), account_api::Error> {
    let Ok(Value::Object(object)) = serde_json::from_slice::<Value>(body) else {
        return Err("invalid_crash_report".into());
    };
    let report = scrub::value(Value::Object(object));
    let stored = serde_json::to_string(&report).map_err(|_| "invalid_crash_report")?;
    if stored.len() > MAX_REPORT_BYTES {
        return Err("crash_report_too_large".into());
    }
    Ok((report, stored))
}

pub(crate) async fn upload(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    body: Bytes,
) -> Result<Response, account_api::Error> {
    let (report, stored) = parse(&body)?;
    let id = Uuid::new_v4().to_string();
    let received_at = util::now();
    let (device_model, app_version, build_commit) = (
        summary_field(&report, "deviceModel"),
        summary_field(&report, "appVersion"),
        summary_field(&report, "buildCommit"),
    );
    let stored_id = id.clone();
    tokio::task::spawn_blocking(move || {
        let mut db = app
            .db
            .lock()
            .map_err(|_| account_api::Error::Code("crash_report_storage_unavailable"))?;
        lease.validate(&db)?;
        let account = lease
            .principal
            .account_id()
            .ok_or_else(auth::unauthorized)?;
        let tx = db.transaction().map_err(db_error)?;
        tx.execute(
            "INSERT INTO crash_reports(id,account_id,received_at,device_model,app_version,build_commit,body) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![stored_id, account, received_at, device_model, app_version, build_commit, stored],
        )
        .map_err(db_error)?;
        tx.execute(
            "DELETE FROM crash_reports WHERE account_id=?1 AND seq NOT IN (SELECT seq FROM crash_reports WHERE account_id=?1 ORDER BY seq DESC LIMIT ?2)",
            params![account, KEPT_PER_ACCOUNT],
        )
        .map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        Ok::<_, account_api::Error>(())
    })
    .await
    .map_err(|_| account_api::Error::Code("crash_report_storage_unavailable"))??;
    let mut response = (
        StatusCode::CREATED,
        Json(json!({"id":id,"received_at":received_at})),
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store"));
    Ok(response)
}

pub(crate) async fn list(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
) -> Result<Json<Value>, account_api::Error> {
    account_api::owner_work(app, lease, |db, account| {
        let mut statement = db
            .prepare("SELECT id,received_at,device_model,app_version,build_commit FROM crash_reports WHERE account_id=?1 ORDER BY seq DESC LIMIT ?2")
            .map_err(db_error)?;
        let reports = statement
            .query_map(params![account, KEPT_PER_ACCOUNT], |row| {
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "received_at": row.get::<_, i64>(1)?,
                    "summary": {
                        "device_model": row.get::<_, String>(2)?,
                        "app_version": row.get::<_, String>(3)?,
                        "build_commit": row.get::<_, String>(4)?,
                    },
                }))
            })
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        Ok(json!({"reports": reports}))
    })
    .await
}

pub(crate) async fn get_one(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
) -> Result<Json<Value>, account_api::Error> {
    account_api::owner_work(app, lease, move |db, account| {
        let row = db
            .query_row(
                "SELECT received_at,body FROM crash_reports WHERE id=?1 AND account_id=?2",
                params![id, account],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(db_error)?;
        let (received_at, body) = row.ok_or("crash_report_not_found")?;
        let report: Value = serde_json::from_str(&body).map_err(|_| "crash_report_not_found")?;
        Ok(json!({"id": id, "received_at": received_at, "report": report}))
    })
    .await
}

pub(crate) async fn remove(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(id): Path<String>,
) -> Result<Json<Value>, account_api::Error> {
    account_api::owner_work(app, lease, move |db, account| {
        let removed = db
            .execute(
                "DELETE FROM crash_reports WHERE id=?1 AND account_id=?2",
                params![id, account],
            )
            .map_err(db_error)?;
        if removed == 0 {
            return Err("crash_report_not_found".into());
        }
        Ok(json!({"deleted": true}))
    })
    .await
}
