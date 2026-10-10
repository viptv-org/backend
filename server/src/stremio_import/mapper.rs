use super::*;

use base64::{engine::general_purpose::STANDARD, Engine};
use flate2::read::ZlibDecoder;
use std::{collections::HashSet, io::Read};

// Stremio stores {last_watched_video_id}:{one_based_index}:{zlib(base64(bitset))}.
// Like Stremio, bits are aligned at the anchor: videos added before it shift the
// stored bits forward, and bits after the anchor name the following videos.
// Regular seasons must be complete and contiguous; specials may skip numbers.
pub(super) fn verified_episodes(field: &str, title: &str, meta: &Value) -> Option<Vec<(String, i64, i64)>> {
    let (prefix, encoded) = field.rsplit_once(':')?;
    let (anchor, count) = prefix.rsplit_once(':')?;
    let count = count.parse::<usize>().ok()?;
    let videos = meta["videos"]
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 2000)?;
    let mut ids = HashSet::new();
    let mut coordinates = HashSet::new();
    let mut ordered = Vec::with_capacity(videos.len());
    let mut previous = None;
    for video in videos {
        let id = video["id"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 512)?;
        let season = video["season"]
            .as_i64()
            .filter(|n| (0..=100000).contains(n))?;
        let episode_number = video["episode"]
            .as_i64()
            .filter(|n| (0..=100000).contains(n))?;
        if !ids.insert(id)
            || !coordinates.insert((season, episode_number))
            || (imdb(title) && episode(id, title) != Some((season, episode_number)))
        {
            return None;
        }
        if previous.is_some_and(|coordinate| coordinate >= (season, episode_number)) {
            return None;
        }
        if season != 0 {
            match previous {
                Some((prior_season, prior_episode)) if prior_season == season => {
                    if episode_number != prior_episode + 1 {
                        return None;
                    }
                }
                _ if episode_number != 1 => return None,
                _ => {}
            }
        }
        previous = Some((season, episode_number));
        ordered.push((id.to_owned(), season, episode_number));
    }
    let anchor_index = ordered.iter().position(|(id, _, _)| id == anchor)?;
    // Fewer videos before the anchor than recorded means episodes vanished: the
    // stored bits cannot be attributed. Added videos are an end-aligned shift.
    if count == 0 || count > 2000 || count > anchor_index + 1 {
        return None;
    }
    let compressed = STANDARD.decode(encoded).ok().filter(|v| v.len() <= 4096)?;
    let decoder = ZlibDecoder::new(compressed.as_slice());
    let mut bytes = Vec::new();
    decoder.take(251).read_to_end(&mut bytes).ok()?;
    if bytes.is_empty() || bytes.len() > 250 || bytes.len() * 8 < count {
        return None;
    }
    let offset = anchor_index + 1 - count;
    let mut result = Vec::new();
    for (index, byte) in bytes.iter().enumerate() {
        for bit in 0..8 {
            if byte & (1 << bit) != 0 {
                let at = index * 8 + bit;
                if offset + at >= ordered.len() {
                    return None;
                }
                result.push(ordered[offset + at].clone());
            }
        }
    }
    (result.iter().any(|(id, _, _)| id == anchor)).then_some(result)
}
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
        if result.len() + 2 > 20_000 {
            summary.needs_review += 1;
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
            let completed = if kind == "series" && bitfield {
                metadata
                    .get(&(kind.to_owned(), id.to_owned()))
                    .and_then(|meta| {
                        state["watched"]
                            .as_str()
                            .and_then(|field| verified_episodes(field, id, meta))
                    })
            } else {
                None
            };
            if kind == "series" && bitfield && completed.is_none() {
                summary.needs_review += 1;
            }
            if completed
                .as_ref()
                .is_some_and(|episodes| episodes.len() + result.len() + 2 > 20_000)
            {
                summary.needs_review += 1;
                continue;
            }
            let resume =
                matches!((offset, duration), (Some(p), Some(d)) if p > 0.0 && d > 0.0 && p <= d);
            let time = timestamp(&state["lastWatched"], now);
            if let Some(episodes) = &completed {
                for (video, season, episode_number) in episodes {
                    if state["video_id"] == *video && resume && time.is_some() {
                        continue;
                    }
                    let mut context = json!({"series_id":id,"season":season,"episode":episode_number,
                        "watched_override":true,"stremio_import_watched":true,
                        "stremio_completion_only":true,"stremio_watch_date_unknown":true});
                    if imdb(id) {
                        context["imdb_id"] = json!(id);
                    }
                    result.push(Candidate {
                        kind: kind.into(),
                        id: video.clone(),
                        title: id.into(),
                        name: name.into(),
                        favorite: false,
                        progress: Some(Progress {
                            position: 0.0,
                            duration: 0.0,
                            timestamp: 0,
                            context,
                        }),
                    });
                }
            }
            // A rewatch resume wins over timesWatched. Counts alone never identify an episode.
            let movie_completed = kind == "movie" && watched && offset == Some(0.0);
            if resume || movie_completed {
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
                    if (kind == "movie" && watched)
                        || (kind == "series"
                            && completed.as_ref().is_some_and(|episodes| {
                                episodes.iter().any(|(video, _, _)| video == &progress_id)
                            }))
                    {
                        context["stremio_import_watched"] = json!(true);
                    }
                    // A watched imported offset may be stale, not evidence of a new rewatch.
                    // Keep it intact, but anchor this import's queue at the last verified
                    // watched episode in the same regular/special sequence.
                    if kind == "series" && resume {
                        if let Some(episodes) = &completed {
                            if let Some((_, season, episode_number)) =
                                episodes.iter().find(|(video, _, _)| video == &progress_id)
                            {
                                if let Some((last, _, _)) =
                                    episodes.iter().rev().find(|(_, s, e)| {
                                        (*s == 0) == (*season == 0)
                                            && (*s, *e) > (*season, *episode_number)
                                    })
                                {
                                    context["stremio_continuation"] =
                                        json!({"id":last,"activity_at":time});
                                }
                            }
                        }
                    }
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
                } else if !(movie_completed && time.is_none()) {
                    summary.needs_review += 1;
                }
            } else if offset.unwrap_or(0.0) > 0.0 || (watched && kind == "movie") {
                summary.needs_review += 1;
            }
            if kind == "movie" && watched && candidate.progress.is_none() {
                // The watched counter is a fact even when the rewatch activity cannot
                // be placed on a timeline; lastWatched may belong to that failed resume.
                let mut context = if imdb(id) {
                    json!({"imdb_id":id})
                } else {
                    json!({})
                };
                context["watched_override"] = json!(true);
                context["stremio_import_watched"] = json!(true);
                context["stremio_completion_only"] = json!(true);
                context["stremio_watch_date_unknown"] = json!(true);
                candidate.progress = Some(Progress {
                    position: 0.0,
                    duration: 0.0,
                    timestamp: 0,
                    context,
                });
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
