use super::*;

#[derive(Clone, Debug)]
pub(super) struct Candidate {
    pub(super) id: String,
    pub(super) provider_id: i64,
    pub(super) stream_id: String,
    pub(super) kind: String,
    pub(super) name: String,
    pub(super) normalized: String,
    pub(super) year: Option<i64>,
    pub(super) imdb_id: Option<String>,
    pub(super) tmdb_id: Option<String>,
    pub(super) extension: String,
    pub(super) poster: Option<String>,
    pub(super) override_id: Option<String>,
}

// Both candidate queries select the same leading columns in the same order.
fn candidate_row(
    r: &rusqlite::Row<'_>,
    override_id: Option<String>,
) -> rusqlite::Result<Candidate> {
    Ok(Candidate {
        id: r.get(0)?,
        provider_id: r.get(1)?,
        stream_id: r.get(2)?,
        kind: r.get(3)?,
        name: r.get(4)?,
        normalized: r.get(5)?,
        year: r.get(6)?,
        imdb_id: r.get(7)?,
        tmdb_id: r.get(8)?,
        extension: r.get(9)?,
        poster: r.get(10)?,
        override_id,
    })
}

impl ProviderService {
    pub(super) fn candidates(&self, kind: Option<&str>) -> Result<Vec<Candidate>, String> {
        self.candidates_filtered(kind, None)
    }

    pub(super) fn candidates_filtered(
        &self,
        kind: Option<&str>,
        request: Option<&MatchRequest>,
    ) -> Result<Vec<Candidate>, String> {
        let db = self.lock()?;
        let ids = request.map(|r| json!(r.ids).to_string());
        let title = request.and_then(|r| r.normalized.as_deref());
        let year = request.and_then(|r| r.year);
        // Each UNION arm uses its lookup index, avoiding materializing entire IPTV libraries.
        let mut stmt = db.prepare("SELECT v.id,v.provider_id,v.stream_id,v.kind,v.name,v.normalized,v.year,v.imdb_id,v.tmdb_id,v.extension,v.poster,m.metadata_id
            FROM provider_vod v JOIN providers p ON p.id=v.provider_id
            LEFT JOIN provider_matches m ON m.vod_id=v.id AND m.kind=v.kind
            WHERE p.enabled=1 AND ((v.kind='movie' AND p.enable_movies=1) OR (v.kind='series' AND p.enable_series=1)) AND (?1 IS NULL OR v.kind=?1) AND (?2 IS NULL OR v.id IN (
                SELECT id FROM provider_vod WHERE imdb_id IN (SELECT value FROM json_each(?2))
                UNION SELECT id FROM provider_vod WHERE tmdb_id IN (SELECT value FROM json_each(?2))
                UNION SELECT vod_id FROM provider_matches WHERE metadata_id IN (SELECT value FROM json_each(?2))
                UNION SELECT id FROM provider_vod WHERE kind=?1 AND normalized=?3 AND year=?4
            )) ORDER BY v.provider_id,v.id").map_err(db_error)?;
        let rows = stmt
            .query_map(params![kind, ids, title, year], |r| {
                candidate_row(r, r.get(11)?)
            })
            .map_err(db_error)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)
    }

    pub(super) fn sparse_candidates(
        &self,
        kind: &str,
        request: &MatchRequest,
        only_provider: Option<i64>,
    ) -> Result<Vec<Candidate>, String> {
        let Some(title) = request.normalized.as_deref() else {
            return Ok(Vec::new());
        };
        let db = self.lock()?;
        // Indexed exact title lookup, bounded before materialization. Never use a LIKE scan.
        let mut stmt = db.prepare("SELECT v.id,v.provider_id,v.stream_id,v.kind,v.name,v.normalized,v.year,v.imdb_id,v.tmdb_id,v.extension,v.poster
            FROM provider_vod v JOIN providers p ON p.id=v.provider_id
            WHERE v.kind=?1 AND v.normalized=?2 AND p.enabled=1 AND ((v.kind='movie' AND p.enable_movies=1) OR (v.kind='series' AND p.enable_series=1))
            AND (v.year IS NULL OR (?3 IS NULL AND (v.imdb_id IS NULL OR v.tmdb_id IS NULL)))
            AND NOT EXISTS(SELECT 1 FROM provider_matches m WHERE m.vod_id=v.id)
            AND (?5 IS NULL OR v.provider_id=?5)
            LIMIT ?4").map_err(db_error)?;
        let rows = stmt
            .query_map(
                params![
                    kind,
                    title,
                    request.year,
                    MAX_LAZY_DETAILS as i64,
                    only_provider
                ],
                |r| candidate_row(r, None),
            )
            .map_err(db_error)?;
        Ok(rows
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db_error)?
            .into_iter()
            .filter(|c| !evidence_conflicts(c, request))
            .collect())
    }

