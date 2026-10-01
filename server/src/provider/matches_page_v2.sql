SELECT v.id,v.provider_id,v.kind,v.name,v.year,v.poster
FROM provider_vod v INDEXED BY provider_vod_unmatched_page
CROSS JOIN providers p ON p.id=v.provider_id
WHERE v.provider_id IN (SELECT provider_id FROM provider_ownership WHERE account_id=?1)
AND p.enabled=1 AND v.imdb_id IS NULL AND v.tmdb_id IS NULL
AND ((v.kind='movie' AND p.enable_movies=1) OR (v.kind='series' AND p.enable_series=1))
AND (?2 IS NULL OR p.id=?2) AND (?3 IS NULL OR v.kind=?3)
AND (v.provider_id,v.id)>(?4,?5)
AND NOT EXISTS(SELECT 1 FROM provider_matches m WHERE m.vod_id=v.id AND m.kind=v.kind)
AND (?6='' OR v.rowid IN (SELECT rowid FROM provider_vod_search_v2 WHERE provider_vod_search_v2 MATCH ?6))
ORDER BY v.provider_id,v.id LIMIT ?7
