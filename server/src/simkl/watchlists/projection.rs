use super::*;
fn millis(value: &Value) -> Option<u64> {
    let date = chrono::DateTime::parse_from_rfc3339(value.as_str()?).ok()?;
    u64::try_from(date.timestamp_millis()).ok()
}
pub(in crate::simkl) fn entries(value: &Value) -> Result<Vec<Entry>, ApiError> {
    let mut entries = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (key, category, field) in [
        ("movies", "movie", "movie"),
        ("shows", "tv", "show"),
        ("anime", "anime", "show"),
    ] {
        let Some(section) = value.get(key) else {
            continue;
        };
        if section.is_null() {
            continue;
        }
        let values = section.as_array().ok_or_else(|| {
            ApiError(StatusCode::BAD_GATEWAY, "Watchlist data was invalid".into())
        })?;
        if values.len() > 10000 {
            return Err("Watchlist exceeds its bounded item limit".into());
        }
        for item in values {
            let mut title = item
                .get(field)
                .filter(|v| v.is_object())
                .or_else(|| item.get("anime"))
                .unwrap_or(item)
                .clone();
            if category == "anime" && item["anime_type"].is_string() {
                title["anime_type"] = item["anime_type"].clone();
            }
            let Ok(mut card) = viptv_engine::simkl::card(&title, category) else {
                continue;
            };
            if !seen.insert((card.identity.kind.clone(), card.identity.id.clone())) {
                continue;
            }
            card.added_at_millis = millis(&item["added_to_watchlist_at"]);
            let mut stats = LibraryStatistics::empty(card.identity.clone());
            stats.status = item["status"]
                .as_str()
                .filter(|s| {
                    matches!(
                        *s,
                        "watching" | "plantowatch" | "hold" | "completed" | "dropped"
                    )
                })
                .map(str::to_owned);
            if item["status"] == "notinteresting" {
                stats.status = Some("dropped".into());
            }
            stats.watched = stats.status.as_deref() == Some("completed");
            stats.last_watched_millis = millis(&item["last_watched_at"]);
            stats.times_watched = item["watch_count"].as_u64();
            stats.user_rating = item["user_rating"]
                .as_f64()
                .filter(|rating| rating.is_finite() && *rating > 0.0 && *rating <= 10.0);
            stats.watched_episode_count = item["watched_episodes_count"].as_u64();
            stats.total_episode_count = item["total_episodes_count"].as_u64();
            stats.aired_episode_count = item["total_aired_episodes_count"].as_u64().or_else(|| {
                stats
                    .total_episode_count
                    .zip(item["not_aired_episodes_count"].as_u64())
                    .filter(|(total, not_aired)| not_aired <= total)
                    .map(|(total, not_aired)| total - not_aired)
            });
            if category != "movie" {
                stats.has_unwatched_aired_episodes = item.get("next_to_watch").and_then(|v| {
                    if v.is_null() {
                        Some(false)
                    } else {
                        v.as_str().map(|marker| !marker.is_empty())
                    }
                });
            }
            let next = &item["next_to_watch_info"];
            if let Some(episode) = next["episode"]
                .as_u64()
                .filter(|n| *n > 0 && *n <= u32::MAX as u64)
            {
                stats.next_episode = Some(viptv_engine::identity::EpisodeCoordinates {
                    order: if category == "anime" {
                        "anidb"
                    } else {
                        "aired"
                    }
                    .into(),
                    season: next["season"].as_u64().and_then(|n| u32::try_from(n).ok()),
                    episode: episode as u32,
                });
                stats.next_air_millis = millis(&next["date"]);
            }
            if let Some((watched, total)) = stats
                .watched_episode_count
                .zip(stats.total_episode_count)
                .filter(|(_, total)| *total > 0)
            {
                card.progress = Some((watched as f64 / total as f64).clamp(0.0, 1.0));
            }
            entries.push(Entry {
                card,
                statistics: stats,
            });
            if entries.len() > 10000 {
                return Err("Watchlist exceeds its bounded item limit".into());
            }
        }
    }

    Ok(entries)
}
