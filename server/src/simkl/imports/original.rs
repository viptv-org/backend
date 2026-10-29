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
pub(super) fn original(
    db: &Connection,
    profile: i64,
    revision: u64,
    items: &Value,
    entries: &[viptv_engine::library::Entry],
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
            let Some(parent) = entries
                .iter()
                .find(|e| e.card.identity.id == format!("simkl:{category}:{id}"))
                .map(|e| &e.card)
            else {
                continue;
            };
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
                        EpisodeCoordinates {
                            order: "anidb".into(),
                            season: None,
                            episode: number,
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
                        format!("{}:{s}:{number}", parent.identity.id)
                    } else {
                        format!("{}:anidb:{number}", parent.identity.id)
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
