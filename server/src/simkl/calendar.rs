//! Public Simkl v2 release schedules. Calendar identity is not playback authority.
use super::*;
use chrono::{DateTime, NaiveDate};
use serde::Serialize;
use viptv_engine::presentation::Card;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CalendarQuery {
    #[serde(rename = "type", default = "default_kind")]
    kind: String,
    start: Option<String>,
    end: Option<String>,
    #[serde(default)]
    skip: usize,
}
fn default_kind() -> String {
    "series".into()
}
#[derive(Serialize)]
pub(crate) struct Entry {
    id: String,
    card: Card,
    date: String,
    episode_title: Option<String>,
    season: Option<u32>,
    episode: Option<u32>,
}

pub(crate) async fn get(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Query(q): Query<CalendarQuery>,
) -> ApiResult {
    let auth::Principal::Account {
        profile_id: Some(profile),
        ..
    } = lease.principal
    else {
        return Err(auth::forbidden());
    };
    let app = app.with_lease(lease.clone());
    let (configuration, _) = crate::engine_v3::configuration::read(&app, &lease, profile)?;
    if configuration.browsing_plugin != "simkl" {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Calendar is unavailable for this browsing integration".into(),
        ));
    }
    let (category, file) = match q.kind.as_str() {
        "movie" => ("movie", "movie_release"),
        "series" => ("tv", "tv"),
        "anime" => ("anime", "anime"),
        _ => return Err("Invalid calendar type".into()),
    };
    let today = DateTime::from_timestamp(util::now(), 0)
        .ok_or("Invalid server clock")?
        .date_naive();
    let date = |raw: Option<&str>, default: NaiveDate| -> Result<NaiveDate, ApiError> {
        match raw {
            Some(raw) if raw.len() == 10 => NaiveDate::parse_from_str(raw, "%Y-%m-%d")
                .map_err(|_| ApiError::from("Invalid calendar date")),
            None => Ok(default),
            _ => Err("Invalid calendar date".into()),
        }
    };
    let start = date(q.start.as_deref(), today - chrono::Duration::days(3))?;
    let end = date(q.end.as_deref(), today + chrono::Duration::days(3))?;
    if end < start || (end - start).num_days() > 33 || q.skip > 10_000 {
        return Err("Invalid calendar window".into());
    }
    let access = store::credential(&app, profile, Capability::Discovery)?;
    let doc = app
        .simkl
        .public_document(
            &format!("/calendar/v2/{file}.json"),
            Duration::from_secs(5 * 3600),
            || store::validate(&app, profile, &access),
        )
        .await?;
    store::validate(&app, profile, &access)?;
    let events = doc["calendar"]
        .as_array()
        .filter(|v| v.len() <= 10_000)
        .ok_or("Simkl calendar was invalid")?;
    let metadata = doc["metadata"]
        .as_object()
        .filter(|v| v.len() <= 10_000)
        .ok_or("Simkl calendar was invalid")?;
    let mut entries = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for event in events {
        let Some(id) = event["simkl_id"].as_u64().filter(|id| *id > 0) else {
            continue;
        };
        let Some(raw_date) = event["date"].as_str().filter(|s| s.len() <= 64) else {
            continue;
        };
        let Ok(timestamp) = DateTime::parse_from_rfc3339(raw_date) else {
            continue;
        };
        let day = timestamp.with_timezone(&chrono::Utc).date_naive();
        if day < start || day > end {
            continue;
        }
        let Some(value) = metadata.get(&id.to_string()) else {
            continue;
        };
        if viptv_engine::simkl::id(value) != Some(id) {
            return Err("Simkl calendar identity mismatch".into());
        }
        let Ok(card) = viptv_engine::simkl::card(value, category) else {
            continue;
        };
        if metadata::allowed(&app, vec![card.clone()])?.is_empty() {
            continue;
        }
        let season = event["episode"]["season"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok());
        let episode = event["episode"]["episode"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 0);
        let episode_title = event["episode"]["title"]
            .as_str()
            .filter(|s| s.len() <= 512 && !s.chars().any(char::is_control))
            .map(str::to_owned);
        let date = timestamp
            .with_timezone(&chrono::Utc)
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let stable = serde_json::to_vec(&(category, id, &date, season, episode))
            .map_err(|_| ApiError::from("Calendar unavailable"))?;
        let stable = sha2::Sha256::digest(stable)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        if !seen.insert(stable.clone()) {
            continue;
        }
        entries.push(Entry {
            id: stable,
            card,
            date,
            episode_title,
            season,
            episode,
        });
    }
    entries.sort_by(|a, b| a.date.cmp(&b.date).then(a.id.cmp(&b.id)));
    let total = entries.len();
    let entries: Vec<_> = entries.into_iter().skip(q.skip).take(100).collect();
    let next = q.skip + entries.len();
    store::validate(&app, profile, &access)?;
    Ok(axum::Json(
        json!({"entries":entries,"next_cursor":(next<total).then(||next.to_string()),"complete":next>=total}),
    ))
}
