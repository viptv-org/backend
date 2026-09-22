// The pure candidate parsing, validation, and ranking logic now lives in the
// shared `viptv-provider` crate; the backend keeps the SQL/service layer.
use super::*;
pub(super) use viptv_provider::candidate::*;

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
                json!({"url":media_url(&provider.url,&provider.username,&provider.password,"movie",&c.stream_id,&c.extension)?,"name":format!("{} · {}",provider.name,c.name),"source":format!("iptv:{}",provider.id)}),
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
            streams.push(json!({"url":media_url(&provider.url,&provider.username,&provider.password,"series",&id,&ext)?,"name":format!("{} · {} S{:02}E{:02}",provider.name,c.name,season,episode),"source":format!("iptv:{}",provider.id)}));
        }
        Ok(streams)
    }
}
