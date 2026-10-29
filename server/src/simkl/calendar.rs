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
    limit: Option<usize>,
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
    source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    episode_card: Option<Card>,
    #[serde(skip)]
    keys: Vec<String>,
}

pub(crate) async fn get(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Query(q): Query<CalendarQuery>,
) -> ApiResult {
    use crate::engine_v3::continuation::calendar as your_shows;
    let auth::Principal::Account {
        profile_id: Some(profile),
        ..
    } = lease.principal
    else {
        return Err(auth::forbidden());
    };
    let app = app.with_lease(lease.clone());
    let (configuration, _) = crate::engine_v3::configuration::read(&app, &lease, profile)?;
    let public = configuration.browsing_plugin == "simkl";
    let library_series = {
        let db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        lease.validate(&db)?;
        !crate::kids::restricted(&db, &lease.principal)? && your_shows::has_series_db(&db, profile)?
    };
    if !public && !library_series {
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
    let limit = q.limit.unwrap_or(100);
    if !(1..=500).contains(&limit) {
        return Err("Invalid calendar page size".into());
    }
    let mut entries = Vec::new();
    let mut sources = Vec::new();
    let mut pending = false;
    let mut own = Vec::new();
    if library_series && category != "movie" {
        match your_shows::episodes(&app, &lease, profile, public).await {
            Ok(found) => {
                pending = found.pending;
                for episode in &found.episodes {
                    let Some(entry) = library_entry(episode, start, end)? else {
                        continue;
                    };
                    if episode.series.identity.id.starts_with("simkl:anime:") == (category == "anime") {
                        entries.push(entry);
                    } else {
                        own.push(entry);
                    }
                }
                sources.push("library");
            }
            Err(error) if public && !fatal(&error) => {}
            Err(error) => return Err(error),
        }
    }
    if public {
        match public_entries(&app, profile, category, file, start, end).await {
            Ok(schedule) => {
                let shown = entries.len();
                for entry in schedule {
                    let duplicate = entries[..shown]
                        .iter()
                        .chain(&own)
                        .any(|library| same_episode(library, &entry));
                    if !duplicate {
                        entries.push(entry);
                    }
                }
                sources.push("public");
            }
            Err(error) if !sources.is_empty() && !fatal(&error) => {}
            Err(error) => return Err(error),
        }
    }
    let mut seen = std::collections::HashSet::new();
    entries.retain(|entry| seen.insert(entry.id.clone()));
    entries.sort_by(|a, b| a.date.cmp(&b.date).then(a.id.cmp(&b.id)));
    let total = entries.len();
    let entries: Vec<_> = entries.into_iter().skip(q.skip).take(limit).collect();
    let next = q.skip + entries.len();
    crate::engine_v3::configuration::validate_revision(&app, profile, configuration.scope.revision)?;
    lease.validate(
        &*app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?,
    )?;
    let mut page = json!({"entries":entries,"next_cursor":(next<total).then(||next.to_string()),"complete":next>=total,"sources":sources});
    if pending {
        page["refresh_after_millis"] = json!(your_shows::PARTIAL_TTL.as_millis() as u64);
    }
    Ok(axum::Json(page))
}

fn fatal(error: &ApiError) -> bool {
    matches!(
        error.0,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::CONFLICT
    )
}

fn stable_id(value: impl Serialize) -> Result<String, ApiError> {
    let stable = serde_json::to_vec(&value).map_err(|_| ApiError::from("Calendar unavailable"))?;
    Ok(sha2::Sha256::digest(stable)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn library_entry(
    found: &crate::engine_v3::continuation::calendar::Episode,
    start: NaiveDate,
    end: NaiveDate,
) -> Result<Option<Entry>, ApiError> {
    let Some(timestamp) = DateTime::from_timestamp_millis(found.released_at_millis as i64) else {
        return Ok(None);
    };
    let day = timestamp.date_naive();
    if day < start || day > end {
        return Ok(None);
    }
    let date = timestamp.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let coordinates = found.episode.identity.episode.as_ref();
    let title = found.episode.title.as_str();
    let episode_title = (!title.is_empty()
        && title != found.episode.identity.id
        && title != found.series.title
        && title.len() <= 512
        && !title.chars().any(char::is_control))
    .then(|| title.to_owned());
    Ok(Some(Entry {
        id: stable_id((
            "library",
            &found.series.identity.id,
            &found.episode.identity.id,
            &date,
        ))?,
        card: found.series.clone(),
        date,
        episode_title,
        season: coordinates.and_then(|c| c.season),
        episode: coordinates.map(|c| c.episode),
        source: "library",
        episode_card: Some(found.episode.clone()),
        keys: found.keys.clone(),
    }))
}

fn same_episode(library: &Entry, public: &Entry) -> bool {
    library.keys.iter().any(|key| public.keys.contains(key))
        && match (library.season.zip(library.episode), public.season.zip(public.episode)) {
            (Some(a), Some(b)) => a == b,
            _ => library.date.get(..10) == public.date.get(..10),
        }
}

async fn public_entries(
    app: &App,
    profile: i64,
    category: &str,
    file: &str,
    start: NaiveDate,
    end: NaiveDate,
) -> Result<Vec<Entry>, ApiError> {
    let app = app.clone();
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
    let mut permitted = std::collections::HashMap::new();
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
        let Ok(mut card) = viptv_engine::simkl::card(value, category) else {
            continue;
        };
        card.metadata_origin = Some(viptv_engine::origin::MetadataOrigin {
            provider_id: "simkl".into(),
            connection_id: None,
            incarnation: None,
        });
        let allowed = match permitted.get(&id) {
            Some(allowed) => *allowed,
            None => {
                let allowed = !metadata::allowed(&app, vec![card.clone()])?.is_empty();
                permitted.insert(id, allowed);
                allowed
            }
        };
        if !allowed {
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
        let stable = stable_id((category, id, &date, season, episode))?;
        if !seen.insert(stable.clone()) {
            continue;
        }
        let keys = crate::engine_v3::continuation::calendar::identity_keys([&card]);
        entries.push(Entry {
            id: stable,
            card,
            date,
            episode_title,
            season,
            episode,
            source: "public",
            episode_card: None,
            keys,
        });
    }
    store::validate(&app, profile, &access)?;
    Ok(entries)
}