    pub(super) async fn enrich_candidate(&self, c: Candidate) -> Result<Option<Candidate>, String> {
        let provider_id = c.provider_id;
        let kind = c.kind.clone();
        let provider = self
            .blocking(move |s| s.provider_for_kind(provider_id, &kind))
            .await?;
        let action = if c.kind == "movie" {
            "get_vod_info"
        } else {
            "get_series_info"
        };
        let details = self.cached_api(&provider, action, &c.stream_id).await?;
        let Some(enriched) = validated_details(&c, &details) else {
            return Ok(None);
        };
        self.blocking(move |s| {
            // Compare-and-set: do not overwrite a concurrent sync, edit, or manual mapping.
            let changed = s
                .lock()?
                .execute(
                    "UPDATE provider_vod SET year=?1,imdb_id=?2,tmdb_id=?3
                WHERE id=?4 AND normalized=?5 AND year IS ?6 AND imdb_id IS ?7 AND tmdb_id IS ?8
                AND EXISTS(SELECT 1 FROM providers p WHERE p.id=provider_id AND p.enabled=1 AND ((provider_vod.kind='movie' AND p.enable_movies=1) OR (provider_vod.kind='series' AND p.enable_series=1)))
                AND NOT EXISTS(SELECT 1 FROM provider_matches m WHERE m.vod_id=provider_vod.id)",
                    params![
                        enriched.year,
                        enriched.imdb_id,
                        enriched.tmdb_id,
                        c.id,
                        c.normalized,
                        c.year,
                        c.imdb_id,
                        c.tmdb_id
                    ],
                )
                .map_err(db_error)?;
            Ok((changed == 1).then_some(enriched))
        })
        .await
    }

    pub fn matches(&self) -> Result<Value, String> {
        Ok(Value::Array(self.candidates(None)?.into_iter()
            .filter(|c| c.override_id.is_none() && c.imdb_id.is_none() && c.tmdb_id.is_none())
            .map(|c| json!({"vod_id":c.id,"provider_id":c.provider_id,"type":c.kind,"name":c.name,"year":c.year,"poster":c.poster})).collect()))
    }

    pub fn override_match(&self, value: Value) -> Result<(), String> {
        let vod_id = required_string(&value, "vod_id", 256)?;
        let metadata_id = required_string(&value, "metadata_id", 256)?;
        if metadata_id.chars().any(char::is_whitespace) {
            return Err("Invalid metadata ID".into());
        }
        let kind = required_string(&value, "type", 16)?;
        if kind != "movie" && kind != "series" {
            return Err("Match type must be movie or series".into());
        }
        let db = self.lock()?;
        let valid: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM provider_vod WHERE id=?1 AND kind=?2)",
                params![vod_id, kind],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        if !valid {
            return Err("Stream candidate not found for this type".into());
        }
        db.execute(
            "INSERT INTO provider_matches(vod_id,metadata_id,kind) VALUES(?1,?2,?3)
            ON CONFLICT(vod_id) DO UPDATE SET metadata_id=excluded.metadata_id,kind=excluded.kind",
            params![vod_id, canonical_id(&metadata_id), kind],
        )
        .map_err(db_error)?;
        Ok(())
    }

    pub(super) async fn resolve_candidate(
        &self,
        c: Candidate,
        season: i64,
        episode: i64,
    ) -> Result<Vec<Value>, String> {
        let provider_id = c.provider_id;
        let kind = c.kind.clone();
        let provider = self
            .blocking(move |s| s.provider_for_kind(provider_id, &kind))
            .await?;
        if c.kind == "movie" {
            return Ok(vec![
                json!({"url":media_url(&provider,"movie",&c.stream_id,&c.extension)?,"name":format!("{} · {}",provider.name,c.name),"source":format!("iptv:{}",provider.id)}),
            ]);
        }
        let info = self
            .cached_api(&provider, "get_series_info", &c.stream_id)
            .await?;
        let mut streams = Vec::new();
        for item in episode_rows(&info, season, episode).into_iter().take(100) {
            let Some(id) = stream_id(item, "id") else {
                continue;
            };
            let ext = episode_extension(item.get("container_extension"));
            streams.push(json!({"url":media_url(&provider,"series",&id,&ext)?,"name":format!("{} · {} S{:02}E{:02}",provider.name,c.name,season,episode),"source":format!("iptv:{}",provider.id)}));
        }
        Ok(streams)
    }
}

