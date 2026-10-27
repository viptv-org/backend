//! The Calendar: the profile's library episodes merged with Simkl's public v2
//! release schedule. Calendar identity is not playback authority.
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
    /// `"library"`: an episode of a series in the profile's library;
    /// `"public"`: the provider's public release schedule.
    source: &'static str,
    /// Library entries only: the typed, playable episode card.
    #[serde(skip_serializing_if = "Option::is_none")]
    episode_card: Option<Card>,
    #[serde(skip)]
    keys: Vec<String>,
}

/// One merged Calendar: episodes of the profile's library series (any
/// browsing integration, including Stremio imports) and, when Simkl is the
/// browsing integration, Simkl's public schedule. An episode in both appears
/// once, as `library`.
pub(crate) async fn get(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Query(q): Query<CalendarQuery>,
) -> ApiResult {
    use crate::engine_v3::continuation::calendar as library;
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
    // Kids sessions keep the public schedule's own approval filter and never
    // start library metadata lookups (as Continue Watching's refresh).
    let library_series = {
        let db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        lease.validate(&db)?;
        !crate::kids::restricted(&db, &lease.principal)? && library::has_series_db(&db, profile)?
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
    let mut library_complete = true;
    if library_series && category != "movie" {
        let found = library::episodes(&app, &lease, profile).await?;
        library_complete = found.complete;
        for episode in &found.episodes {
            // Simkl anime series belong to the anime calendar; every other
            // library series to the series calendar.
            if episode.series.identity.id.starts_with("simkl:anime:") != (category == "anime") {
                continue;
            }
            if let Some(entry) = library_entry(episode, start, end)? {
                entries.push(entry);
            }
        }
        sources.push("library");
    }
    if public {
        match public_entries(&app, profile, category, file, start, end).await {
            Ok(schedule) => {
                let library = entries.len();
                for entry in schedule {
                    if !entries[..library].iter().any(|own| same_episode(own, &entry)) {
                        entries.push(entry);
                    }
                }
                sources.push("public");
            }
            // The library still answers when only the public schedule failed.
            Err(error)
                if !sources.is_empty()
                    && !matches!(
                        error.0,
                        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::CONFLICT
                    ) => {}
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
    if !library_complete {
        // Some library series are still being looked up.
        page["refresh_after_millis"] = json!(4000);
    }
    Ok(axum::Json(page))
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

/// The same series (any shared identity) and the same episode: matching
/// coordinates, or the same air day when either side has none.
fn same_episode(own: &Entry, public: &Entry) -> bool {
    own.keys.iter().any(|key| public.keys.contains(key))
        && match (own.season.zip(own.episode), public.season.zip(public.episode)) {
            (Some(a), Some(b)) => a == b,
            _ => own.date.get(..10) == public.date.get(..10),
        }
}

/// Simkl's public v2 schedule for `category` within the window.
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
        let mut keys = vec![card.identity.id.clone()];
        keys.extend(
            card.identity
                .ids
                .iter()
                .map(|external| format!("{}:{}", external.namespace, external.value)),
        );
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
