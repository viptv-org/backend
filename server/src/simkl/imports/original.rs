use super::*;
use viptv_engine::identity::EpisodeCoordinates;
#[derive(Default)]
pub(super) struct Counts {
    pub inserted: usize,
    pub skipped: usize,
    pub unresolved: usize,
}
fn runtime(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .filter(|v| v.is_finite() && *v > 0.0 && *v <= 1440.0)
        .map(|n| n * 60.0)
}
fn tvdb_coordinates(episode: &Value) -> Option<(u32, u32)> {
    let season = episode["tvdb"]["season"]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())?;
    let episode = episode["tvdb"]["episode"]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 0)?;
    Some((season, episode))
}
pub(super) fn original(
    db: &Connection,
    profile: i64,
    revision: u64,
    items: &Value,
    entries: &[viptv_engine::library::Entry],
) -> Result<Counts, ApiError> {
    original_canonical(
        db,
        profile,
        revision,
        items,
        entries,
        &std::collections::HashMap::new(),
    )
}

pub(super) fn original_canonical(
    db: &Connection,
    profile: i64,
    revision: u64,
    items: &Value,
    entries: &[viptv_engine::library::Entry],
    canonical: &std::collections::HashMap<(String, String), viptv_engine::identity::MediaIdentity>,
) -> Result<Counts, ApiError> {
    let mut counts = Counts::default();
    for (key, field, category) in [
        ("movies", "movie", "movie"),
        ("shows", "show", "tv"),
        ("anime", "show", "anime"),
    ] {
        for item in items[key].as_array().into_iter().flatten() {
            let Some(id) = item[field]["ids"]["simkl"].as_u64() else {
                continue;
            };
            let Some(entry) = entries
                .iter()
                .find(|e| e.card.identity.id == format!("simkl:{category}:{id}"))
            else {
                continue;
            };
            let mut parent = entry.card.clone();
            if let Some(show) = canonical.get(&(parent.identity.kind.clone(), parent.identity.id.clone())) {
                parent.identity = show.clone();
            }
            let parent = &parent;
            if parent.identity.kind == "movie" {
                if item["status"] == "completed" {
                    if let Some(updated) = progress::millis(&item["last_watched_at"]) {
                        let duration = runtime(&item[field]["runtime"]).unwrap_or(0.0);
                        if progress::apply(
                            db, profile, parent, None, duration, duration, updated, true, revision,
                        )? {
                            counts.inserted += 1;
                        } else {
                            counts.skipped += 1;
                        }
                    }
                }
                continue;
            }
            for season in item["seasons"].as_array().into_iter().flatten() {
                let season_number = season["number"]
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok());
                for episode in season["episodes"].as_array().into_iter().flatten() {
                    let number = episode["number"]
                        .as_u64()
                        .and_then(|n| u32::try_from(n).ok())
                        .filter(|n| *n > 0);
                    let (Some(number), Some(updated)) =
                        (number, progress::millis(&episode["watched_at"]))
                    else {
                        counts.unresolved += 1;
                        continue;
                    };
                    let mut card = parent.clone();
                    card.identity.ids.clear();
                    card.parent_identity = Some(parent.identity.clone());
                    let coordinates = if category == "anime" {
                        match tvdb_coordinates(episode) {
                            Some((season, episode)) => EpisodeCoordinates {
                                order: "aired".into(),
                                season: Some(season),
                                episode,
                            },
                            None => EpisodeCoordinates {
                                order: "anidb".into(),
                                season: None,
                                episode: number,
                            },
                        }
                    } else {
                        let Some(season) = season_number else {
                            counts.unresolved += 1;
                            continue;
                        };
                        EpisodeCoordinates {
                            order: "aired".into(),
                            season: Some(season),
                            episode: number,
                        }
                    };
                    card.identity.id = if let Some(s) = coordinates.season {
                        format!("{}:{s}:{}", parent.identity.id, coordinates.episode)
                    } else {
                        format!("{}:anidb:{}", parent.identity.id, coordinates.episode)
                    };
                    card.identity.episode = Some(coordinates);
                    let duration = runtime(&episode["runtime"]).unwrap_or(0.0);
                    if progress::apply(
                        db,
                        profile,
                        &card,
                        Some(parent),
                        duration,
                        duration,
                        updated,
                        true,
                        revision,
                    )? {
                        counts.inserted += 1;
                    } else {
                        counts.skipped += 1;
                    }
                    if counts.inserted + counts.skipped + counts.unresolved > 50000 {
                        return Err("History exceeds its bounded item limit".into());
                    }
                }
            }
        }
    }
    Ok(counts)
}