pub(super) fn candidate_from_json(
    provider_id: i64,
    kind: &str,
    value: &Value,
) -> Option<Candidate> {
    let stream_id = stream_id(
        value,
        if kind == "series" {
            "series_id"
        } else {
            "stream_id"
        },
    )?;
    let name = value.get("name")?.as_str()?.trim();
    let (title, suffix_year) = title_year(name);
    // Keep the provider title (not the display fallback) as the matching input.
    let name = if name.is_empty() {
        format!("Untitled {kind} #{stream_id}")
    } else {
        name.to_owned()
    };
    let year = ["year", "releaseDate", "release_date"]
        .iter()
        .find_map(|k| value.get(*k).and_then(valid_year))
        .or(suffix_year);
    Some(Candidate {
        id: format!("iptv:{provider_id}:{kind}:{stream_id}"),
        provider_id,
        stream_id,
        kind: kind.into(),
        name,
        normalized: normalize(&title),
        year,
        imdb_id: imdb_id(value.get("imdb_id")).or_else(|| imdb_id(value.get("imdb"))),
        tmdb_id: tmdb_id(value.get("tmdb_id")).or_else(|| tmdb_id(value.get("tmdb"))),
        extension: extension(value.get("container_extension").and_then(Value::as_str)),
        poster: text(value, "stream_icon").or_else(|| text(value, "cover")),
        override_id: None,
    })
}

// Lazy discovery must not use an ID match to erase contradictory year/namespace evidence.
pub(super) fn evidence_conflicts(c: &Candidate, r: &MatchRequest) -> bool {
    c.year.zip(r.year).is_some_and(|(a, b)| a != b)
        || [(&c.imdb_id, "tt"), (&c.tmdb_id, "tmdb:")]
            .iter()
            .any(|(id, prefix)| {
                id.as_ref().is_some_and(|id| {
                    r.ids.iter().any(|v| v.starts_with(prefix)) && !r.ids.contains(id)
                })
            })
}

pub(super) fn validated_details(c: &Candidate, details: &Value) -> Option<Candidate> {
    let info = details.get("info")?.as_object()?;
    if details.get("movie_data").is_some_and(|v| !v.is_object()) {
        return None;
    }
    let mut enriched = c.clone();
    let mut named = false;
    for object in [
        Some(info),
        details.get("movie_data").and_then(Value::as_object),
    ]
    .into_iter()
    .flatten()
    {
        for key in ["stream_id", "vod_id", "series_id"] {
            if let Some(value) = object.get(key) {
                if scalar(value).as_deref() != Some(c.stream_id.as_str()) {
                    return None;
                }
            }
        }
        for key in ["name", "title"] {
            if let Some(value) = object.get(key).filter(|v| !v.is_null()) {
                let name = value.as_str()?.trim();
                if name.is_empty() {
                    continue;
                }
                let (title, year) = title_year(name);
                if normalize(&title) != c.normalized || c.normalized.is_empty() {
                    return None;
                }
                named = true;
                if let Some(year) = year {
                    if enriched.year.is_some_and(|old| old != year) {
                        return None;
                    }
                    enriched.year = Some(year);
                }
            }
        }
        for key in ["year", "releasedate", "releaseDate", "release_date"] {
            if let Some(value) = object
                .get(key)
                .filter(|v| !v.is_null() && v.as_str() != Some(""))
            {
                let raw = scalar(value)?;
                if raw.len() != 4 {
                    let bytes = raw.as_bytes();
                    if bytes.len() != 10
                        || bytes[4] != b'-'
                        || bytes[7] != b'-'
                        || !bytes
                            .iter()
                            .enumerate()
                            .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
                        || !(1..=12).contains(&raw[5..7].parse::<u32>().ok()?)
                        || !(1..=31).contains(&raw[8..10].parse::<u32>().ok()?)
                    {
                        return None;
                    }
                }
                let year = valid_year(value)?;
                if enriched.year.is_some_and(|old| old != year) {
                    return None;
                }
                enriched.year = Some(year);
            }
        }
        for (keys, target, parse) in [
            (
                ["imdb_id", "imdb"],
                &mut enriched.imdb_id,
                imdb_id as fn(Option<&Value>) -> Option<String>,
            ),
            (
                ["tmdb_id", "tmdb"],
                &mut enriched.tmdb_id,
                tmdb_id as fn(Option<&Value>) -> Option<String>,
            ),
        ] {
            for key in keys {
                if let Some(value) = object
                    .get(key)
                    .filter(|v| !v.is_null() && v.as_str() != Some(""))
                {
                    let id = parse(Some(value))?;
                    if target.as_ref().is_some_and(|old| old != &id) {
                        return None;
                    }
                    *target = Some(id);
                }
            }
        }
    }
    named.then_some(enriched)
}

