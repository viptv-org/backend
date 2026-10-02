use super::*;

#[derive(Clone)]
pub(super) struct Candidate {
    pub kind: String,
    pub id: String,
    pub title: String,
    pub name: String,
    pub favorite: bool,
    pub progress: Option<Progress>,
}
#[derive(Clone)]
pub(super) struct Progress {
    pub position: f64,
    pub duration: f64,
    pub timestamp: i64,
    pub context: Value,
}

fn imdb(id: &str) -> bool {
    id.strip_prefix("tt").is_some_and(|digits| {
        (5..=12).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit())
    })
}
fn episode(id: &str, title: &str) -> Option<(i64, i64)> {
    let parts: Vec<_> = id.split(':').collect();
    if parts.len() != 3 || parts[0] != title {
        return None;
    }
    let number = |s: &str| {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse::<i64>().ok().filter(|n| (0..=100000).contains(n))
    };
    Some((number(parts[1])?, number(parts[2])?))
}
fn milliseconds(v: &Value) -> Option<f64> {
    v.as_u64()
        .filter(|n| *n <= 1_000_000_000)
        .map(|n| n as f64 / 1000.0)
}
fn timestamp(v: &Value, now: i64) -> Option<i64> {
    // Stremio serializes lastWatched as an RFC3339 date, not array order or activity count.
    let date = chrono::DateTime::parse_from_rfc3339(v.as_str()?).ok()?;
    let seconds = date.timestamp();
    (seconds > 0 && date.timestamp_millis() <= now.checked_mul(1000)?).then_some(seconds)
}

pub(super) fn map(
    items: &[Value],
    library: bool,
    history: bool,
    now: i64,
) -> (Vec<Candidate>, Summary) {
    map_verified(items, library, history, now, &HashMap::new())
}

pub(super) fn map_verified(
    items: &[Value],
    library: bool,
    history: bool,
    now: i64,
    metadata: &HashMap<(String, String), Value>,
) -> (Vec<Candidate>, Summary) {
    let mut result = Vec::new();
    let mut summary = Summary {
        source_items: items.len(),
        ..Default::default()
    };
    let mut identities = HashMap::new();
    for item in items {
        if let (Some(kind), Some(id)) = (item["type"].as_str(), item["_id"].as_str()) {
            *identities.entry((kind, id)).or_insert(0usize) += 1;
        }
    }
    for item in items {
        let state = &item["state"];
        let Some(id) = item["_id"].as_str() else {
            summary.skipped_items += 1;
            continue;
        };
        let kind = item["type"].as_str().unwrap_or("");
        if !matches!(kind, "movie" | "series") || !state.is_object() {
            summary.skipped_items += 1;
            continue;
        }
        let removed = item["removed"].as_bool().unwrap_or(false);
        let temp = item["temp"].as_bool().unwrap_or(false);
        let offset = milliseconds(&state["timeOffset"]);
        let duration = milliseconds(&state["duration"]);
        let watched = state["timesWatched"].as_u64().unwrap_or(0) > 0;
        let bitfield = state["watched"].as_str().is_some_and(|s| !s.is_empty());
        if removed && offset.unwrap_or(0.0) == 0.0 && !watched && !bitfield {
            summary.skipped_items += 1;
            continue;
        }
        // Ambiguous duplicates are deferred, not resolved by response-array order.
        if !(imdb(id) || metadata.contains_key(&(kind.to_owned(), id.to_owned())))
            || identities.get(&(kind, id)).copied().unwrap_or(0) != 1
        {
            summary.needs_review += 1;
            continue;
        }
        let Some(name) = item["name"].as_str().filter(|s| {
            !s.trim().is_empty()
                && s.len() <= 512
                && !s.chars().any(char::is_control)
                && !s.contains("://")
                && !s.contains('@')
        }) else {
            summary.needs_review += 1;
            continue;
        };
        let favorite = library && !removed && !temp;
        let mut candidate = Candidate {
            kind: kind.into(),
            id: id.into(),
            title: id.into(),
            name: name.into(),
            favorite,
            progress: None,
        };
        if history {
            if kind == "series" && bitfield {
                summary.needs_review += 1;
            }
            let resume =
                matches!((offset, duration), (Some(p), Some(d)) if p > 0.0 && d > 0.0 && p <= d);
            // A rewatch resume wins over timesWatched. Counts alone never identify an episode.
            let movie_completed = kind == "movie" && watched && offset == Some(0.0);
            if resume || movie_completed {
                let time = timestamp(&state["lastWatched"], now);
                let identity = if kind == "series" {
                    state["video_id"].as_str().and_then(|video| {
                        (if imdb(id) { episode(video, id) } else { None })
                            .or_else(|| {
                                metadata
                                    .get(&(kind.to_owned(), id.to_owned()))
                                    .and_then(|meta| meta["videos"].as_array())
                                    .and_then(|videos| videos.iter().find(|v| v["id"] == video))
                                    .and_then(|v| {
                                        Some((v["season"].as_i64()?, v["episode"].as_i64()?))
                                    })
                                    .filter(|(s, e)| {
                                        (0..=100000).contains(s) && (0..=100000).contains(e)
                                    })
                            })
                            .map(|(s, e)| {
                                (
                                    video.to_owned(),
                                    if imdb(id) {
                                        json!({"series_id":id,"imdb_id":id,"season":s,"episode":e})
                                    } else {
                                        json!({"series_id":id,"season":s,"episode":e})
                                    },
                                )
                            })
                    })
                } else {
                    Some((
                        id.to_owned(),
                        if imdb(id) {
                            json!({"imdb_id":id})
                        } else {
                            json!({})
                        },
                    ))
                };
                if let (Some(time), Some((progress_id, mut context))) = (time, identity) {
                    let d = duration.unwrap_or(0.0);
                    let p = if resume {
                        offset.unwrap()
                    } else {
                        context["watched_override"] = json!(true);
                        context["stremio_import_watched"] = json!(true);
                        d
                    };
                    // Favorite identity is the series title; progress identity is an exact episode.
                    if favorite && progress_id != id {
                        result.push(candidate);
                        candidate = Candidate {
                            kind: kind.into(),
                            id: progress_id,
                            title: id.into(),
                            name: name.into(),
                            favorite: false,
                            progress: None,
                        };
                    } else {
                        candidate.id = progress_id;
                    }
                    candidate.progress = Some(Progress {
                        position: p,
                        duration: d,
                        timestamp: time,
                        context,
                    });
                } else {
                    summary.needs_review += 1;
                }
            } else if offset.unwrap_or(0.0) > 0.0 || watched {
                summary.needs_review += 1;
            }
        }
        if candidate.favorite || candidate.progress.is_some() {
            result.push(candidate);
        } else {
            summary.skipped_items += 1;
        }
    }
    (result, summary)
}
