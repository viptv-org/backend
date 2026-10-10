use super::*;

pub(super) fn enrich_detail(v: &Value) -> Value {
    {
        let mut out = v["original"].clone();
        if let Some(m) = v["metadata"].as_object() {
            for (k, value) in m {
                if !value.is_null()
                    && !(k == "genres" && value.as_array().is_some_and(Vec::is_empty))
                {
                    out[k] = value.clone();
                }
            }
        }
        // Metadata enriches a title; occurrence coordinates and history facts
        // retain their authority when the lookup addresses its parent series.
        for key in [
            "season",
            "episode",
            "seriesId",
            "watched",
            "resumeActive",
            "completionOnly",
            "watchDateKnown",
            "updatedAtMillis",
            "sourceAddonId",
            "sourceFingerprint",
            "sourceName",
            "sourceBingeGroup",
            "sourceReleaseGroup",
            "sourceQuality",
            "sourceAudio",
        ] {
            if !v["original"][key].is_null() {
                out[key] = v["original"][key].clone();
            }
        }
        if num(&v["metadata"], "position") <= 0.0 && num(&v["original"], "position") > 0.0 {
            out["position"] = v["original"]["position"].clone();
        }
        let mut raw = v["original"]["raw"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        if let Some(r) = v["metadata"]["raw"].as_object() {
            raw.extend(r.clone());
        }
        out["raw"] = json!(raw);
        out
    }
}

pub(super) fn merge_episode_progress(v: &Value) -> Result {
    Ok({
        let eps = v["episodes"].as_array().ok_or(CoreError::InvalidInput)?;
        let rows = v["history"].as_array().ok_or(CoreError::InvalidInput)?;
        let mut by_id = std::collections::HashMap::new();
        let mut by_coordinate = std::collections::HashMap::new();
        for row in rows {
            by_id.entry(row["id"].to_string()).or_insert(row);
            if row["seriesId"] == v["seriesId"] && !row["episode"].is_null() {
                by_coordinate.entry((row["season"].to_string(), row["episode"].to_string())).or_insert(row);
            }
        }
        Value::Array(
            eps.iter()
                .map(|ep| {
                    let row = by_id.get(&ep["id"].to_string()).copied().or_else(||
                        by_coordinate.get(&(ep["season"].to_string(), ep["episode"].to_string())).copied());
                    let mut out = ep.clone();
                    if let Some(row) = row {
                        for key in [
                            "position",
                            "duration",
                            "watched",
                            "resumeActive",
                            "watchDateKnown",
                            "completionOnly",
                            "sourceAddonId",
                            "sourceName",
                            "sourceFingerprint",
                            "sourceBingeGroup",
                            "sourceReleaseGroup",
                            "sourceQuality",
                            "sourceAudio",
                            "updatedAtMillis",
                        ] {
                            // Only duration may fall back to metadata. A matched history row
                            // replaces activity/source authority even when the fact is unknown.
                            if key != "duration" || !row[key].is_null() {
                                out[key] = row[key].clone();
                            }
                        }
                        out["raw"]["updated_at"] = row["raw"]["updated_at"].clone();
                    }
                    out
                })
                .collect(),
        )
    })
}

pub(super) fn initial_episode(v: &Value) -> Result {
    Ok({
        let mut eps = v["episodes"]
            .as_array()
            .ok_or(CoreError::InvalidInput)?
            .clone();
        eps.sort_by(|a, b| {
            num(a, "season")
                .total_cmp(&num(b, "season"))
                .then(num(a, "episode").total_cmp(&num(b, "episode")))
        });
        let updated = |e: &Value| {
            e["updatedAtMillis"]
                .as_f64()
                .unwrap_or(num(&e["raw"], "updated_at") * 1000.0)
        };
        let latest = eps
            .iter()
            .enumerate()
            .filter(|(_, e)| e["completionOnly"] != true && updated(e) > 0.0)
            .max_by(|(_, a), (_, b)| updated(a).total_cmp(&updated(b)));
        let active = eps
            .iter()
            .filter(|e| {
                e["resumeActive"] == true && e["completionOnly"] != true && updated(e) > 0.0
            })
            .max_by(|a, b| updated(a).total_cmp(&updated(b)));
        if let Some(active) = active {
            active.clone()
        } else if let Some((index, latest)) = latest {
            if watched(latest) {
                eps.iter()
                    .skip(index + 1)
                    .find(|e| {
                        !watched(e)
                            && e["season"] != 0
                            && e["releasedAtMillis"]
                                .as_f64()
                                .or_else(|| {
                                    e["raw"]["released"]
                                        .as_str()
                                        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                                        .map(|d| d.timestamp_millis() as f64)
                                })
                                .unwrap_or(0.0)
                                <= num(v, "now")
                    })
                    .unwrap_or(latest)
                    .clone()
            } else {
                latest.clone()
            }
        } else {
            eps.iter()
                .find(|e| e["completionOnly"] != true && num(e, "position") > 0.0 && !watched(e))
                .or_else(|| {
                    eps.iter().find(|e| {
                        !v["original"]["episode"].is_null()
                            && e["season"] == v["original"]["season"]
                            && e["episode"] == v["original"]["episode"]
                            && !watched(e)
                    })
                })
                .or_else(|| eps.iter().find(|e| !watched(e) && e["season"] != 0))
                .or(eps.first())
                .cloned()
                .unwrap_or(Value::Null)
        }
    })
}

pub(super) fn exact_resume(v: &Value) -> Result {
    Ok(v["sources"]
        .as_array()
        .ok_or(CoreError::InvalidInput)?
        .iter()
        .find(|s| {
            !text(&v["item"], "sourceFingerprint").is_empty()
                && s.get("sourceFingerprint")
                    .unwrap_or(&s["raw"]["source_fingerprint"])
                    == &v["item"]["sourceFingerprint"]
                && s["sourceAddonId"] == v["item"]["sourceAddonId"]
        })
        .cloned()
        .unwrap_or(Value::Null))
}

pub(super) fn enrich_home(v: &Value) -> Value {
    {
        let mut out = v["original"].clone();
        let m = &v["metadata"];
        for k in [
            "poster",
            "background",
            "description",
            "year",
            "runtime",
            "imdbRating",
            "genres",
            "credits",
        ] {
            if !m[k].is_null() && !m[k].as_array().is_some_and(Vec::is_empty) {
                out[k] = m[k].clone();
            }
        }
        let is_episode = num(&out, "episode") > 0.0 || text(&out, "type") == "episode";
        if is_episode {
            let matching = m["episodes"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|episode| {
                    (!text(&out, "id").is_empty() && text(episode, "id") == text(&out, "id"))
                        || (out["season"].is_number()
                            && out["episode"].is_number()
                            && episode["season"] == out["season"]
                            && episode["episode"] == out["episode"])
                });
            if let Some(episode) = matching {
                if !image(episode, "thumbnail").is_null() {
                    out["thumbnail"] = episode["thumbnail"].clone();
                }
                if !text(episode, "episodeTitle").is_empty() {
                    out["episodeTitle"] = episode["episodeTitle"].clone();
                } else if !text(episode, "name").is_empty() {
                    out["episodeTitle"] = episode["name"].clone();
                }
            }
        } else if !image(m, "thumbnail").is_null() {
            out["thumbnail"] = m["thumbnail"].clone();
        }
        if !image(m, "titleLogo").is_null() {
            out["titleLogo"] = m["titleLogo"].clone();
        }
        if text(&out, "name").is_empty() || (m["raw"]["simkl_category"] == "anime" && !text(m, "name").is_empty()) {
            out["name"] = m["name"].clone();
        }
        // A shelf occurrence needs its matched still, not every episode.
        // Detail responses retain the full typed catalog independently.
        out["episodes"] = json!([]);
        out
    }
}

pub(super) fn resume(v: &Value) -> Result {
    Ok(v["sources"]
        .as_array()
        .ok_or(CoreError::InvalidInput)?
        .iter()
        .find(|s| {
            let a = text(s, "sourceAddonId");
            let f = text(s, "sourceFingerprint");
            !a.is_empty()
                && !f.is_empty()
                && format!("{a}\u{0000}{f}") == text(v, "expectedIdentity")
        })
        .map(|s| s["id"].clone())
        .unwrap_or(Value::Null))
}