#[derive(Clone)]
pub(super) struct MatchRequest {
    pub(super) ids: HashSet<String>,
    pub(super) normalized: Option<String>,
    pub(super) year: Option<i64>,
    pub(super) season: Option<i64>,
    pub(super) episode: Option<i64>,
}
impl MatchRequest {
    pub(super) fn parse(value: &Value, kind: &str) -> Result<Self, String> {
        let mut id = required_string(value, "id", 256)?;
        let number = |key: &str| -> Result<Option<i64>, String> {
            match value.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(v) => timestamp(v)
                    .map(Some)
                    .ok_or_else(|| format!("Invalid {key}")),
            }
        };
        let mut season = number("season")?;
        let mut episode = number("episode")?;
        if kind == "series" {
            // Strip only two numeric suffixes, preserving namespaced IDs such as tmdb:123.
            let pieces: Vec<&str> = id.rsplitn(3, ':').collect();
            if pieces.len() == 3 {
                if let (Ok(e), Ok(s)) = (pieces[0].parse::<i64>(), pieces[1].parse::<i64>()) {
                    if s < 0 || e < 0 {
                        return Err("Invalid season or episode".into());
                    }
                    if season.is_some_and(|v| v != s) || episode.is_some_and(|v| v != e) {
                        return Err("Episode ID conflicts with season or episode".into());
                    }
                    season = Some(s);
                    episode = Some(e);
                    id = pieces[2].to_owned();
                }
            }
        }
        let mut ids = HashSet::from([canonical_id(&id)]);
        if let Some(id) = imdb_id(value.get("imdb_id")) {
            ids.insert(id);
        }
        if let Some(id) = tmdb_id(value.get("tmdb_id")) {
            ids.insert(id);
        }
        let title = text(value, "name").map(|s| title_year(&s));
        let year = value
            .get("year")
            .and_then(valid_year)
            .or_else(|| title.as_ref().and_then(|t| t.1));
        let normalized = title.map(|t| normalize(&t.0)).filter(|t| !t.is_empty());
        Ok(Self {
            ids,
            normalized,
            year,
            season,
            episode,
        })
    }
}

pub(super) fn select_candidates<'a>(
    candidates: &'a [Candidate],
    request: &MatchRequest,
) -> Vec<&'a Candidate> {
    candidates
        .iter()
        .filter(|c| {
            // A manual mapping is authoritative, including when it rules a candidate out.
            if let Some(id) = &c.override_id {
                return request.ids.contains(id);
            }
            if c.imdb_id
                .as_ref()
                .is_some_and(|id| request.ids.contains(id))
                || c.tmdb_id
                    .as_ref()
                    .is_some_and(|id| request.ids.contains(id))
            {
                return true;
            }
            // Do not override contradictory IDs from the same metadata namespace with a title.
            if c.imdb_id.is_some() && request.ids.iter().any(|id| id.starts_with("tt")) {
                return false;
            }
            if c.tmdb_id.is_some() && request.ids.iter().any(|id| id.starts_with("tmdb:")) {
                return false;
            }
            request.year.is_some()
                && request.year == c.year
                && request
                    .normalized
                    .as_ref()
                    .is_some_and(|n| n == &c.normalized)
        })
        .collect()
}

pub(super) fn episode_rows(info: &Value, season: i64, episode: i64) -> Vec<&Value> {
    let Some(episodes) = info.get("episodes") else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    if let Some(seasons) = episodes.as_object() {
        if let Some(items) = seasons.get(&season.to_string()).and_then(Value::as_array) {
            rows.extend(items.iter().filter(|v| {
                v.get("episode_num").and_then(timestamp) == Some(episode)
                    && v.get("season").is_none_or(|s| timestamp(s) == Some(season))
            }));
        }
    } else if let Some(items) = episodes.as_array() {
        rows.extend(items.iter().filter(|v| {
            v.get("season").and_then(timestamp) == Some(season)
                && v.get("episode_num").and_then(timestamp) == Some(episode)
        }));
    }
    rows
}
